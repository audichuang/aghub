import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { RepairReportDto, RepairResponse } from "../generated/dto";
import {
	isBlocked,
	migrationBannerModel,
	migrationRowFacts,
	migrationSummary,
	migrationToastMessage,
	repairScope,
} from "./skill-migration.ts";

function row(over: Partial<RepairReportDto> = {}): RepairReportDto {
	return {
		name: "my-skill",
		shape: "unmigrated_copy",
		outcome: "migrated",
		reason: null,
		fix: null,
		master: "/home/u/.aghub/my-skill",
		referrers: ["/home/u/.cursor/skills/my-skill"],
		unlinked: [],
		quarantined: null,
		fused: [],
		...over,
	};
}

function answer(over: Partial<RepairResponse> = {}): RepairResponse {
	return {
		dry_run: true,
		scope: "global",
		skills: [],
		refused: false,
		...over,
	};
}

// THE trap desktop AGENTS.md names by hand. Getting this wrong tells a user
// with a broken layout that they are fine — the one failure mode a migration
// banner must not have.
test("a failed preview never renders as 'nothing to migrate'", () => {
	assert.equal(
		migrationBannerModel(undefined, false).visible,
		false,
		"query failed with no data: hidden, and that is NOT an all-clear",
	);
	assert.equal(
		migrationBannerModel(answer({ skills: [row()] }), false).visible,
		false,
		"isSuccess gates it — stale data from a failed refetch is not trusted",
	);
	assert.equal(
		migrationBannerModel(answer(), true).visible,
		false,
		"a real empty answer is also hidden: same pixels, different reason",
	);
});

test("the banner appears only when the dry run actually found work", () => {
	const model = migrationBannerModel(answer({ skills: [row()] }), true);
	assert.equal(model.visible, true);
	assert.deepEqual(
		model.rows.map((r) => r.name),
		["my-skill"],
	);
});

// The three facts the spec's preview requires.
test("a migrating row exposes master, link count and who stays fused", () => {
	const facts = migrationRowFacts(row({ fused: ["codex", "warp"] }));
	assert.equal(facts.refused, false);
	assert.equal(facts.master, "/home/u/.aghub/my-skill");
	assert.equal(facts.linkCount, 1);
	assert.deepEqual(
		facts.fused,
		["codex", "warp"],
		"who does NOT become individually revocable is half the answer",
	);
});

// A refusal writes nothing, so it must not advertise a move.
test("a refused row carries no migration facts", () => {
	const facts = migrationRowFacts(
		row({ outcome: "refused", reason: "differs", fix: "diff -r a b" }),
	);
	assert.equal(facts.refused, true);
	assert.equal(
		facts.master,
		null,
		"nothing is moving; do not promise a path",
	);
	assert.equal(facts.linkCount, 0);
	assert.deepEqual(facts.fused, []);
});

// The whole point of the summary: the repeated facts are hoisted, and the
// refused rows are counted apart because they migrate nothing.
test("the summary hoists the scope-wide facts and excludes refusals", () => {
	const s = migrationSummary([
		row({ name: "a", fused: ["cline", "warp"] }),
		row({
			name: "b",
			master: "/home/u/.aghub/b",
			referrers: ["/home/u/.claude/skills/b", "/home/u/.codex/skills/b"],
			fused: ["warp"],
		}),
		row({
			name: "c",
			outcome: "refused",
			referrers: ["x"],
			fused: ["nope"],
		}),
	]);
	assert.equal(s.migrating, 2);
	assert.equal(s.refused, 1);
	assert.equal(s.masterParent, "/home/u/.aghub");
	assert.equal(s.totalLinks, 3, "a refused row promises no links");
	assert.deepEqual(
		s.fused,
		["cline", "warp"],
		"the union, deduped — and never an agent named only by a refusal",
	);
});

test("an all-refused preview promises no store path", () => {
	const s = migrationSummary([row({ outcome: "refused" })]);
	assert.equal(s.migrating, 0);
	assert.equal(s.masterParent, null, "nothing is moving anywhere");
	assert.equal(s.totalLinks, 0);
});

// The backend answers with the host's separators; a Windows master must not
// report itself as its own parent.
test("a windows master path is cut at its own separator", () => {
	const s = migrationSummary([row({ master: "C:\\Users\\u\\.aghub\\a" })]);
	assert.equal(s.masterParent, "C:\\Users\\u\\.aghub");
});

// `failed` is a second blocked kind: core keeps it apart from `refused` because
// re-running helps only for one of them, but the dialog owes the user the same
// treatment for both — and the summary must not count either as migrating.
test("a failed row is blocked exactly like a refused one", () => {
	const failed = row({
		outcome: "failed",
		reason: "Permission denied",
		fix: "fix the permission, then re-run",
	});
	assert.equal(isBlocked(failed), true);
	const facts = migrationRowFacts(failed);
	assert.equal(facts.refused, true, "it renders through the blocked branch");
	assert.equal(facts.master, null, "nothing landed; promise no path");
	assert.equal(facts.linkCount, 0);

	const s = migrationSummary([row({ name: "ok" }), failed]);
	assert.equal(s.migrating, 1);
	assert.equal(s.refused, 1, "blocked is blocked, whichever kind");
	assert.equal(s.totalLinks, 1, "a failed row promises no links");
});

// A pure-`tidied` row only detaches a stale Referrer left in a read-only
// compat dir — it creates zero links and moves nothing to the Master. It must
// not be folded into `migrating` (which used to claim every non-blocked row
// "moves to" the store) and must surface as its own `tidying` count instead.
test("a pure-tidied row is tidying, not migrating, and promises no store path", () => {
	const tidied = row({
		outcome: "tidied",
		referrers: [],
		unlinked: ["/home/u/.agent/skills/my-skill"],
	});
	assert.equal(
		isBlocked(tidied),
		false,
		"tidied is a completed action, not a refusal",
	);

	const s = migrationSummary([tidied]);
	assert.equal(s.migrating, 0, "detaching a stale link is not a migration");
	assert.equal(s.tidying, 1);
	assert.equal(
		s.masterParent,
		null,
		"nothing moved anywhere — do not promise a store path",
	);
	assert.equal(s.totalLinks, 0);
	assert.equal(s.totalUnlinked, 1);
});

// A mixed scope must still show the store path for the row that really
// migrates, while counting the tidied one separately.
test("a mixed migrating + tidying scope counts both and keeps the store path", () => {
	const s = migrationSummary([
		row({ name: "a" }), // default: migrated, 1 referrer
		row({
			name: "b",
			outcome: "tidied",
			referrers: [],
			unlinked: ["/home/u/.agent/skills/b"],
		}),
	]);
	assert.equal(s.migrating, 1);
	assert.equal(s.tidying, 1);
	assert.equal(
		s.masterParent,
		"/home/u/.aghub",
		"the migrating row still has a store path to show",
	);
});

// Pins `migrating`'s new `outcome !== "tidied"` guard against narrowing too
// far: `relinked` and `reconciled` rows are real migration actions and must
// not fall out of `migrating` just because they are not literally
// `outcome === "migrated"`.
test("a relinked row still counts as migrating", () => {
	const s = migrationSummary([row({ outcome: "relinked" })]);
	assert.equal(s.migrating, 1);
});

// THE bug this whole helper exists to fix: a commit that ONLY detached stale
// Referrers migrated nothing, so the toast must not read "Migrated N
// skill(s)" — that is the exact false claim a pure-tidied commit makes.
// `notEqual` is the regression guard: reverting to `result.skills.length`
// logic (every acted-on row counted as "migrated") makes this go red.
test("a pure-tidied COMMIT toasts as tidied, never as migrated", () => {
	const msg = migrationToastMessage([
		row({
			outcome: "tidied",
			referrers: [],
			unlinked: ["/home/u/.agent/skills/my-skill"],
		}),
	]);
	assert.notEqual(
		msg.key,
		"skillLayoutMigrated",
		"nothing was migrated — only a stale link was detached",
	);
	assert.equal(msg.key, "skillLayoutSummaryTidiedDone");
	assert.equal(msg.count, 1);
});

test("a pure-migrated commit still toasts as migrated", () => {
	const msg = migrationToastMessage([row(), row({ name: "b" })]);
	assert.equal(msg.key, "skillLayoutMigrated");
	assert.equal(msg.count, 2);
});

// A batch commit can migrate some skills and merely tidy others in the same
// run — the toast must own up to both counts rather than picking one.
test("a mixed migrated + tidied commit toasts both counts", () => {
	const msg = migrationToastMessage([
		row({ name: "a" }),
		row({
			name: "b",
			outcome: "tidied",
			referrers: [],
			unlinked: ["/home/u/.agent/skills/b"],
		}),
	]);
	assert.equal(msg.key, "skillLayoutMigratedAndTidied");
	assert.equal(msg.migrated, 1);
	assert.equal(msg.tidied, 1);
});

// A "Run again" click after everything is already fixed (or a bulk re-run
// that finds only conformant skills) commits real skills but changes
// nothing — the toast must say so honestly instead of claiming "Migrated 0
// skill(s)", which reads as a broken button, not as "you're done".
test("a commit that changed nothing toasts as nothing left, not zero migrated", () => {
	const msg = migrationToastMessage([]);
	assert.equal(msg.key, "skillLayoutNothingToMigrate");
	assert.notEqual(msg.key, "skillLayoutMigrated");
});

// ROUND 3 REGRESSION: core's compat-dir sweep grants a BRAND-NEW Referrer
// and detaches a stale compat link in the SAME row — the central case the
// sweep exists for (an agent's write slot was Absent and it read the skill
// only through a read-only compat dir; see
// crates/core/src/skills/shape.rs `compat_unlink_permitted` and
// crates/core/src/skills/repair.rs step 4 vs step 6). `Create` never
// promotes the outcome away from `conformant`, only `Relink` does, so this
// row reports `outcome: "tidied"` even though `referrers.length > 0`. The
// Master pre-existed; nothing moved to the store, so this must stay OUT of
// `migrating` — gated on `outcome`, never on `referrers.length` alone.
function grantedAndDetachedRow(): RepairReportDto {
	return row({
		outcome: "tidied",
		referrers: ["/home/u/.claude/skills/my-skill"],
		unlinked: ["/home/u/.agent/skills/my-skill"],
	});
}

// Preview path: the dialog's own summary must count this row as tidying
// only, and must not promise a store path for it — the Master never moved.
test("a row that grants a referrer AND detaches a compat link previews as tidying only", () => {
	const s = migrationSummary([grantedAndDetachedRow()]);
	assert.equal(
		s.migrating,
		0,
		"core calls this row tidied — the Master never moved",
	);
	assert.equal(s.tidying, 1, "it DID detach a stale compat link");
	assert.equal(
		s.masterParent,
		null,
		"the Master pre-existed; nothing is moving anywhere",
	);
	assert.equal(s.totalUnlinked, 1);
});

// Commit path: the toast must not read "Migrated 1 skill(s)" for a row core
// itself calls `tidied` — that is the exact false claim this whole helper
// exists to remove, re-surfacing for the commonest run the feature has.
test("the same row's commit toasts as tidied, never as migrated", () => {
	const msg = migrationToastMessage([grantedAndDetachedRow()]);
	assert.notEqual(
		msg.key,
		"skillLayoutMigrated",
		"a link was granted, but core did not call this row a migration",
	);
	assert.equal(msg.key, "skillLayoutSummaryTidiedDone");
	assert.equal(msg.count, 1);
});

// The OTHER shape that is genuinely BOTH: shape.rs's compat sweep can detach
// a stale link in the SAME pass that adopts a brand-new Master — an entry
// resolving to the about-to-be-adopted directory is unlinked once step 5
// turns that directory into the Master itself. Unlike the row above, core
// DOES call this one `migrated` (adopt always wins the outcome), so it must
// land in `migrating` too — `tidying` must not be gated on `outcome` in a way
// that only recognizes the `"tidied"` label.
test("an adopt that also detaches a compat link counts as both, and toasts both", () => {
	const adoptedAndDetached = row({
		unlinked: ["/home/u/.agent/skills/my-skill"],
	}); // default outcome "migrated", 1 referrer, from the `row()` fixture

	const s = migrationSummary([adoptedAndDetached]);
	assert.equal(s.migrating, 1, "core called this row migrated");
	assert.equal(s.tidying, 1, "it ALSO detached a stale compat link");
	assert.equal(
		s.masterParent,
		"/home/u/.aghub",
		"the Master really was adopted here",
	);

	const msg = migrationToastMessage([adoptedAndDetached]);
	assert.equal(msg.key, "skillLayoutMigratedAndTidied");
	assert.equal(msg.migrated, 1);
	assert.equal(msg.tidied, 1);
});

// `repairScope` decides what the button WRITES. `names: undefined` is a BULK
// repair, so every way of arriving there by accident needs its own case.

test("repairScope: the last run's rows must never become the scope basis", () => {
	// The round-3 defect, as the sequence a user performs: preview [a, b],
	// uncheck b, commit `names: [a]`. Afterwards the live preview still holds
	// b. Basing the scope on the RESULT ([a]) makes picked === all and posts
	// `undefined` — a bulk repair over the row the user deselected.
	const preview = [row({ name: "a" }), row({ name: "b" })];
	const lastRun = [row({ name: "a" })];
	const after = repairScope(preview, lastRun, new Set(["a"]));
	assert.deepEqual(after.names, []);
	assert.equal(after.count, 0);
});

test("repairScope: a completed name is subtracted even from a stale preview", () => {
	const preview = [row({ name: "a" }), row({ name: "b" })];
	const lastRun = [row({ name: "a" })];
	const scope = repairScope(preview, lastRun, null);
	assert.equal(scope.names, undefined);
	assert.equal(scope.count, 1);
});

test("repairScope: nothing outstanding is not the same as everything", () => {
	// `0 === 0` used to read as "all selected" and post a bulk repair.
	assert.deepEqual(repairScope([], null, null).names, []);
	assert.equal(repairScope([], null, new Set()).count, 0);
	const allBlocked = [
		row({ name: "bad", outcome: "refused", reason: "r", fix: "f" }),
	];
	assert.deepEqual(repairScope(allBlocked, null, null).names, []);
});

test("repairScope: no selection narrows to nothing; a full one is one bulk call", () => {
	const preview = [row({ name: "a" }), row({ name: "b" })];
	assert.deepEqual(repairScope(preview, null, new Set()).names, []);
	assert.deepEqual(repairScope(preview, null, new Set(["a"])).names, ["a"]);
	assert.equal(repairScope(preview, null, null).names, undefined);
});
