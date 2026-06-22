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
import { queryKeys } from "./keys";

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
	await queryClient.invalidateQueries({
		queryKey: queryKeys.skills.all(),
	});
	await Promise.all([
		queryClient.refetchQueries({
			queryKey: queryKeys.skills.lists(),
			type: "active",
		}),
		queryClient.refetchQueries({
			queryKey: queryKeys.skills.lock.all(),
			type: "active",
		}),
	]);
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
			await onSuccess?.(data);
		},
	});
}
