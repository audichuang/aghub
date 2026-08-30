import { toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { useApi } from "../hooks/use-api";
import {
	needsMasterLink,
	supportsSkillMutation,
} from "../lib/agent-capabilities";
import {
	newToolPromptDelta,
	reconcileAddsForNewAgents,
	type NewToolPromptAgent,
} from "../lib/new-tool-prompt";
import {
	getLastKnownAvailableAgents,
	setLastKnownAvailableAgents,
} from "../lib/store";
import { useSkillCoverage } from "../requests/agents";
import {
	reconcileSkillsMutationOptions,
	skillListQueryOptions,
} from "../requests/skills";
import { NewToolsModal } from "./new-tools-modal";

export function NewToolsPromptController() {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const { availableAgents } = useAgentAvailability();
	const { coverage, isLoading: coverageLoading } = useSkillCoverage("global");
	const { data: skills = [], isLoading: skillsLoading } = useQuery(
		skillListQueryOptions({ api, scope: "global" }),
	);
	const { data: lastKnown, isLoading: lastKnownLoading } = useQuery({
		queryKey: ["lastKnownAvailableAgents"],
		queryFn: getLastKnownAvailableAgents,
	});
	const reconcileMutation = useMutation(
		reconcileSkillsMutationOptions({ api, queryClient }),
	);
	const [dismissed, setDismissed] = useState(false);
	const persistedRef = useRef(false);

	const promptAgents: NewToolPromptAgent[] = useMemo(
		() =>
			availableAgents.map((agent) => ({
				id: agent.id,
				isAvailable: agent.availability.is_available,
				isDisabled: agent.isDisabled,
				skillMutableGlobal: supportsSkillMutation(agent, "global"),
				needsLink: needsMasterLink(coverage[agent.id]),
			})),
		[availableAgents, coverage],
	);

	const delta = useMemo(() => {
		if (coverageLoading || skillsLoading || lastKnownLoading) return null;
		return newToolPromptDelta({
			lastKnown: lastKnown ?? null,
			agents: promptAgents,
		});
	}, [
		coverageLoading,
		lastKnown,
		lastKnownLoading,
		promptAgents,
		skillsLoading,
	]);

	useEffect(() => {
		if (!delta || persistedRef.current) return;
		if (delta.kind === "seedOnly" || delta.kind === "quiet") {
			persistedRef.current = true;
			void setLastKnownAvailableAgents(delta.seed).then(() => {
				queryClient.setQueryData(
					["lastKnownAvailableAgents"],
					delta.seed,
				);
			});
		}
	}, [delta, queryClient]);

	const persistSeed = async (seed: string[]) => {
		await setLastKnownAvailableAgents(seed);
		queryClient.setQueryData(["lastKnownAvailableAgents"], seed);
		setDismissed(true);
	};

	const promptIds = !dismissed && delta?.kind === "prompt" ? delta.ids : null;
	const pendingSeed = delta?.kind === "prompt" ? delta.seed : null;

	const handleSkip = () => {
		if (!pendingSeed) {
			setDismissed(true);
			return;
		}
		void persistSeed(pendingSeed);
	};

	const handleLink = async () => {
		if (!promptIds || !pendingSeed) return;
		const plans = reconcileAddsForNewAgents(skills, promptIds);
		let failed = 0;
		for (const plan of plans) {
			try {
				await reconcileMutation.mutateAsync({
					source: {
						agent: plan.sourceAgent,
						scope: "global",
						project_root: null,
						name: plan.name,
					},
					added: plan.added,
					removed: null,
				});
			} catch {
				failed += 1;
			}
		}
		if (failed === 0) {
			toast.success(t("newToolsLinkSuccess"));
		} else if (failed < plans.length) {
			toast.danger(
				t("newToolsLinkPartial", { failed, total: plans.length }),
			);
		} else if (plans.length > 0) {
			toast.danger(t("newToolsLinkError"));
		}
		await persistSeed(pendingSeed);
	};

	const agentLabels = (promptIds ?? []).map((id) => {
		const match = availableAgents.find((agent) => agent.id === id);
		return match?.display_name ?? id;
	});

	return (
		<NewToolsModal
			isOpen={promptIds !== null}
			agentLabels={agentLabels}
			skillCount={new Set(skills.map((skill) => skill.name)).size}
			isLinking={reconcileMutation.isPending}
			onSkip={handleSkip}
			onLink={() => {
				void handleLink();
			}}
		/>
	);
}
