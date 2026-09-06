/**
 * Pure URL-state helpers for the skills page (`pages/settings/skills.tsx`).
 *
 * Scope is meant to be the page's SINGLE data root: everything else (which
 * skill/source is selected, which project's data gets fetched) derives from
 * it, and nothing may reconstruct scope from a different param. Kept here,
 * not in the page component, so `node --test` can exercise the parsing and
 * the skill/source exclusion rule directly — a regression here is a silent
 * "wrong data root" bug, not merely a visual one.
 */

/**
 * Matches the "project:" prefix `ScopeControl`'s OWN internal Select item
 * ids use, but is a SEPARATE constant on purpose: this module is a pure
 * `.ts` file and must never import from `scope-control.tsx` (a `.tsx` file
 * breaks `node --test`, which strips types but does not transform JSX).
 */
const SCOPE_PROJECT_PREFIX = "project:";

export type PageScope =
	| { scope: "global" }
	| { scope: "project"; projectPath: string | null };

/**
 * Raw `scope` URL param -> typed value.
 *
 * - Missing, unparseable, or an empty `project:` path all fall back to
 *   global.
 * - `project:<path>` for a path NOT among the app's currently known
 *   projects keeps `scope: "project"` with `projectPath: null` — the page
 *   stays on project scope (it must never silently read global data for a
 *   stale or foreign URL) while the caller renders the same "select a
 *   project" empty state it already shows before any project is chosen.
 */
export function parseScopeParam(
	raw: string | null,
	knownProjectPaths: ReadonlySet<string>,
): PageScope {
	if (raw && raw.startsWith(SCOPE_PROJECT_PREFIX)) {
		const path = raw.slice(SCOPE_PROJECT_PREFIX.length);
		if (path.length > 0) {
			return {
				scope: "project",
				projectPath: knownProjectPaths.has(path) ? path : null,
			};
		}
	}
	return { scope: "global" };
}

/**
 * Typed value -> raw `scope` URL param.
 *
 * Global always serializes to the literal default value ("global"); nuqs's
 * `clearOnDefault` (configured at the `useQueryState` call site) is what
 * omits it from the URL. This function never special-cases "no param" —
 * hand-writing that here is exactly the manual work `clearOnDefault` exists
 * to replace.
 */
export function serializeScopeParam(
	scope: "global" | "project",
	projectPath: string | null,
): string {
	if (scope === "project" && projectPath) {
		return `${SCOPE_PROJECT_PREFIX}${projectPath}`;
	}
	return "global";
}

/**
 * `skill` and `source` are mutually exclusive URL params: selecting one
 * must always clear the other, never merely leave it as whatever it was.
 * Centralizing the shape here means every call site (a list row click, a
 * group header click, a deep link) applies the SAME rule instead of each
 * one having to remember to clear the sibling param by hand.
 */
export interface SkillSourceParams {
	skill: string | null;
	source: string | null;
}

export function selectSkillParams(name: string): SkillSourceParams {
	return { skill: name, source: null };
}

export function selectSourceParams(sourceValue: string): SkillSourceParams {
	return { skill: null, source: sourceValue };
}

export function clearSelectionParams(): SkillSourceParams {
	return { skill: null, source: null };
}

/**
 * Resolve the `source` URL param to a row in the CURRENT scope's source list.
 *
 * The param can be either a source's clone URL (`sourceUrl`) or the lock's
 * bare source id (`owner/repo`, what `SkillList`'s group headers carry — they
 * never see a clone URL). Both must resolve, or a deep link built from one
 * shape silently fails against rows keyed by the other.
 *
 * The two are NOT equally unique. `sourceUrl` is unique per row by
 * construction; the bare id is host-blind, so one `owner/repo` served by two
 * forges is TWO rows sharing it. Picking the first would open a panel for the
 * wrong repository — and that panel updates and deletes skills. So an
 * ambiguous bare id resolves to nothing and the caller says it cannot find the
 * source, which is recoverable; acting on the wrong origin is not.
 */
export function resolveSourceRow<
	T extends { source: string; sourceUrl: string },
>(rows: readonly T[], value: string | null): T | null {
	if (!value) return null;
	const byUrl = rows.find((r) => r.sourceUrl === value);
	if (byUrl) return byUrl;
	const byId = rows.filter((r) => r.source === value);
	return byId.length === 1 ? byId[0] : null;
}
