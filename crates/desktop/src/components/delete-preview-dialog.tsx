import { AlertDialog, Button, Spinner } from "@heroui/react";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import {
	canConfirm,
	confirmOutcome,
	type DeleteFn,
	useDeletePreview,
} from "../lib/delete-preview";

export interface DeletePreviewDialogProps {
	isOpen: boolean;
	onClose: () => void;
	/**
	 * One bound delete per item/agent/scope being removed. The dialog runs all
	 * of them as dry-runs on open (the preview) and as confirmed deletes only
	 * after the user clicks the danger button.
	 */
	deleteFns: DeleteFn[];
	heading: string;
	/** Short sentence above the path list, e.g. "Delete X? This can't be undone." */
	description: string;
	confirmLabel: string;
	/** Fired after every confirmed delete succeeds. */
	onConfirmed: () => void | Promise<void>;
	/** Fired with the failed-item indices when one or more deletes fail. */
	onFailed?: (failedIndices: number[]) => void;
}

/**
 * Shared two-step destructive-delete confirm for skills / MCPs / sub-agents.
 *
 * 1. On open, runs the server dry-run (confirm=false) for each item and renders
 *    the EXACT paths (and skipped paths) it would remove — the real preview.
 * 2. Only after the user confirms does it run confirm=true and actually delete.
 *
 * This is the fix for #5: confirm=true is never reached without showing the
 * preview, and `needs_confirm` is the normal path here, never an error.
 */
export function DeletePreviewDialog({
	isOpen,
	onClose,
	deleteFns,
	heading,
	description,
	confirmLabel,
	onConfirmed,
	onFailed,
}: DeletePreviewDialogProps) {
	const { t } = useTranslation();
	const { state, isDeleting, load, confirm, reset } = useDeletePreview();

	// Run the server dry-run preview when the dialog opens (and when the target
	// set changes while open); reset when it closes. An effect is the right tool
	// here: it syncs an imperative server call to the open/close lifecycle, not
	// React state. `deleteFns` is memoized by every caller, so its identity is
	// stable and safe to depend on.
	useEffect(() => {
		if (isOpen) {
			void load(deleteFns);
		} else {
			reset();
		}
	}, [isOpen, deleteFns, load, reset]);

	const handleConfirm = async () => {
		const outcome = confirmOutcome(await confirm(deleteFns));
		if (!outcome.ok) {
			onFailed?.(outcome.failed);
			onClose();
			return;
		}
		await onConfirmed();
		onClose();
	};

	const ready = state.status === "ready";

	return (
		<AlertDialog.Backdrop isOpen={isOpen} onOpenChange={onClose}>
			<AlertDialog.Container>
				<AlertDialog.Dialog className="sm:max-w-[480px]">
					<AlertDialog.CloseTrigger />
					<AlertDialog.Header>
						<AlertDialog.Icon status="danger" />
						<AlertDialog.Heading>{heading}</AlertDialog.Heading>
					</AlertDialog.Header>
					<AlertDialog.Body>
						<p className="text-sm text-muted">{description}</p>

						{state.status === "loading" && (
							<div className="mt-4 flex items-center gap-2 text-sm text-muted">
								<Spinner size="sm" color="current" />
								{t("deletePreviewLoading")}
							</div>
						)}

						{state.status === "error" && (
							<p className="mt-4 text-sm text-danger">
								{state.message}
							</p>
						)}

						{ready && (
							<div className="mt-4 space-y-3">
								<div>
									<h4 className="mb-2 text-xs font-medium tracking-wide text-muted uppercase">
										{t("deletePreviewPaths", {
											count: state.preview.paths.length,
										})}
									</h4>
									{state.preview.paths.length > 0 ? (
										<ul className="max-h-48 space-y-1 overflow-y-auto rounded-lg bg-surface-secondary px-3 py-2">
											{state.preview.paths.map((p) => (
												<li
													key={p}
													className="font-mono text-xs break-all text-foreground"
												>
													{p}
												</li>
											))}
										</ul>
									) : (
										<p className="text-xs text-muted">
											{t("deletePreviewNothing")}
										</p>
									)}
								</div>

								{state.preview.skipped.length > 0 && (
									<div>
										<h4 className="mb-2 text-xs font-medium tracking-wide text-muted uppercase">
											{t("deletePreviewSkipped", {
												count: state.preview.skipped
													.length,
											})}
										</h4>
										<ul className="max-h-32 space-y-1 overflow-y-auto rounded-lg bg-surface-secondary px-3 py-2">
											{state.preview.skipped.map((p) => (
												<li
													key={p}
													className="font-mono text-xs break-all text-muted"
												>
													{p}
												</li>
											))}
										</ul>
									</div>
								)}
							</div>
						)}
					</AlertDialog.Body>
					<AlertDialog.Footer>
						<Button
							slot="close"
							variant="tertiary"
							onPress={onClose}
							isDisabled={isDeleting}
						>
							{t("cancel")}
						</Button>
						<Button
							variant="danger"
							onPress={handleConfirm}
							isDisabled={!canConfirm(state, isDeleting)}
							className="min-w-[120px]"
						>
							{isDeleting ? (
								<>
									<Spinner
										size="sm"
										color="current"
										className="mr-2"
									/>
									{t("deleting")}
								</>
							) : (
								confirmLabel
							)}
						</Button>
					</AlertDialog.Footer>
				</AlertDialog.Dialog>
			</AlertDialog.Container>
		</AlertDialog.Backdrop>
	);
}
