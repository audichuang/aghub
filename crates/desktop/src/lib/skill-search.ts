import Fuse from "fuse.js";

/** The shape both callers search over — the skill list's grouped rows. */
export interface SearchableSkillGroup {
	name: string;
	description?: string;
}

/**
 * ONE spelling of "does this query match this skill", shared by the list's
 * filter and the detail panel's "is the open skill still in the results?"
 * banner.
 *
 * These were hand-mirrored copies. They must agree: the banner's whole job is
 * to explain why a skill the list is no longer showing is still open, so a
 * threshold that drifts between them makes it either lie or never appear.
 */
export const SKILL_SEARCH_OPTIONS = {
	keys: [
		{ name: "name", weight: 2 },
		{ name: "description", weight: 1 },
	],
	threshold: 0.4,
	includeScore: true,
};

/**
 * Build the index. Callers must memoize this on the DATA alone — never on the
 * query — or the index is rebuilt on every keystroke.
 */
export function createSkillSearch<T extends SearchableSkillGroup>(
	groups: T[],
): Fuse<T> {
	return new Fuse(groups, SKILL_SEARCH_OPTIONS);
}
