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

export interface BulkDeleteItem extends DeleteTargetItem {
	name: string;
	source_path?: string | null;
}

export interface BulkDeleteGroup {
	key: string;
	items: BulkDeleteItem[];
	resourceType?: "mcp" | "skill";
}

export interface BulkDeleteRequest {
	resourceType: "mcp" | "skill";
	name: string;
	groupKey: string;
	agent: string;
	scope: "global" | "project";
	projectRoot?: string;
	agents: string[];
	sourcePath?: string | null;
}

export interface BuildBulkDeleteRequestsOptions {
	groups: readonly BulkDeleteGroup[];
	resourceType: "mcp" | "skill" | "mixed";
	managedAgentIds: ReadonlySet<string>;
	includeUnmanaged: boolean;
	projectPath?: string;
}

/**
 * Builds the list of delete requests for bulk delete.
 *
 * For skills, disabled agents are excluded from both target items and the
 * `agents` parameter unless the user explicitly ticked consent
 * (`includeUnmanaged === true`).
 */
export function buildBulkDeleteRequests({
	groups,
	resourceType,
	managedAgentIds,
	includeUnmanaged,
	projectPath,
}: BuildBulkDeleteRequestsOptions): BulkDeleteRequest[] {
	const requests: BulkDeleteRequest[] = [];
	const seen = new Set<string>();

	for (const group of groups) {
		const groupResourceType = group.resourceType ?? resourceType;
		const isSkill = groupResourceType === "skill";
		const targets = isSkill
			? splitDeleteTargets(group.items, managedAgentIds, includeUnmanaged)
			: null;
		const candidateItems = targets ? targets.named : group.items;

		for (const item of candidateItems) {
			if (!item.agent) continue;
			const scope: "global" | "project" =
				item.source === "project" ? "project" : "global";
			const projectRoot = scope === "project" ? projectPath : undefined;

			// Every agent this group is deleted from at this scope. A
			// shared config file or Referrer is removed only when all of
			// its readers ride in the same request. Disabled agents must
			// never be named without consent.
			const scopeAgents = (targets ? targets.named : candidateItems)
				.filter((other) => (other.source ?? "global") === scope)
				.flatMap((other) => (other.agent ? [other.agent] : []));

			const dedupKey =
				groupResourceType === "skill" && item.source_path
					? `skill:${item.source_path}:${scope}`
					: groupResourceType === "skill"
						? `skill:${item.agent}:${group.key}:${scope}`
						: `${groupResourceType}:${item.agent}:${item.name}:${scope}`;

			if (seen.has(dedupKey)) continue;
			seen.add(dedupKey);

			requests.push({
				resourceType: groupResourceType === "mcp" ? "mcp" : "skill",
				name: item.name,
				groupKey: group.key,
				agent: item.agent,
				scope,
				projectRoot,
				agents: [...new Set(scopeAgents)],
				sourcePath: item.source_path ?? null,
			});
		}
	}

	return requests;
}

/**
 * Collects all unmanaged (disabled) items across skill groups so the UI
 * knows whether to prompt the user for consent.
 */
export function collectUnmanagedDeleteTargets(
	groups: readonly BulkDeleteGroup[],
	managedAgentIds: ReadonlySet<string>,
	resourceType: "mcp" | "skill" | "mixed",
): BulkDeleteItem[] {
	const unmanaged: BulkDeleteItem[] = [];
	for (const group of groups) {
		const groupResourceType = group.resourceType ?? resourceType;
		if (groupResourceType === "skill") {
			const targets = splitDeleteTargets(
				group.items,
				managedAgentIds,
				false,
			);
			unmanaged.push(...targets.unmanaged);
		}
	}
	return unmanaged;
}
