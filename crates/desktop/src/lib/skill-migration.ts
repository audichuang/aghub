import type { RepairReportDto, RepairResponse } from "../generated/dto";

/**
 * Should the migration banner show, and for how many skills?
 *
 * Lives in `lib/` rather than beside the component so `node --test` can reach
 * it: node strips types but does NOT transform JSX, so anything exported from a
 * `.tsx` file is untestable by this project's runner.
 *
 * This is where the `!isLoading` trap lives. A FAILED query settles with `data`
 * undefined, and `data?.skills ?? []` renders that as "nothing to migrate" —
 * indistinguishable from a real all-clear. A user with an un-migrated layout
 * would be told they were fine. So the gate is `isSuccess`, and mere presence
 * of (possibly stale) data is not enough.
 */
export function migrationBannerModel(
	data: RepairResponse | undefined,
	isSuccess: boolean,
): { visible: boolean; rows: RepairReportDto[] } {
	const rows = isSuccess && data ? data.skills : [];
	return { visible: rows.length > 0, rows };
}

/**
 * Rows that need the user's attention rather than a one-line summary.
 *
 * `refused` is a DECISION (repair looked and declined) and `failed` is the OS
 * saying no mid-attempt. They differ in whether re-running helps — which is why
 * core keeps them apart — but they render identically: both carry a `reason` and
 * a literal `fix`, and both mean this skill did not migrate.
 */
export function isBlocked(row: RepairReportDto): boolean {
	return row.outcome === "refused" || row.outcome === "failed";
}

/**
 * The per-row facts the spec's preview must answer, derived once so the
 * component only lays them out.
 *
 * A blocked row deliberately carries NO migration facts: nothing landed, and
 * showing "Moves to …" beside a refusal would describe a write that will not
 * happen.
 */
export function migrationRowFacts(row: RepairReportDto): {
	refused: boolean;
	master: string | null;
	linkCount: number;
	fused: string[];
} {
	const refused = isBlocked(row);
	return {
		refused,
		master: refused ? null : row.master,
		linkCount: refused ? 0 : row.referrers.length,
		fused: refused ? [] : row.fused,
	};
}

/**
 * The facts that are the SAME for every row, hoisted out of the per-skill list.
 *
 * A fifty-skill preview repeated the store path and the fused-agent sentence
 * fifty times, which buried the two things that actually differ per skill (the
 * name, and whether it was refused). These are scope-wide facts, so they belong
 * in one sentence above the list.
 *
 * `masterParent` is taken from the rows rather than composed from a home dir:
 * the store path is the backend's answer and the UI must not re-derive it.
 * `refused` counts blocked rows of BOTH kinds — the summary's job is to say
 * how many skills the user still has to deal with, and that number does not
 * care whether repair declined or the OS did.
 * `fused` is the UNION — a mixed scope shows the superset, which is the honest
 * reading of "these agents stay fused".
 */
export function migrationSummary(rows: readonly RepairReportDto[]): {
	migrating: number;
	refused: number;
	masterParent: string | null;
	totalLinks: number;
	fused: string[];
} {
	const acting = rows.filter((r) => !isBlocked(r));
	const fused = new Set<string>();
	let totalLinks = 0;
	for (const row of acting) {
		totalLinks += row.referrers.length;
		for (const agent of row.fused) fused.add(agent);
	}
	// Cut at the last separator of either flavour: these paths come from the
	// backend, so a Windows master arrives with backslashes and a `lastIndexOf`
	// on "/" alone would return the whole path as its own parent.
	const first = acting[0]?.master ?? null;
	const cut =
		first === null
			? -1
			: Math.max(first.lastIndexOf("/"), first.lastIndexOf("\\"));
	return {
		migrating: acting.length,
		refused: rows.length - acting.length,
		masterParent: first === null || cut <= 0 ? first : first.slice(0, cut),
		totalLinks,
		fused: [...fused].sort(),
	};
}
