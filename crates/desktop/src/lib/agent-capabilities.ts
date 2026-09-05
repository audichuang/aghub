import type {
	AgentInfo,
	AgentSkillCoverageDto,
	TransportDto,
} from "../generated/dto";

export type AgentScope = "global" | "project";

export function supportsMcp(agent: Pick<AgentInfo, "capabilities">): boolean {
	return (
		agent.capabilities.mcp.scopes.global ||
		agent.capabilities.mcp.scopes.project
	);
}

export function supportsMcpScope(
	agent: Pick<AgentInfo, "capabilities">,
	scope: AgentScope,
): boolean {
	return agent.capabilities.mcp.scopes[scope];
}

export function supportsMcpTransport(
	agent: Pick<AgentInfo, "capabilities">,
	transport: TransportDto | undefined,
): boolean {
	if (!transport) return false;
	if (transport.type === "stdio") return agent.capabilities.mcp.stdio;
	return agent.capabilities.mcp.remote;
}

export function supportsSkill(agent: Pick<AgentInfo, "capabilities">): boolean {
	return (
		agent.capabilities.skills.scopes.global ||
		agent.capabilities.skills.scopes.project
	);
}

export function supportsSkillScope(
	agent: Pick<AgentInfo, "capabilities">,
	scope: AgentScope,
): boolean {
	return agent.capabilities.skills.scopes[scope];
}

export function supportsSkillMutation(
	agent: Pick<AgentInfo, "capabilities">,
	scope: AgentScope,
): boolean {
	return scope === "global"
		? agent.capabilities.skills.mutable_global
		: agent.capabilities.skills.mutable_project;
}

export function supportsSubAgent(
	agent: Pick<AgentInfo, "capabilities">,
): boolean {
	return (
		agent.capabilities.sub_agents.scopes.global ||
		agent.capabilities.sub_agents.scopes.project
	);
}

export function supportsSubAgentScope(
	agent: Pick<AgentInfo, "capabilities">,
	scope: AgentScope,
): boolean {
	return agent.capabilities.sub_agents.scopes[scope];
}

export function needsMasterLink(
	cov: AgentSkillCoverageDto | undefined,
): boolean {
	return cov?.needs_link ?? false;
}

/**
 * Agents that receive a skill through the SAME directory as `id`.
 *
 * Several agents provide no skills directory of their own and read the shared
 * `.agents/skills` instead — up to eight at project scope. For those, a grant is
 * not per-agent: writing that one directory hands the skill to every reader of
 * it, and removing it takes the skill from all of them at once.
 */
export function sharedWith(cov: AgentSkillCoverageDto | undefined): string[] {
	return cov?.shared_with ?? [];
}

/**
 * A set of agents that must be selected and deselected together.
 *
 * `members.length === 1` is an ordinary agent with a private directory; anything
 * larger is a shared slot, and the UI must not offer its members as independent
 * checkboxes — doing so promises a per-agent choice the filesystem cannot keep.
 */
export interface AgentSelectionGroup<A> {
	members: A[];
	shared: boolean;
}

/**
 * Group installable agents into independently selectable units.
 *
 * This replaces the old `{ autoCovered, linkTargets }` split. `autoCovered` meant
 * "this agent reads the shared master directly, so it is covered whether you
 * asked for it or not" — the leak, rendered as a feature, in a read-only chip the
 * user could not uncheck. There is no such agent any more: nothing reads the
 * store, so every supported agent takes a link and every agent is selectable.
 *
 * What survived is narrower and real: some agents share one directory. They are
 * returned as one group so the UI can bind their checkbox together instead of
 * lying about the granularity.
 */
export function groupAgentsBySlot<A extends { id: string }>(
	installable: A[],
	coverage: Record<string, AgentSkillCoverageDto>,
): AgentSelectionGroup<A>[] {
	const linkable = installable.filter((a) => needsMasterLink(coverage[a.id]));
	const byId = new Map(linkable.map((a) => [a.id, a]));
	const groups: AgentSelectionGroup<A>[] = [];
	const claimed = new Set<string>();

	for (const agent of linkable) {
		if (claimed.has(agent.id)) continue;
		// Only peers we can actually render belong in the group: an agent the
		// server reports as a sharer but that this list does not contain (not
		// installed, filtered out) must not become a phantom checkbox.
		const peers = sharedWith(coverage[agent.id])
			.filter((id) => byId.has(id) && !claimed.has(id))
			.map((id) => byId.get(id) as A);
		const members = [agent, ...peers];
		for (const member of members) claimed.add(member.id);
		groups.push({ members, shared: members.length > 1 });
	}
	return groups;
}

/**
 * The agents a selection must expand to before it is sent.
 *
 * Selecting one member of a shared slot selects the whole slot, because the
 * write is one directory. Callers submit THIS, never the raw checkbox state —
 * otherwise the request names one agent while the disk grants several, and the
 * result rows disagree with what the user chose.
 */
export function expandSelection(
	selected: string[],
	coverage: Record<string, AgentSkillCoverageDto>,
	installableIds: string[],
): string[] {
	const available = new Set(installableIds);
	const out = new Set<string>();
	for (const id of selected) {
		out.add(id);
		for (const peer of sharedWith(coverage[id])) {
			if (available.has(peer)) out.add(peer);
		}
	}
	return [...out];
}
