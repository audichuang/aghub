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
 * The per-row facts the spec's preview must answer, derived once so the
 * component only lays them out.
 *
 * `refused` rows deliberately carry NO migration facts: nothing is moving, and
 * showing "Moves to …" beside a refusal would describe a write that will not
 * happen.
 */
export function migrationRowFacts(row: RepairReportDto): {
	refused: boolean;
	master: string | null;
	linkCount: number;
	fused: string[];
} {
	const refused = row.outcome === "refused";
	return {
		refused,
		master: refused ? null : row.master,
		linkCount: refused ? 0 : row.referrers.length,
		fused: refused ? [] : row.fused,
	};
}
