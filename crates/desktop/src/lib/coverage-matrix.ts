// Pure model for the agent-coverage overview grid (pages/settings/coverage.tsx).
// A row is one resource (skill or mcp server); columns are the unified set of
// usable agents. A cell records whether the resource is installed on that agent
// AND whether that agent can carry this kind of resource at all (applicable) —
// so skill and mcp sections can share one column axis while greying out the
// cells an agent cannot own.

export interface CoverageResource {
	name: string;
	/** Distinct agent ids that currently carry this resource. */
	installedAgents: string[];
}

export interface CoverageCell {
	agentId: string;
	/** The agent supports this resource kind at this scope. */
	applicable: boolean;
	installed: boolean;
}

export interface CoverageRow {
	name: string;
	installedAgents: Set<string>;
	cells: CoverageCell[];
}

// Collapse per-(name, agent) resource rows into one entry per name with the set
// of agents that carry it. Installs on agents outside `usableAgentIds` are
// dropped so a stray entry on a now-unusable agent never widens the matrix, but
// the resource itself still appears (its name key is always kept).
export function groupResourcesByName(
	items: { name: string; agent?: string | null }[],
	usableAgentIds: string[],
): CoverageResource[] {
	const usable = new Set(usableAgentIds);
	const byName = new Map<string, Set<string>>();
	for (const it of items) {
		let agents = byName.get(it.name);
		if (!agents) {
			agents = new Set<string>();
			byName.set(it.name, agents);
		}
		if (it.agent && usable.has(it.agent)) agents.add(it.agent);
	}
	return [...byName.entries()]
		.map(([name, agents]) => ({ name, installedAgents: [...agents] }))
		.sort((a, b) => a.name.localeCompare(b.name));
}

export function buildCoverageRows(
	resources: CoverageResource[],
	columnAgentIds: string[],
	applicableAgentIds: Set<string>,
): CoverageRow[] {
	return resources.map((r) => {
		const installed = new Set(r.installedAgents);
		return {
			name: r.name,
			installedAgents: installed,
			cells: columnAgentIds.map((agentId) => ({
				agentId,
				applicable: applicableAgentIds.has(agentId),
				installed: installed.has(agentId),
			})),
		};
	});
}

export type CellTogglePlan =
	| {
			kind: "reconcile";
			sourceAgent: string;
			added: string[];
			removed: string[];
	  }
	| { kind: "blocked"; reason: "last-install" }
	| { kind: "noop" };

// Plan the reconcile for toggling ONE agent on ONE resource.
//
// Removing the only remaining install is BLOCKED: that is a full uninstall, and
// nuking a resource from a stray cell click in an overview grid is too easy to
// do by accident — the user should route through the explicit delete flow. This
// is a stronger guard than `wouldOrphanSkill` (which only catches the add+remove
// copy race, never fires for a single-cell toggle). For an add, the resource's
// first existing install is the copy source; a resource with no install at all
// is a no-op (nothing to copy from — should not occur for a rendered row).
export function planCellToggle(
	installedAgents: Set<string>,
	agentId: string,
): CellTogglePlan {
	if (installedAgents.has(agentId)) {
		if (installedAgents.size <= 1) {
			return { kind: "blocked", reason: "last-install" };
		}
		const sourceAgent =
			[...installedAgents].find((a) => a !== agentId) ?? agentId;
		return {
			kind: "reconcile",
			sourceAgent,
			added: [],
			removed: [agentId],
		};
	}
	const sourceAgent = [...installedAgents][0];
	if (!sourceAgent) return { kind: "noop" };
	return { kind: "reconcile", sourceAgent, added: [agentId], removed: [] };
}
