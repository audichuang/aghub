import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { RepairReportDto, RepairResponse } from "../generated/dto";
import {
	migrationBannerModel,
	migrationRowFacts,
	migrationSummary,
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
