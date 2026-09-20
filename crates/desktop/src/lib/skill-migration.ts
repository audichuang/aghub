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
	unlinkCount: number;
	fused: string[];
} {
	const refused = isBlocked(row);
	return {
		refused,
		master: refused ? null : row.master,
		linkCount: refused ? 0 : row.referrers.length,
		// Counted and shown separately from `linkCount`: these are symlinks the
		// migration REMOVES, and a preview that only lists what it adds is the
		// one that surprises a user who created that link by hand.
		unlinkCount: refused ? 0 : row.unlinked.length,
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
 * `migrating`, `linking` and `tidying` are NOT a partition of `acting` — a row
 * can count in several (see the comments below) or in none (an already
 * conformant row that needed no action at all). `migrating` is content moving
 * into the store; `linking` is agent Referrers being created or repointed at a
 * Master that never moved. Keeping them apart is the whole point: they used to
 * be one number, and it read as "your skills are about to move".
 */
export function migrationSummary(rows: readonly RepairReportDto[]): {
	migrating: number;
	linking: number;
	tidying: number;
	refused: number;
	masterParent: string | null;
	totalLinks: number;
	migratingLinks: number;
	linkingLinks: number;
	totalUnlinked: number;
	fused: string[];
} {
	const acting = rows.filter((r) => !isBlocked(r));
	// `migrated` is the ONLY outcome that means the skill's CONTENT moved into
	// the store — see `RepairOutcome` in crates/core/src/skills/repair.rs, where
	// `relinked` and `reconciled` both describe a Master that was ALREADY there
	// and a Referrer being repointed at it.
	//
	// This used to also count any non-`tidied` row with referrers, which swept
	// both of those in. A user whose skills were already in `.aghub` and who
	// merely lacked the private slots of two newly added agents was told
	// "50 skills move to ~/.aghub" — false, and false in the direction that
	// invites a manual re-sort of a store that is already correct. The old
	// reading deliberately excluded `tidied`; it never asked what `relinked`
	// meant, because until a second agent joined the roster the two answers
	// agreed on every row anyone had looked at.
	const migrating = acting.filter((r) => r.outcome === "migrated");
	// The other half of that split: real writes that create or repoint an
	// agent's Referrer while the Master stays where it is.
	//
	// NOT the complement of `migrating` — an already-conformant row that needed
	// nothing is in neither — and deliberately NOT disjoint from `tidying`: the
	// compat-dir sweep's central case (write slot Absent, skill read only
	// through a read-only compat dir) grants a brand-new Referrer AND detaches
	// the compat link in the SAME row, and core still reports `tidied`. That
	// row did create a link, so it belongs here too.
	const linking = acting.filter(
		(r) => r.outcome !== "migrated" && r.referrers.length > 0,
	);
	// Independent of `migrating`, not its complement: shape.rs's compat sweep
	// can ALSO detach a stale link in the same pass that adopts a brand-new
	// Master (an entry resolving to the about-to-be-adopted directory is
	// unlinked once step 5 turns that directory into the Master itself), so a
	// row can be both migrating and tidying at once. Gating this on
	// `referrers.length === 0` (as it used to) silently dropped such a row
	// from here — count a row as tidying whenever it detached something,
	// whatever else it also did.
	const tidying = acting.filter((r) => r.unlinked.length > 0);
	const fused = new Set<string>();
	let totalLinks = 0;
	let totalUnlinked = 0;
	for (const row of acting) {
		totalLinks += row.referrers.length;
		totalUnlinked += row.unlinked.length;
		for (const agent of row.fused) fused.add(agent);
	}
	// Cut at the last separator of either flavour: these paths come from the
	// backend, so a Windows master arrives with backslashes and a `lastIndexOf`
	// on "/" alone would return the whole path as its own parent.
	// Taken from `migrating`, not `acting`: an all-tidied scope moves nothing,
	// so it must not promise a store path either.
	const first = migrating[0]?.master ?? null;
	const cut =
		first === null
			? -1
			: Math.max(first.lastIndexOf("/"), first.lastIndexOf("\\"));
	return {
		migrating: migrating.length,
		linking: linking.length,
		tidying: tidying.length,
		refused: rows.length - acting.length,
		totalUnlinked,
		masterParent: first === null || cut <= 0 ? first : first.slice(0, cut),
		totalLinks,
		// Split per bucket so each sentence counts only the links IT is about.
		// One shared `totalLinks` made the move sentence claim links that a
		// link-only row contributed, which is the same conflation one level down.
		migratingLinks: migrating.reduce((n, r) => n + r.referrers.length, 0),
		linkingLinks: linking.reduce((n, r) => n + r.referrers.length, 0),
		fused: [...fused].sort(),
	};
}

/**
 * The toast a just-finished commit deserves — as a translation key plus its
 * interpolation params, so the component only has to call `t(...)`.
 *
 * `result.skills.length` alone (the previous message) is the bug this fixes:
 * a commit that only detached stale Referrers migrated NOTHING, so counting
 * every row as "migrated" is the exact false claim a pure-tidied run makes.
 * Reuses `migrationSummary`'s `migrating`/`tidying` split rather than
 * re-deriving it, so the toast and the dialog's own summary can never
 * disagree about which bucket a row landed in.
 *
 * The `migrating === 0 && tidying === 0` case is real, not hypothetical: once
 * the dialog stops auto-closing (see the banner), "Run again" stays
 * clickable after a clean commit, and a bulk re-run drops every
 * now-conformant skill from the report — `skills: []`. Claiming a migration
 * there would be exactly as false as the bug this function fixes, so it gets
 * its own honest "nothing left" key instead of falling through to
 * `skillLayoutMigrated` with `count: 0`.
 */
export function migrationToastMessage(skills: readonly RepairReportDto[]): {
	key: string;
	count?: number;
	links?: number;
	migrated?: number;
	linked?: number;
	tidied?: number;
} {
	const { migrating, linking, linkingLinks, tidying } =
		migrationSummary(skills);
	if (migrating > 0 && tidying > 0) {
		return {
			key: "skillLayoutMigratedAndTidied",
			migrated: migrating,
			tidied: tidying,
		};
	}
	if (migrating > 0) {
		return { key: "skillLayoutMigrated", count: migrating };
	}
	// Above the tidied arms, and separate from the migrated ones: a run that
	// only created or repointed Referrers WROTE something, so "nothing left to
	// migrate" would be as false here as "migrated 50 skills" was.
	if (linking > 0 && tidying > 0) {
		return {
			key: "skillLayoutLinkedAndTidied",
			linked: linking,
			tidied: tidying,
		};
	}
	if (linking > 0) {
		// Reuses the dialog's own done-state sentence, like the tidied arm does.
		return {
			key: "skillLayoutSummaryLinkedDone",
			count: linking,
			links: linkingLinks,
		};
	}
	if (tidying > 0) {
		// Reuses the dialog's own done-state sentence rather than minting a
		// near-duplicate — it already says exactly this.
		return { key: "skillLayoutSummaryTidiedDone", count: tidying };
	}
	return { key: "skillLayoutNothingToMigrate" };
}

/**
 * What a repair button will actually WRITE.
 *
 * `names: undefined` means a BULK repair — every skill the lock names at this
 * scope. That is the API's contract, and it is why this decision is a function
 * with BOTH candidate inputs rather than an inline ternary: it used to widen
 * silently in two different ways, and each one had to be reachable by a test.
 *
 *  - **Basis.** The scope is the OUTSTANDING work (`previewRows`), never the
 *    last run's rows. Basing it on the result meant that after a narrowed
 *    commit those rows were exactly what had just been committed, so
 *    `picked.length === selectable.length` turned true and "Run again" posted no
 *    names — a bulk repair that migrated the row the user had explicitly
 *    deselected. `lastRunRows` is taken so the completed names can be SUBTRACTED
 *    (a stale preview must not re-offer them), never used as the basis.
 *  - **Empty.** `0 === 0` is also "everything", so a press with nothing checked
 *    posted a bulk repair. Empty stays empty here; the caller disables the
 *    button on `count === 0`.
 */
export function repairScope(
	previewRows: readonly RepairReportDto[],
	lastRunRows: readonly RepairReportDto[] | null,
	picked: ReadonlySet<string> | null,
): { names: string[] | undefined; count: number } {
	const alreadyDone = new Set(
		(lastRunRows ?? []).filter((r) => !isBlocked(r)).map((r) => r.name),
	);
	const selectable = previewRows
		.filter((r) => !isBlocked(r))
		.map((r) => r.name)
		.filter((name) => !alreadyDone.has(name));
	const pickedNames = selectable.filter(
		(name) => picked === null || picked.has(name),
	);
	const all =
		pickedNames.length > 0 && pickedNames.length === selectable.length;
	return { names: all ? undefined : pickedNames, count: pickedNames.length };
}
