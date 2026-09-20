import Fuse from "fuse.js";

/** The shape both callers search over — the MCP list's merged rows. */
export interface SearchableMcpGroup {
	mergeKey: string;
}

/**
 * ONE spelling of "does this query match this MCP group", shared by the list's
 * filter and the detail panel's "is the open server still in the results?"
 * banner.
 *
 * The same seam `skill-search.ts` is for skills, added for the same reason:
 * the banner's whole job is to explain why a server the list is no longer
 * showing is still open on the right, so a threshold or a key set that drifts
 * between the two makes it either lie or never appear. The MCP page had no
 * banner at all — the list said "no matching servers" while the right panel
 * kept a fully operable edit/delete/duplicate surface for a server outside the
 * results — so this seam exists before the second copy does.
 *
 * The keys are `items.<field>`, NOT `items.0.<field>` — which is what the
 * list used and is why moving this here found a second bug. Fuse splits a key
 * on "." and, on reaching an array, walks EVERY element; the literal "0" is
 * then looked up as a property of each element object and is always
 * `undefined`. `items.0.name` therefore matched nothing, for any query: the
 * MCP search box emptied the list on the first keystroke and never found a
 * server. (A QA pass that only searched for a string nothing should match
 * reads that as correct, which is how it survived.)
 *
 * Walking every element is also the RIGHT answer here rather than a
 * workaround: a merged group's members share a name and a source but differ
 * by AGENT, so "cursor" should find a group that has a cursor member even
 * when it is not the first one.
 */
export const MCP_SEARCH_OPTIONS = {
	keys: [
		{ name: "items.name", weight: 2 },
		{ name: "items.source", weight: 1 },
		{ name: "items.agent", weight: 1 },
	],
	threshold: 0.4,
	includeScore: true,
};

/**
 * Build the index. Callers must memoize this on the DATA alone — never on the
 * query — or the index is rebuilt on every keystroke.
 */
export function createMcpSearch<T extends SearchableMcpGroup>(
	groups: T[],
): Fuse<T> {
	return new Fuse(groups, MCP_SEARCH_OPTIONS);
}

/**
 * Is `mergeKey` still in the results for `query`?
 *
 * `true` for an empty query: nothing is filtered out, so nothing is outside
 * the results. Returning `false` there would show the banner permanently on a
 * page whose search box is untouched — which is the default state.
 */
export function mcpGroupMatchesSearch<T extends SearchableMcpGroup>(
	search: Fuse<T>,
	query: string,
	mergeKey: string | null,
): boolean {
	const trimmed = query.trim();
	if (!trimmed || mergeKey === null) return true;
	return search.search(trimmed).some((r) => r.item.mergeKey === mergeKey);
}
