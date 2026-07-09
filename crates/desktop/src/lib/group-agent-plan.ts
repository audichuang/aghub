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
