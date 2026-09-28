import Fuse from "fuse.js";

/** The shape both callers search over — the MCP list's merged rows. */
export interface SearchableMcpGroup {
	mergeKey: string;
}

/**
 * ONE spelling of "does this query match this MCP group", shared by the list's
 * filter and the detail panel's "open server is outside the results" banner
 * (as `skill-search.ts` is for skills) — if the two drift, the banner lies.
 *
 * Keys are `items.<field>`, NEVER `items.0.<field>`: Fuse walks every array
 * element and looks `0` up on each, so that key matches nothing. Walking every
 * member is intended — a merged group's members differ by agent.
 * See docs/history/desktop-frontend.md#mcp-search-matched-nothing
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
