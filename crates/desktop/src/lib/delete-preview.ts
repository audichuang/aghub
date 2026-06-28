import { useCallback, useState } from "react";
import type { DeleteSkillByPathResponse } from "../generated/dto";

/**
 * The delete endpoints (skill / mcp / sub-agent) all return the shared
 * `RemovalView` wire shape, generated here as `DeleteSkillByPathResponse`.
 */
export type RemovalView = DeleteSkillByPathResponse;

/**
 * A bound delete call: everything except `confirm` is already closed over, so
 * the SAME closure runs the dry-run (confirm=false) and the real delete
 * (confirm=true). One per item/agent/scope being removed.
 */
export type DeleteFn = (confirm: boolean) => Promise<RemovalView>;

/**
 * Task 15 (#5) two-step destructive delete.
 *
 * The bug this replaces: the old `deleteWithDryRun` ran confirm=false then
 * IMMEDIATELY confirm=true with no UI in between — so desktop deleted without
 * ever showing the user what would go. The fix is to split the two phases
 * across a confirm dialog: `runDryRun` (preview) is rendered, the user reads
 * the real paths, and only then does `runConfirmedDelete` execute.
 *
 * `needs_confirm` (all-agents / symlink-layout removal) is NOT a blocker here:
 * the confirm dialog IS that confirmation, so the confirmed phase always
 * proceeds — it must never throw on it.
 */
export async function runDryRun(deleteFn: DeleteFn): Promise<RemovalView> {
	const preview = await deleteFn(false);
	if (preview.success === false) {
		throw new Error(preview.error ?? "Delete preview failed");
	}
	return preview;
}

export async function runConfirmedDelete(
	deleteFn: DeleteFn,
): Promise<RemovalView> {
	const result = await deleteFn(true);
	if (result.success === false) {
		throw new Error(result.error ?? "Delete failed");
	}
	return result;
}

export interface DeletePreview {
	/** Paths that WOULD be removed, aggregated + de-duped across all items. */
	paths: string[];
	/** Paths intentionally skipped (outside allow-listed roots), de-duped. */
	skipped: string[];
}

function aggregate(views: RemovalView[]): DeletePreview {
	const paths = new Set<string>();
	const skipped = new Set<string>();
	for (const v of views) {
		for (const p of v.paths) paths.add(p);
		for (const s of v.skipped) skipped.add(s);
	}
	return { paths: [...paths], skipped: [...skipped] };
}

/**
 * Preview phase of the gate: dry-run every item (confirm=false) and aggregate
 * the real paths. Rejects if ANY dry-run fails — the caller must then NOT reach
 * the confirm phase. This is the seam a destructive call site must go through
 * instead of jumping straight to confirm=true.
 */
export async function previewAll(fns: DeleteFn[]): Promise<DeletePreview> {
	const views = await Promise.all(fns.map(runDryRun));
	return aggregate(views);
}

/**
 * Confirm phase of the gate: execute every item (confirm=true). Resolves with
 * the indices that failed (empty = all succeeded) so callers can name what
 * didn't go, instead of throwing on the first failure.
 */
export async function confirmAll(fns: DeleteFn[]): Promise<number[]> {
	const results = await Promise.allSettled(fns.map(runConfirmedDelete));
	return results
		.map((r, i) => (r.status === "rejected" ? i : -1))
		.filter((i) => i >= 0);
}

export type PreviewState =
	| { status: "idle" }
	| { status: "loading" }
	| { status: "ready"; preview: DeletePreview }
	| { status: "error"; message: string };

/**
 * The confirm-button gate: confirm=true may run ONLY after the preview reached
 * "ready" (a successful dry-run) and no delete is already in flight. Pure so the
 * ordering invariant — confirm can never precede a successful preview — is
 * tested without a renderer; `DeletePreviewDialog` binds its danger button's
 * `isDisabled` to `!canConfirm(...)`.
 */
export function canConfirm(state: PreviewState, isDeleting: boolean): boolean {
	return state.status === "ready" && !isDeleting;
}

/**
 * Resolve the confirm phase's outcome from the failed-item indices `confirmAll`
 * returned. Pure so the branch — onFailed fires (and onConfirmed does NOT) when
 * any delete failed, else onConfirmed runs — is tested without a renderer.
 */
export function confirmOutcome(
	failedIndices: number[],
): { ok: true } | { ok: false; failed: number[] } {
	return failedIndices.length > 0
		? { ok: false, failed: failedIndices }
		: { ok: true };
}

export interface ConfirmGateCallbacks {
	/** Fired after every confirmed delete succeeds. */
	onConfirmed: () => void | Promise<void>;
	/** Fired with the failed-item indices when one or more deletes fail. */
	onFailed?: (failedIndices: number[]) => void;
	/** Closes the dialog after the confirm phase resolves either way. */
	onClose: () => void;
}

/**
 * The destructive executor: runs every confirm=true delete and returns the
 * failed-item indices. The hook's `confirm` is exactly this wrapped in
 * in-flight tracking; the gate takes it as a parameter so a test can pass a
 * spy and assert the gate never invokes it on a non-ready state.
 */
export type ConfirmExecutor = (fns: DeleteFn[]) => Promise<number[]>;

/**
 * The dialog's confirm-button handler, extracted pure so the open->confirm
 * ordering contract is unit-tested without a renderer. This is the seam the #5
 * bug violated: it MUST refuse to invoke the destructive executor (confirm=true)
 * unless the preview reached "ready" (a successful dry-run) and no delete is in
 * flight — the same `canConfirm` gate the danger button binds to. A call site
 * that jumps straight to delete(confirm=true) without a preview is exactly this
 * returning before `runConfirm` is reached.
 *
 * Returns `true` if it ran the destructive phase, `false` if the gate blocked
 * it — so a test can prove confirm=true never fires on a non-ready state.
 */
export async function confirmDelete(
	state: PreviewState,
	isDeleting: boolean,
	fns: DeleteFn[],
	runConfirm: ConfirmExecutor,
	cb: ConfirmGateCallbacks,
): Promise<boolean> {
	if (!canConfirm(state, isDeleting)) return false;
	const outcome = confirmOutcome(await runConfirm(fns));
	if (!outcome.ok) {
		cb.onFailed?.(outcome.failed);
	} else {
		await cb.onConfirmed();
	}
	cb.onClose();
	return true;
}

/**
 * Drives the two-step delete for one set of items. Call `load(fns)` when the
 * confirm dialog opens to run the dry-runs and surface the real paths; call
 * `confirm(fns)` after the user clicks delete to actually remove them.
 *
 * Returns per-item failures from the confirm phase instead of throwing, so the
 * caller can render a precise toast.
 */
export function useDeletePreview() {
	const [state, setState] = useState<PreviewState>({ status: "idle" });
	const [isDeleting, setIsDeleting] = useState(false);

	const load = useCallback(async (fns: DeleteFn[]) => {
		setState({ status: "loading" });
		try {
			setState({ status: "ready", preview: await previewAll(fns) });
		} catch (error) {
			setState({
				status: "error",
				message:
					error instanceof Error
						? error.message
						: "Delete preview failed",
			});
		}
	}, []);

	const reset = useCallback(() => {
		setState({ status: "idle" });
		setIsDeleting(false);
	}, []);

	/**
	 * Runs the confirmed delete for each fn. Resolves with the indices that
	 * failed (empty = all succeeded) so callers can name what didn't go.
	 */
	const confirm = useCallback(async (fns: DeleteFn[]): Promise<number[]> => {
		setIsDeleting(true);
		try {
			return await confirmAll(fns);
		} finally {
			setIsDeleting(false);
		}
	}, []);

	return { state, isDeleting, load, confirm, reset };
}
