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
	globalSkillLockQueryOptions,
	reconcileSkillsMutationOptions,
	skillListQueryOptions,
} from "../requests/skills";
import { NewToolsModal } from "./new-tools-modal";

export function NewToolsPromptController() {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const { availableAgents, disabledAgentsLoaded } = useAgentAvailability();
	const { coverage, isLoading: coverageLoading } = useSkillCoverage("global");
	const { data: skills = [], isLoading: skillsLoading } = useQuery(
		skillListQueryOptions({ api, scope: "global" }),
	);
	// Only lock-owned skills are aghub's to move. Discovery also lists private
	// hand-made skills, and reconciling one would promote it into the shared
	// Master (see `reconcileAddsForNewAgents`).
	const { data: globalLock, isLoading: lockLoading } = useQuery(
		globalSkillLockQueryOptions({ api }),
	);
	const lockedNames = useMemo(
		() => new Set((globalLock?.skills ?? []).map((entry) => entry.name)),
		[globalLock],
	);
	const { data: lastKnown, isLoading: lastKnownLoading } = useQuery({
		queryKey: ["lastKnownAvailableAgents"],
		queryFn: getLastKnownAvailableAgents,
	});
	const reconcileMutation = useMutation(
		reconcileSkillsMutationOptions({ api, queryClient }),
	);
	const [dismissed, setDismissed] = useState(false);
	/** Last seed written this session, so an agent that DISAPPEARS is dropped
	 * from `lastKnown` even without an app restart — otherwise an
	 * uninstall→reinstall inside one session would never prompt again. */
	const persistedSeedRef = useRef<string | null>(null);

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
		if (
			coverageLoading ||
			skillsLoading ||
			lockLoading ||
			lastKnownLoading ||
			!disabledAgentsLoaded
		) {
			// `disabledAgents` is read from the store AFTER the provider first
			// renders, so every agent looks enabled for one commit. Seeding on
			// that frame would record a disabled agent as "known" and never
			// prompt for it once the user enables it.
			return null;
		}
		return newToolPromptDelta({
			lastKnown: lastKnown ?? null,
			agents: promptAgents,
		});
	}, [
		coverageLoading,
		disabledAgentsLoaded,
		lastKnown,
		lastKnownLoading,
		lockLoading,
		promptAgents,
		skillsLoading,
	]);

	useEffect(() => {
		if (!delta) return;
		if (delta.kind !== "seedOnly" && delta.kind !== "quiet") return;
		const fingerprint = delta.seed.join("\u0000");
		if (persistedSeedRef.current === fingerprint) return;
		persistedSeedRef.current = fingerprint;
		void setLastKnownAvailableAgents(delta.seed).then(() => {
			queryClient.setQueryData(["lastKnownAvailableAgents"], delta.seed);
		});
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
		const plans = reconcileAddsForNewAgents(skills, promptIds, lockedNames);
		let failed = 0;
		for (const plan of plans) {
			try {
				const result = await reconcileMutation.mutateAsync({
					source: {
						agent: plan.sourceAgent,
						scope: "global",
						project_root: null,
						name: plan.name,
					},
					added: plan.added,
					removed: null,
				});
				// A batch answers HTTP 200 with per-row failures inside the
				// envelope, so a resolved promise is NOT success. Counting only
				// thrown requests reported "linked!" while agents were skipped.
				if (result.failed_count > 0) failed += 1;
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
