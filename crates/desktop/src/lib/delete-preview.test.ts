import assert from "node:assert/strict";
// No FE test runner is installed here; pure two-phase logic uses Node's runner,
// matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { DeleteSkillByPathResponse } from "../generated/dto";
import {
	canConfirm,
	confirmAll,
	confirmDelete,
	confirmOutcome,
	type DeleteFn,
	type PreviewState,
	previewAll,
	runConfirmedDelete,
	runDryRun,
} from "./delete-preview.ts";

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

/** Records every confirm flag each fn was invoked with, in order. */
function spy(over: Partial<DeleteSkillByPathResponse> = {}) {
	const calls: boolean[] = [];
	const fn: DeleteFn = async (confirm) => {
		calls.push(confirm);
		return res({ ...over, dry_run: !confirm, executed: confirm });
	};
	return { fn, calls };
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

// --- The confirm gate (#5 audit, major finding) -------------------------------
// previewAll / confirmAll are the two phases useDeletePreview and
// DeletePreviewDialog drive. A call site that deletes without a preview (the
// sources-page bug) is exactly previewAll being skipped or confirmAll running
// before a successful preview.

test("previewAll runs ONLY confirm=false and aggregates+dedupes paths", async () => {
	const a = spy({ paths: ["/x", "/shared"], skipped: ["/s1"] });
	const b = spy({ paths: ["/y", "/shared"], skipped: ["/s1", "/s2"] });
	const preview = await previewAll([a.fn, b.fn]);
	assert.deepEqual(a.calls, [false], "must never execute in preview");
	assert.deepEqual(b.calls, [false], "must never execute in preview");
	assert.deepEqual(preview.paths, ["/x", "/shared", "/y"]);
	assert.deepEqual(preview.skipped, ["/s1", "/s2"]);
});

test("previewAll rejects if ANY dry-run fails, so confirm is never reached", async () => {
	const ok = spy({ paths: ["/x"] });
	const bad: DeleteFn = async () => res({ success: false, error: "nope" });
	await assert.rejects(previewAll([ok.fn, bad]), /nope/);
});

test("confirmAll runs ONLY confirm=true and reports failed indices", async () => {
	const a = spy();
	const fail: DeleteFn = async () => res({ success: false, error: "x" });
	const c = spy();
	const failed = await confirmAll([a.fn, fail, c.fn]);
	assert.deepEqual(a.calls, [true]);
	assert.deepEqual(c.calls, [true]);
	assert.deepEqual(failed, [1], "the rejected fn's index is reported");
});

test("confirmAll returns empty when every delete succeeds", async () => {
	const a = spy();
	const b = spy();
	assert.deepEqual(await confirmAll([a.fn, b.fn]), []);
});

// --- The confirm-button gate (#5 audit, finding #3) ---------------------------
// canConfirm is bound to DeletePreviewDialog's danger-button isDisabled, and
// confirmOutcome drives its onConfirmed/onFailed branch. Testing them pins the
// call-site invariants without a React renderer: confirm can NEVER fire before a
// successful preview, and a partial failure must not report success.

test("canConfirm is false until the preview is ready", () => {
	const preview: PreviewState = {
		status: "ready",
		preview: { paths: [], skipped: [] },
	};
	// Confirm is impossible while idle / loading / errored — i.e. before a
	// successful dry-run preview exists. This is the regression guard against a
	// call site reaching confirm=true without showing the paths first.
	assert.equal(canConfirm({ status: "idle" }, false), false);
	assert.equal(canConfirm({ status: "loading" }, false), false);
	assert.equal(canConfirm({ status: "error", message: "x" }, false), false);
	assert.equal(canConfirm(preview, false), true, "ready → confirm allowed");
});

test("canConfirm is false while a delete is already in flight", () => {
	const preview: PreviewState = {
		status: "ready",
		preview: { paths: ["/a"], skipped: [] },
	};
	assert.equal(
		canConfirm(preview, true),
		false,
		"must not allow a second confirm while deleting",
	);
});

test("confirmOutcome: any failed index reports failure, none reports ok", () => {
	assert.deepEqual(confirmOutcome([]), { ok: true });
	assert.deepEqual(confirmOutcome([1, 3]), {
		ok: false,
		failed: [1, 3],
	});
});

test("the gate sequence: a failed preview must abort before any confirm", async () => {
	// Mirrors DeletePreviewDialog: preview first; confirm only if it resolved.
	const ok = spy();
	const bad: DeleteFn = async () => res({ success: false, error: "boom" });
	let confirmed = false;
	try {
		await previewAll([ok.fn, bad]);
		confirmed = true;
		await confirmAll([ok.fn, bad]);
	} catch {
		// expected
	}
	assert.equal(
		confirmed,
		false,
		"confirm must not run after a failed preview",
	);
	assert.ok(
		!ok.calls.includes(true),
		"no fn may be executed with confirm=true when preview failed",
	);
});

// --- The confirm-gate controller (#5 audit) -----------------------------------
// `confirmDelete` is the EXACT pure controller DeletePreviewDialog's danger
// button runs. These tests would FAIL if a call site (the sources-page bug)
// reverted to running the destructive executor without a successful preview.
// We pass a spy executor so "did confirm=true run?" is directly observable
// without a React renderer.

const readyState: PreviewState = {
	status: "ready",
	preview: { paths: ["/a"], skipped: [] },
};

/** Records whether the destructive executor was invoked, and its result. */
function execSpy(failed: number[] = []) {
	const calls: DeleteFn[][] = [];
	const exec = async (fns: DeleteFn[]) => {
		calls.push(fns);
		return failed;
	};
	return { exec, calls };
}

test("confirmDelete runs the destructive executor ONLY when the preview is ready", async () => {
	const e = execSpy();
	const events: string[] = [];
	const ran = await confirmDelete(readyState, false, [spy().fn], e.exec, {
		onConfirmed: () => void events.push("confirmed"),
		onFailed: () => void events.push("failed"),
		onClose: () => void events.push("close"),
	});
	assert.equal(
		ran,
		true,
		"the gate must run the destructive phase when ready",
	);
	assert.equal(e.calls.length, 1, "executor invoked exactly once");
	assert.deepEqual(events, ["confirmed", "close"]);
});

for (const blocked of [
	{ status: "idle" } as PreviewState,
	{ status: "loading" } as PreviewState,
	{ status: "error", message: "preview failed" } as PreviewState,
]) {
	test(`confirmDelete NEVER runs confirm=true on a ${blocked.status} state`, async () => {
		const e = execSpy();
		const events: string[] = [];
		const ran = await confirmDelete(blocked, false, [spy().fn], e.exec, {
			onConfirmed: () => void events.push("confirmed"),
			onFailed: () => void events.push("failed"),
			onClose: () => void events.push("close"),
		});
		assert.equal(ran, false, "the gate must block a non-ready confirm");
		assert.equal(
			e.calls.length,
			0,
			"the destructive executor must NOT be invoked without a ready preview",
		);
		assert.deepEqual(events, [], "no callback fires when the gate blocks");
	});
}

test("confirmDelete blocks a second confirm while one is already in flight", async () => {
	const e = execSpy();
	const ran = await confirmDelete(readyState, true, [spy().fn], e.exec, {
		onConfirmed: () => {},
		onClose: () => {},
	});
	assert.equal(ran, false);
	assert.equal(e.calls.length, 0, "no double-fire while deleting");
});

test("confirmDelete reports partial failures via onFailed (not onConfirmed)", async () => {
	const e = execSpy([1]); // item 1 failed
	const events: string[] = [];
	let reported: number[] | undefined;
	await confirmDelete(readyState, false, [spy().fn, spy().fn], e.exec, {
		onConfirmed: () => void events.push("confirmed"),
		onFailed: (f) => {
			events.push("failed");
			reported = f;
		},
		onClose: () => void events.push("close"),
	});
	assert.deepEqual(events, ["failed", "close"], "onConfirmed must not fire");
	assert.deepEqual(
		reported,
		[1],
		"the failed index is reported to the caller",
	);
});
