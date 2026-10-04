/**
 * Which agents a "delete skill" request names, split by whether aghub manages
 * them.
 *
 * `SkillGroup.items` is unfiltered: it carries a row for every agent that reads
 * the skill, disabled ones included. A disabled agent is one the user told
 * aghub not to manage, but its link still keeps the shared Master alive, so a
 * delete that leaves it out keeps the skill on disk while reporting success.
 * Naming it is the consent the server needs to take it away (core
 * `unmanaged_skill_dirs`), so it is an explicit opt-in rather than being swept
 * along silently.
 */
export interface DeleteTargetItem {
	agent?: string | null;
	source?: string | null;
}

export interface DeleteTargets<T extends DeleteTargetItem> {
	/** Rows for agents aghub manages — always part of the request. */
	managed: T[];
	/** Rows for agents that still read the skill but are not managed. */
	unmanaged: T[];
	/** The rows the request names: managed, plus unmanaged when opted in. */
	named: T[];
}

export function splitDeleteTargets<T extends DeleteTargetItem>(
	items: readonly T[],
	managedAgentIds: ReadonlySet<string>,
	includeUnmanaged: boolean,
): DeleteTargets<T> {
	const withAgent = items.filter((item) => !!item.agent);
	const managed = withAgent.filter((item) =>
		managedAgentIds.has(item.agent as string),
	);
	const unmanaged = withAgent.filter(
		(item) => !managedAgentIds.has(item.agent as string),
	);
	return {
		managed,
		unmanaged,
		named: includeUnmanaged ? [...managed, ...unmanaged] : managed,
	};
}
