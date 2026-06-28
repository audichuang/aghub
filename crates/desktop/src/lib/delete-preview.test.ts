import assert from "node:assert/strict";
// No FE test runner is installed here; pure two-phase logic uses Node's runner,
// matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { DeleteSkillByPathResponse } from "../generated/dto";
import { deleteWithDryRun } from "./delete-preview.ts";

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

test("runs dry-run first, then executes with confirm=true", async () => {
	const calls: boolean[] = [];
	await deleteWithDryRun(async (confirm) => {
		calls.push(confirm);
		return res({ dry_run: !confirm, executed: confirm });
	});
	assert.deepEqual(calls, [false, true]);
});

test("needs_confirm dry-run still proceeds to the confirmed delete", async () => {
	// Regression (#5 audit): all-agents / symlink-layout skill removal previews
	// with needs_confirm=true. The caller already gathered the user's OK in its
	// confirm dialog, so the second phase IS that confirmation — it must NOT
	// throw "additional confirmation" and abandon the delete.
	const calls: boolean[] = [];
	const out = await deleteWithDryRun(async (confirm) => {
		calls.push(confirm);
		return res({
			needs_confirm: true,
			dry_run: !confirm,
			executed: confirm,
		});
	});
	assert.deepEqual(calls, [false, true], "must run both phases");
	assert.equal(out.executed, true);
});

test("a failed dry-run short-circuits before the destructive call", async () => {
	const calls: boolean[] = [];
	await assert.rejects(
		deleteWithDryRun(async (confirm) => {
			calls.push(confirm);
			return res({ success: false, error: "boom" });
		}),
		/boom/,
	);
	assert.deepEqual(calls, [false], "must not call confirm=true on failure");
});
