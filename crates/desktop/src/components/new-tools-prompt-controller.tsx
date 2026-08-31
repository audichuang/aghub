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
	const { availableAgents, disabledAgentsLoaded, agentsReady } =
		useAgentAvailability();
	const { coverage, isSuccess: coverageReady } = useSkillCoverage("global");
	const { data: skills = [], isSuccess: skillsReady } = useQuery(
		skillListQueryOptions({ api, scope: "global" }),
	);
	// Only lock-owned skills are aghub's to move. Discovery also lists private
	// hand-made skills, and reconciling one would promote it into the shared
	// Master (see `reconcileAddsForNewAgents`).
	const { data: globalLock, isSuccess: lockReady } = useQuery(
		globalSkillLockQueryOptions({ api }),
	);
	const lockedNames = useMemo(
		() => new Set((globalLock?.skills ?? []).map((entry) => entry.name)),
		[globalLock],
	);
	const { data: lastKnown, isSuccess: lastKnownReady } = useQuery({
		queryKey: ["lastKnownAvailableAgents"],
		queryFn: getLastKnownAvailableAgents,
	});
	const reconcileMutation = useMutation(
		reconcileSkillsMutationOptions({ api, queryClient }),
	);
	/** The delta already handled. Keyed by its ids, not a bare boolean: a plain
	 * "dismissed" flag silenced every LATER prompt in the same session, so a
	 * second newly-installed agent could not be offered until an app restart. */
	const [handledIds, setHandledIds] = useState<string | null>(null);
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
		// Every input must have SUCCEEDED, not merely stopped loading. A failed
		// lock query would otherwise read as an empty lock: Confirm would make
		// zero plans, claim success, and still advance `lastKnown` — silently
		// burning the one chance to ask about this agent.
		// EVERY input must have succeeded. A failed coverage or availability
		// query yields empty data that reads as "no agent needs a link", and
		// persisting THAT seeds an empty `lastKnown` — so when the query later
		// recovers, every pre-existing agent looks newly installed and the user
		// gets the upgrade-time prompt spam locked decision D4 forbids.
		if (
			!coverageReady ||
			!agentsReady ||
			!skillsReady ||
			!lockReady ||
			!lastKnownReady
		) {
			return null;
		}
		if (!disabledAgentsLoaded) {
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
		agentsReady,
		coverageReady,
		disabledAgentsLoaded,
		lastKnown,
		lastKnownReady,
		lockReady,
		promptAgents,
		skillsReady,
	]);

	useEffect(() => {
		if (!delta) return;
		if (delta.kind !== "seedOnly" && delta.kind !== "quiet") return;
		const fingerprint = delta.seed.join("\u0000");
		if (persistedSeedRef.current === fingerprint) return;
		let cancelled = false;
		void setLastKnownAvailableAgents(delta.seed)
			.then(() => {
				if (cancelled) return;
				// Recorded only AFTER the store accepted it: a rejected write
				// must stay retryable this session, not be remembered as done.
				persistedSeedRef.current = fingerprint;
				queryClient.setQueryData(
					["lastKnownAvailableAgents"],
					delta.seed,
				);
			})
			.catch(() => {
				// Nothing to show the user: the next render retries, and until
				// it lands the prompt simply stays un-suppressed.
			});
		return () => {
			cancelled = true;
		};
	}, [delta, queryClient]);

	const deltaIds = delta?.kind === "prompt" ? delta.ids.join("\u0000") : null;

	// Forget the handled delta as soon as we are no longer prompting. Without
	// this, handling agent A, uninstalling it (which removes it from
	// `lastKnown`), then REINSTALLING it produces the identical id list and the
	// modal would never reopen — the documented reinstall case.
	if (deltaIds === null && handledIds !== null) setHandledIds(null);

	const persistSeed = async (seed: string[]) => {
		setHandledIds(deltaIds);
		try {
			await setLastKnownAvailableAgents(seed);
			queryClient.setQueryData(["lastKnownAvailableAgents"], seed);
		} catch {
			// The store refused: re-open rather than silently swallowing it,
			// otherwise the agent is never offered again this session.
			setHandledIds(null);
			toast.danger(t("newToolsLinkError"));
		}
	};

	const promptIds =
		delta?.kind === "prompt" && handledIds !== deltaIds ? delta.ids : null;
	const pendingSeed = delta?.kind === "prompt" ? delta.seed : null;

	const handleSkip = () => {
		if (!pendingSeed) {
			setHandledIds(deltaIds);
			return;
		}
		void persistSeed(pendingSeed);
	};

	const handleLink = async () => {
		if (!promptIds || !pendingSeed) return;
		const plans = reconcileAddsForNewAgents(skills, promptIds, lockedNames);
		// Count AGENT LINKS, not plans: one skill reconciled onto two agents
		// where only one succeeds is a partial failure, and per-plan counting
		// would call the whole plan failed.
		let attempted = 0;
		let failed = 0;
		for (const plan of plans) {
			attempted += plan.added.length;
			try {
				// A batch answers HTTP 200 with per-row failures INSIDE the
				// envelope, so a resolved promise is not success on its own.
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
				failed += result.failed_count;
			} catch {
				failed += plan.added.length;
			}
		}
		if (attempted === 0) {
			// Locked skills the discovery list does not show yet: nothing was
			// reconciled, so claiming success would be a lie.
			toast.danger(t("newToolsLinkError"));
		} else if (failed === 0) {
			toast.success(t("newToolsLinkSuccess"));
		} else if (failed < attempted) {
			toast.danger(
				t("newToolsLinkPartial", { failed, total: attempted }),
			);
		} else if (attempted > 0) {
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
			// What will ACTUALLY be linked: lock-owned skills only. Counting the
			// discovery list would promise to move private skills we now skip.
			skillCount={lockedNames.size}
			isLinking={reconcileMutation.isPending}
			onSkip={handleSkip}
			onLink={() => {
				void handleLink();
			}}
		/>
	);
}
