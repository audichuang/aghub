/**
 * Re-exporting from requests/delete-skill where delete helpers and backend
 * classification logic have been centralized.
 */
export {
	splitDeleteTargets,
	buildBulkDeleteRequests,
	collectUnmanagedDeleteTargets,
	type DeleteTargetItem,
	type DeleteTargets,
	type BulkDeleteItem,
	type BulkDeleteGroup,
	type BulkDeleteRequest,
	type BuildBulkDeleteRequestsOptions,
	type BuildBulkDeleteRequestsResult,
	type BackendHolders,
} from "../requests/delete-skill.ts";
