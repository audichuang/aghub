import {
	mutationOptions,
	type QueryClient,
	queryOptions,
} from "@tanstack/react-query";
import type {
	CreateSkillRequest,
	DeleteSkillByPathRequest,
	ApplySkillUpdateRequest,
	ApplySkillUpdateResponse,
	ApplySkillUpdatesRequest,
	ApplySkillUpdatesResponse,
	GitInstallRequest,
	GitInstallResponse,
	GitScanRequest,
	GitSyncRequest,
	GitSyncResponse,
	ImportSkillRequest,
	InstallSkillRequest,
	InstallSkillResponse,
	OperationBatchResponse,
	ReconcileRequest,
	SkillResponse,
	SkillUpdateResponse,
	TransferRequest,
} from "../generated/dto";
import type { GitForwardHeaders } from "../lib/api";
import type { ApiClient } from "./client";
import { queryKeys } from "./keys.ts";

/**
 * Resolve the per-request forward header for a known single source, or
 * undefined when forwarding is not engaged. Supplied by `useGitForwarding`.
 * Resolved transiently inside the mutationFn so the token never enters the
 * cache, a query key, or a stored mutation variable.
 */
export type ForwardForSource = (
	source: string,
) => Promise<GitForwardHeaders | undefined>;

/**
 * Resolve the per-request forward header for `check-updates` (bound sources),
 * or undefined when forwarding is not engaged. Supplied by `useGitForwarding`.
 */
export type ForwardForBoundSources = () => Promise<
	GitForwardHeaders | undefined
>;

interface SkillListQueryParams {
	api: ApiClient;
	scope?: "global" | "project" | "all";
	projectRoot?: string;
	enabled?: boolean;
	staleTime?: number;
}

export function skillListQueryOptions({
	api,
	scope = "global",
	projectRoot,
	enabled = true,
	staleTime = 30_000,
}: SkillListQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.list(scope, projectRoot),
		queryFn: () => api.skills.listAll(scope, projectRoot),
		enabled,
		staleTime,
	});
}

interface SkillUsageQueryParams {
	api: ApiClient;
	enabled?: boolean;
	staleTime?: number;
}

/**
 * Usage counts for the installed global Claude skills, least-used first.
 * Reads Claude Code's own `skillUsage` counter; never-dispatched skills report
 * `usage_count: 0`. Claude-only.
 */
export function skillUsageQueryOptions({
	api,
	enabled = true,
	staleTime = 30_000,
}: SkillUsageQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.usage(),
		queryFn: () => api.skills.usage(),
		enabled,
		staleTime,
	});
}

interface GlobalSkillLockQueryParams {
	api: ApiClient;
	enabled?: boolean;
	staleTime?: number;
}

export function globalSkillLockQueryOptions({
	api,
	enabled = true,
	staleTime = 30_000,
}: GlobalSkillLockQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.lock.global(),
		queryFn: () => api.skills.getGlobalLock(),
		enabled,
		staleTime,
	});
}

interface ProjectSkillLockQueryParams {
	api: ApiClient;
	projectPath?: string;
	enabled?: boolean;
	staleTime?: number;
}

export function projectSkillLockQueryOptions({
	api,
	projectPath,
	enabled = true,
	staleTime = 30_000,
}: ProjectSkillLockQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.lock.project(projectPath),
		queryFn: () => api.skills.getProjectLock(projectPath),
		enabled,
		staleTime,
	});
}

interface SkillPathQueryParams {
	api: ApiClient;
	path?: string;
	scope?: "global" | "project";
	projectRoot?: string;
	enabled?: boolean;
	staleTime?: number;
}

export function skillContentQueryOptions({
	api,
	path,
	scope = "global",
	projectRoot,
	enabled = true,
	staleTime = 60_000,
}: SkillPathQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.content(path ?? "", scope, projectRoot),
		queryFn: () => api.skills.getContent(path!, scope, projectRoot),
		enabled: enabled && Boolean(path),
		staleTime,
	});
}

export function skillTreeQueryOptions({
	api,
	path,
	scope = "global",
	projectRoot,
	enabled = true,
	staleTime = 60_000,
}: SkillPathQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.tree(path ?? "", scope, projectRoot),
		queryFn: () => api.skills.getTree(path!, scope, projectRoot),
		enabled: enabled && Boolean(path),
		staleTime,
	});
}

export async function invalidateSkillQueries(queryClient: QueryClient) {
	// Mark all skill queries stale, but do NOT block on refetching here.
	// `skills.all()` also covers the network-heavy source-diff queries; awaiting
	// their refetch means that if ANY source is stuck pending (e.g. an
	// uncheckable / slow / offline source — the "無法檢查" rows), the mutation
	// that called this (reconcile / transfer / create / delete …) hangs forever
	// and its dialog sticks on "套用中…". So: broad-invalidate with
	// refetchType:"none" (stale only), and fire-and-forget the lightweight
	// list/lock refetches so the mutation resolves immediately.
	await queryClient.invalidateQueries({
		queryKey: queryKeys.skills.all(),
		refetchType: "none",
	});
	void queryClient.refetchQueries({
		queryKey: queryKeys.skills.lists(),
		type: "active",
	});
	void queryClient.refetchQueries({
		queryKey: queryKeys.skills.lock.all(),
		type: "active",
	});
}

/// An applied update changes whether a skill IS outdated, and the badge that
/// says so is fed by a key prefix `invalidateSkillQueries` deliberately only
/// marks stale — so without this the list keeps offering "Update available" for
/// skills that were just updated (nothing else clears it: no
/// refetch-on-window-focus, and the page's observer never unmounts).
///
/// Only the apply-update flows call it. An update check re-fetches EVERY source
/// over the network, which is not a price a create/delete/transfer should pay.
/// Fire-and-forget for the same reason `invalidateSkillQueries` is: a stuck or
/// offline source must not hang the mutation that triggered it.
function refetchUpdateChecks(queryClient: QueryClient) {
	void queryClient.refetchQueries(
		{ queryKey: queryKeys.skills.updateChecksAll(), type: "active" },
		// Default `true` would ORPHAN an in-flight check and start another —
		// and the queryFn ignores the abort signal, so the server keeps working
		// on the orphan. Updating several skills in a row would then run one
		// full every-source check per click instead of sharing one.
		{ cancelRefetch: false },
	);
}

/// A flow that REWRITES a skill's files (apply-update, sync) leaves the open
/// detail panel showing pre-update text: the skill path is unchanged, so the
/// content/tree keys are unchanged, and `invalidateSkillQueries` only marks
/// them stale (`refetchType: "none"`). Nothing else refetches them — the panel
/// does not remount and window-focus refetching is off — so the user sees a
/// success toast over stale bytes. Fire-and-forget for the same reason the
/// other refetch helpers are.
function refetchSkillBodies(queryClient: QueryClient) {
	for (const key of [queryKeys.skills.contents(), queryKeys.skills.trees()]) {
		void queryClient.refetchQueries({ queryKey: key, type: "active" });
	}
}

interface CreateSkillVariables {
	agent: string;
	body: CreateSkillRequest;
	projectPath?: string;
}

interface CreateSkillMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (
		data: SkillResponse,
		variables: CreateSkillVariables,
	) => void | Promise<void>;
}

export function createSkillMutationOptions({
	api,
	queryClient,
	onSuccess,
}: CreateSkillMutationParams) {
	return mutationOptions({
		mutationFn: ({ agent, body, projectPath }: CreateSkillVariables) =>
			api.skills.create(agent, body, projectPath),
		onSuccess: async (data, variables) => {
			await invalidateSkillQueries(queryClient);
			await onSuccess?.(data, variables);
		},
	});
}

interface ImportSkillVariables {
	agent: string;
	body: ImportSkillRequest;
	projectPath?: string;
}

interface ImportSkillMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (
		data: SkillResponse,
		variables: ImportSkillVariables,
	) => void | Promise<void>;
}

export function importSkillMutationOptions({
	api,
	queryClient,
	onSuccess,
}: ImportSkillMutationParams) {
	return mutationOptions({
		mutationFn: ({ agent, body, projectPath }: ImportSkillVariables) =>
			api.skills.import(agent, body, projectPath),
		onSuccess: async (data, variables) => {
			await invalidateSkillQueries(queryClient);
			await onSuccess?.(data, variables);
		},
	});
}

interface InstallSkillMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: InstallSkillResponse) => void | Promise<void>;
}

export function installSkillMutationOptions({
	api,
	queryClient,
	onSuccess,
}: InstallSkillMutationParams) {
	return mutationOptions({
		mutationFn: (body: InstallSkillRequest) => api.skills.install(body),
		onSuccess: async (data) => {
			await invalidateSkillQueries(queryClient);
			await onSuccess?.(data);
		},
	});
}

interface DeleteSkillByPathMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: () => void | Promise<void>;
}

export function deleteSkillByPathMutationOptions({
	api,
	queryClient,
	onSuccess,
}: DeleteSkillByPathMutationParams) {
	return mutationOptions({
		mutationFn: async (body: DeleteSkillByPathRequest) => {
			const result = await api.skills.deleteByPath(body);

			if (!result.success) {
				throw new Error(result.error || "Failed to delete skill");
			}

			return result;
		},
		onSuccess: async () => {
			await invalidateSkillQueries(queryClient);
			await onSuccess?.();
		},
	});
}

interface ReconcileSkillsMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: OperationBatchResponse) => void | Promise<void>;
}

export function reconcileSkillsMutationOptions({
	api,
	queryClient,
	onSuccess,
}: ReconcileSkillsMutationParams) {
	return mutationOptions({
		mutationFn: (body: ReconcileRequest) => api.skills.reconcile(body),
		onSuccess: async (data) => {
			await invalidateSkillQueries(queryClient);
			await onSuccess?.(data);
		},
	});
}

interface TransferSkillsMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: OperationBatchResponse) => void | Promise<void>;
}

export function transferSkillsMutationOptions({
	api,
	queryClient,
	onSuccess,
}: TransferSkillsMutationParams) {
	return mutationOptions({
		mutationFn: (body: TransferRequest) => api.skills.transfer(body),
		onSuccess: async (data) => {
			await invalidateSkillQueries(queryClient);
			await onSuccess?.(data);
		},
	});
}

export function gitScanSkillsMutationOptions({
	api,
	forwardForSource,
}: {
	api: ApiClient;
	forwardForSource?: ForwardForSource;
}) {
	return mutationOptions({
		mutationFn: async (body: GitScanRequest) => {
			// The scan body carries the clone URL — resolve+forward against it.
			const headers = await forwardForSource?.(body.url);
			return api.skills.gitScan(body, headers);
		},
	});
}

interface GitInstallSkillsMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: GitInstallResponse) => void | Promise<void>;
}

/**
 * Variables for the git-install mutation. The install body carries only a
 * `session_id`; the server reuses the scan session's cached token, so install
 * does NOT forward a git-credential header.
 */
export interface GitInstallSkillsVariables {
	body: GitInstallRequest;
}

export function gitInstallSkillsMutationOptions({
	api,
	queryClient,
	onSuccess,
}: GitInstallSkillsMutationParams) {
	return mutationOptions({
		mutationFn: async ({ body }: GitInstallSkillsVariables) =>
			api.skills.gitInstall(body),
		onSuccess: async (data) => {
			await invalidateSkillQueries(queryClient);
			await onSuccess?.(data);
		},
	});
}

export function openSkillFolderMutationOptions({ api }: { api: ApiClient }) {
	return mutationOptions({
		mutationFn: (skillPath: string) => api.skills.openFolder(skillPath),
	});
}

interface CheckSkillUpdatesMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: SkillUpdateResponse[]) => void | Promise<void>;
	onError?: (error: Error) => void;
	/**
	 * Resolver for the bound-source forward header (remote mode). `check-updates`
	 * spans many sources, so the FE enumerates explicitly-bound sources and
	 * forwards a token per bound source. Omitted in Local mode.
	 */
	forwardForBoundSources?: ForwardForBoundSources;
}

export interface CheckSkillUpdatesParams {
	offline?: boolean;
	scope?: "global" | "project" | "all";
	projectRoot?: string;
}

export function checkSkillUpdatesQueryKey(params?: CheckSkillUpdatesParams) {
	return queryKeys.skills.updateChecks(
		params?.scope ?? "global",
		params?.projectRoot,
	);
}

/// Modeled as a mutation (not a query) because it is an explicit, network-heavy
/// user action (clones each source) — this gives clean onSuccess/onError hooks
/// without a fetch-on-render effect. The global skill lock is the input scope.
export function checkSkillUpdatesMutationOptions({
	api,
	queryClient,
	onSuccess,
	onError,
	forwardForBoundSources,
}: CheckSkillUpdatesMutationParams) {
	return mutationOptions({
		mutationFn: async (params?: CheckSkillUpdatesParams) => {
			const headers = await forwardForBoundSources?.();
			return api.skills.checkUpdates(params, headers);
		},
		onSuccess: async (data, variables) => {
			queryClient.setQueryData(
				checkSkillUpdatesQueryKey(variables),
				data,
			);
			await onSuccess?.(data);
		},
		onError,
	});
}

interface CheckSkillUpdatesQueryParams {
	api: ApiClient;
	/** When false the query does not fire (use to suppress when offline). */
	enabled?: boolean;
	params?: CheckSkillUpdatesParams;
	/** Bound-source forward-header resolver (remote mode). Omitted locally. */
	forwardForBoundSources?: ForwardForBoundSources;
	/**
	 * Throttle threshold in ms. Default 10 minutes — matches the spec's
	 * "staleTime = throttle" pattern so React Query skips a re-fetch if the
	 * last result is younger than this.
	 *
	 * §12-C1: preflight is near-zero-cost only in steady-state (ref_commit
	 * populated, local not drifted). Throttle + offline suppression are
	 * REQUIRED, not optional.
	 */
	staleTime?: number;
}

/**
 * `useQuery`-compatible options for `GET /skills/check-updates`.
 *
 * Use this for the **auto-check-on-page-enter** path. The mutation variant
 * (`checkSkillUpdatesMutationOptions`) is kept for the manual refresh
 * button where explicit loading state matters.
 *
 * The check writes back to the skill lock (auto-heals ref_commit/hash) —
 * this side-effect is accepted per spec §4.3.
 */
export function checkSkillUpdatesQueryOptions({
	api,
	enabled = true,
	params,
	staleTime = 600_000, // 10 minutes
	forwardForBoundSources,
}: CheckSkillUpdatesQueryParams) {
	return queryOptions({
		queryKey: checkSkillUpdatesQueryKey(params),
		queryFn: async () => {
			const headers = await forwardForBoundSources?.();
			return api.skills.checkUpdates(params, headers);
		},
		enabled,
		staleTime,
	});
}

interface ApplySkillUpdateMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: ApplySkillUpdateResponse) => void | Promise<void>;
	onError?: (error: Error) => void;
	/**
	 * Resolver for the single-source forward header (remote mode). apply-update
	 * re-fetches the source server-side, so the controller-resolved token must
	 * reach the remote — keyed by the source's clone URL (P1-c). Omitted locally.
	 */
	forwardForSource?: ForwardForSource;
}

/**
 * Variables for the apply-update mutation. `sourceUrl` (optional) is the clone
 * URL of the source being applied — threaded only to resolve the forward header
 * (the apply body carries the skill name/scope, not the URL). Not persisted.
 */
export interface ApplySkillUpdateVariables {
	body: ApplySkillUpdateRequest;
	sourceUrl?: string;
}

export function applySkillUpdateMutationOptions({
	api,
	queryClient,
	onSuccess,
	onError,
	forwardForSource,
}: ApplySkillUpdateMutationParams) {
	return mutationOptions({
		mutationFn: async ({ body, sourceUrl }: ApplySkillUpdateVariables) => {
			const headers = sourceUrl
				? await forwardForSource?.(sourceUrl)
				: undefined;
			return api.skills.applyUpdate(body, headers);
		},
		onSuccess: async (data) => {
			await invalidateSkillQueries(queryClient);
			refetchUpdateChecks(queryClient);
			refetchSkillBodies(queryClient);
			await onSuccess?.(data);
		},
		onError,
	});
}

interface ApplySkillUpdatesMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: ApplySkillUpdatesResponse) => void | Promise<void>;
	onError?: (error: Error) => void;
	forwardForSource?: ForwardForSource;
}

export interface ApplySkillUpdatesVariables {
	body: ApplySkillUpdatesRequest;
	sourceUrl?: string;
}

interface ApplySkillUpdatesApi {
	skills: Pick<ApiClient["skills"], "applyUpdates">;
}

/** Resolve forwarding once and issue exactly one batch HTTP request. */
export async function applySkillUpdatesRequest(
	api: ApplySkillUpdatesApi,
	forwardForSource: ForwardForSource | undefined,
	{ body, sourceUrl }: ApplySkillUpdatesVariables,
) {
	const headers = sourceUrl ? await forwardForSource?.(sourceUrl) : undefined;
	return api.skills.applyUpdates(body, headers);
}

export function applySkillUpdatesMutationOptions({
	api,
	queryClient,
	onSuccess,
	onError,
	forwardForSource,
}: ApplySkillUpdatesMutationParams) {
	return mutationOptions({
		mutationFn: (variables: ApplySkillUpdatesVariables) =>
			applySkillUpdatesRequest(api, forwardForSource, variables),
		onSuccess: async (data) => {
			await invalidateSkillQueries(queryClient);
			refetchUpdateChecks(queryClient);
			refetchSkillBodies(queryClient);
			await onSuccess?.(data);
		},
		onError,
	});
}

interface GitSyncSkillMutationParams {
	api: ApiClient;
	queryClient: QueryClient;
	onSuccess?: (data: GitSyncResponse) => void | Promise<void>;
}

/**
 * Variables for the git-sync mutation. The sync body carries a `session_id`;
 * the server reuses the scan session's cached token, so sync does NOT forward a
 * git-credential header.
 */
export interface GitSyncSkillVariables {
	body: GitSyncRequest;
}

export function gitSyncSkillMutationOptions({
	api,
	queryClient,
	onSuccess,
}: GitSyncSkillMutationParams) {
	return mutationOptions({
		mutationFn: async ({ body }: GitSyncSkillVariables) =>
			api.skills.gitSync(body),
		onSuccess: async (data) => {
			await invalidateSkillQueries(queryClient);
			// git-sync runs the same resync transaction as apply-update, so it
			// flips a skill from outdated to current and owes the same refetch.
			refetchUpdateChecks(queryClient);
			refetchSkillBodies(queryClient);
			await onSuccess?.(data);
		},
	});
}
