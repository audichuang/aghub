export type TriState = "all" | "some" | "none";

export interface GroupAgentStat {
	agentId: string;
	installed: number;
	total: number;
	state: TriState;
}

export interface SkillReconcilePlan {
	name: string;
	sourceAgent: string;
	scope: "global" | "project";
	added: string[];
	removed: string[];
}

export function computeGroupAgentStats(
	skills: { name: string; items: { agent: string }[] }[],
	usableAgentIds: string[],
): GroupAgentStat[] {
	const total = skills.length;
	return usableAgentIds.map((agentId) => {
		const installed = skills.filter((s) =>
			s.items.some((it) => it.agent === agentId),
		).length;
		const state: TriState =
			installed === 0 ? "none" : installed === total ? "all" : "some";
		return { agentId, installed, total, state };
	});
}

export type AgentDiffLabel = "adding" | "removing" | "installed";

export interface SkillAgentDiff {
	selected: string[];
	added: string[];
	removed: string[];
	labels: Record<string, AgentDiffLabel>;
}

// Compute the add/remove diff for managing ONE skill's agents. `desired` is the
// "touched" overlay (agent id -> checked); untouched agents fall back to their
// installed state. Everything is scoped to `usableAgentIds`, so a removal never
// targets an agent the UI can't mutate, and `added`/`removed` are always
// disjoint (a prerequisite the reconcile API enforces).
export function computeSkillAgentDiff(
	usableAgentIds: string[],
	installedAgentIds: Set<string>,
	desired: Record<string, boolean>,
): SkillAgentDiff {
	const selected = usableAgentIds.filter((id) =>
		id in desired ? desired[id] : installedAgentIds.has(id),
	);
	const selectedSet = new Set(selected);
	const added = selected.filter((id) => !installedAgentIds.has(id));
	const removed = usableAgentIds.filter(
		(id) => installedAgentIds.has(id) && !selectedSet.has(id),
	);
	const labels: Record<string, AgentDiffLabel> = {};
	for (const id of usableAgentIds) {
		const isInstalled = installedAgentIds.has(id);
		const isSelected = selectedSet.has(id);
		if (isSelected && !isInstalled) labels[id] = "adding";
		else if (!isSelected && isInstalled) labels[id] = "removing";
		else if (isSelected && isInstalled) labels[id] = "installed";
	}
	return { selected, added, removed, labels };
}

// True when applying `added`/`removed` would leave the skill installed for NO
// agent while still depending on a fresh copy to a new agent.
//
// This is a UX hint, NOT the data-safety guard. Core owns that now: it skips
// every removal when a copy fails, and it refuses — before writing anything —
// any reconcile whose end state cannot exist. Keeping the check here only saves
// a round-trip and shows a friendlier message than the backend error.
//
// Do NOT copy this into other dialogs: they are already covered by core's
// refusal, and a second copy of the rule is a second copy to keep in sync.
export function wouldOrphanSkill(
	installedAgentIds: Set<string>,
	added: string[],
	removed: string[],
): boolean {
	if (added.length === 0) return false;
	const removedSet = new Set(removed);
	const hasSurvivingInstall = [...installedAgentIds].some(
		(id) => !removedSet.has(id),
	);
	return !hasSurvivingInstall;
}

export function buildReconcilePlans(
	skills: { name: string; items: { agent: string; source: string }[] }[],
	usableAgentIds: string[],
	desired: Set<string>,
): SkillReconcilePlan[] {
	const plans: SkillReconcilePlan[] = [];
	for (const skill of skills) {
		const installedAgents = new Set(skill.items.map((it) => it.agent));
		const added = usableAgentIds.filter(
			(id) => desired.has(id) && !installedAgents.has(id),
		);
		const removed = usableAgentIds.filter(
			(id) => !desired.has(id) && installedAgents.has(id),
		);
		if (added.length === 0 && removed.length === 0) continue;
		// reconcile needs an existing install as the source (its agent/scope);
		// use the first item.
		const primary = skill.items[0];
		plans.push({
			name: skill.name,
			sourceAgent: primary?.agent ?? "claude",
			scope: primary?.source === "project" ? "project" : "global",
			added,
			removed,
		});
	}
	return plans;
}
