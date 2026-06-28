import type { DeleteSkillByPathResponse } from "../generated/dto";

/**
 * Two-phase delete: run the server dry-run (confirm=false) first so a failed
 * preview short-circuits before anything destructive, then execute the real
 * delete with confirm=true.
 *
 * Task 15 (#5): MCP / sub-agent / skill deletes must NOT auto-confirm in one
 * call. `deleteFn` is the api.*.delete bound to everything except `confirm`, so
 * the same closure runs both phases.
 *
 * `needs_confirm` (all-agents / symlink-layout skill removal) is NOT an unmet
 * gate here: every caller invokes this only from a user-facing confirm dialog,
 * so the confirm=true second phase IS that confirmation. We must proceed, not
 * abandon the delete — throwing on it broke the all-agents source flows.
 */
export async function deleteWithDryRun(
	deleteFn: (confirm: boolean) => Promise<DeleteSkillByPathResponse>,
): Promise<DeleteSkillByPathResponse> {
	const preview = await deleteFn(false);
	if (preview.success === false) {
		throw new Error(preview.error ?? "Delete preview failed");
	}
	return deleteFn(true);
}
