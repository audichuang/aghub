import assert from "node:assert/strict";
// No FE test runner is installed here; pure two-phase logic uses Node's runner,
// matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { DeleteSkillByPathResponse } from "../generated/dto";
import { runConfirmedDelete, runDryRun } from "./delete-preview.ts";

function res(
	over: Partial<DeleteSkillByPathResponse>,
): DeleteSkillByPathResponse {
	return {
		success: true,
		dry_run: true,
		executed: false,
		needs_confirm: false,
		paths: [],
		skipped: [],
		deleted_path: null,
		error: null,
		pruned_lock_entries: null,
		prune_error: null,
		...over,
	} as DeleteSkillByPathResponse;
}

test("runDryRun previews with confirm=false and returns the paths", async () => {
	const calls: boolean[] = [];
	const preview = await runDryRun(async (confirm) => {
		calls.push(confirm);
		return res({ dry_run: true, paths: ["/a", "/b"] });
	});
	assert.deepEqual(calls, [false], "dry-run must not execute");
	assert.deepEqual(preview.paths, ["/a", "/b"]);
});

test("a failed dry-run throws before anything destructive", async () => {
	await assert.rejects(
		runDryRun(async () => res({ success: false, error: "boom" })),
		/boom/,
	);
});

test("runConfirmedDelete executes with confirm=true", async () => {
	const calls: boolean[] = [];
	const out = await runConfirmedDelete(async (confirm) => {
		calls.push(confirm);
		return res({ dry_run: false, executed: confirm });
	});
	assert.deepEqual(calls, [true]);
	assert.equal(out.executed, true);
});

test("needs_confirm is the normal confirmed path, never an error", async () => {
	// Regression (#5 audit): all-agents / symlink-layout removal previews with
	// needs_confirm=true. The user already confirmed in the dialog, so the
	// confirmed phase must proceed — it must NOT throw "additional confirmation".
	const out = await runConfirmedDelete(async (confirm) =>
		res({ needs_confirm: true, dry_run: !confirm, executed: confirm }),
	);
	assert.equal(out.executed, true);
});
