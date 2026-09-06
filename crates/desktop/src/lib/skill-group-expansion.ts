/**
 * Which source groups in `components/skill-list.tsx` are open.
 *
 * The stored state is ONLY what the user explicitly toggled; every other group
 * follows the default below. The obvious alternative — seeding a `useState`
 * initializer from the group list — is what this replaces, and it is a trap:
 * the initializer runs on the FIRST render, before the skill list and lock
 * queries resolve, so it always sees zero groups and every group renders
 * collapsed forever. That was survivable while single-skill sources were
 * flattened into an always-visible list; once every source became a
 * collapsible group it meant an empty list on load and a search that appeared
 * to match nothing.
 */

/**
 * Default: open. A collapsed group shows only a header and a count, so
 * defaulting to closed hides the page's entire contents behind N clicks — and
 * during a search it reads as "no results" even when a group holds matches.
 */
export function isGroupExpanded(
	overrides: ReadonlyMap<string, boolean>,
	key: string,
): boolean {
	return overrides.get(key) ?? true;
}

export function toggleGroupExpansion(
	overrides: ReadonlyMap<string, boolean>,
	key: string,
): Map<string, boolean> {
	const next = new Map(overrides);
	next.set(key, !isGroupExpanded(overrides, key));
	return next;
}
