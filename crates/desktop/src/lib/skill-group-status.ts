import type { SkillUpdateResponse } from "../generated/dto";

/**
 * Whether a source group's per-skill update badges can be collapsed into
 * ONE line on the group header, and why.
 *
 * `"none"` covers every case that must keep per-row badges: an empty group,
 * a skill with no status yet (a check has not run — not the same claim as
 * "cannot check"), a skill that IS checkable, or a mix of different
 * `uncheckable` reasons.
 */
export type SharedUncheckableReason =
	| { kind: "none" }
	| { kind: "auth" }
	| { kind: "other"; reason: string };

/**
 * When every visible skill in a group is `uncheckable` for the exact same
 * reason, the group heading can say so ONCE instead of repeating a badge on
 * every row below it.
 */
export function sharedUncheckableReason(
	names: readonly string[],
	statuses: ReadonlyMap<string, SkillUpdateResponse>,
): SharedUncheckableReason {
	if (names.length === 0) return { kind: "none" };
	const first = statuses.get(names[0]);
	if (!first || first.status !== "uncheckable") return { kind: "none" };
	const reason = first.reason;
	for (const name of names) {
		const status = statuses.get(name);
		if (
			!status ||
			status.status !== "uncheckable" ||
			status.reason !== reason
		) {
			return { kind: "none" };
		}
	}
	return reason === "auth" ? { kind: "auth" } : { kind: "other", reason };
}

/**
 * Human-readable i18n key for an `uncheckable` reason's tooltip.
 *
 * Lives here (not in `skill-update-badge.tsx`, which imports it) so both the
 * per-row badge and the group-header rollup share ONE mapping — a `.tsx`
 * file cannot be imported by a `lib/*.test.ts` module, since `node --test`
 * strips types but does not transform JSX.
 */
export function uncheckableTooltipKey(reason: string): string {
	switch (reason) {
		case "auth":
			return "skillUncheckableAuth";
		case "network":
			return "skillUncheckableNetwork";
		case "local":
			return "skillUncheckableLocal";
		case "ssh":
		case "unsupportedScheme":
			return "skillUncheckableUnsupported";
		case "noPath":
			return "skillUncheckableNoPath";
		case "timeout":
			return "skillUncheckableTimeout";
		default:
			return "skillUncheckableGeneric";
	}
}

/**
 * Which skills a check could not verify, and why — so the "N could not be
 * verified" toast can name them instead of leaving only a count. Capped at
 * `limit` entries; `more` is how many were left out. Duplicate names (the same
 * skill reported for two scopes with one reason) are listed once.
 */
export function uncheckableDetails(
	rows: readonly SkillUpdateResponse[],
	limit = 3,
): { items: { name: string; reasonKey: string }[]; more: number } {
	const seen = new Set<string>();
	const all: { name: string; reasonKey: string }[] = [];
	for (const row of rows) {
		if (row.status !== "uncheckable") continue;
		const reasonKey = uncheckableTooltipKey(row.reason);
		const key = `${row.name}\u0000${reasonKey}`;
		if (seen.has(key)) continue;
		seen.add(key);
		all.push({ name: row.name, reasonKey });
	}
	return {
		items: all.slice(0, limit),
		more: Math.max(0, all.length - limit),
	};
}
