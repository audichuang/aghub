import { queryOptions } from "@tanstack/react-query";
import type { SourceDiffResponse, SourcesListResponse } from "../generated/dto";
import type { GitForwardHeaders } from "../lib/api";
import type { ApiClient } from "./client";
import { queryKeys } from "./keys";

/**
 * Resolve the per-request forward header for a single known source, or
 * undefined when forwarding is not engaged. Supplied by `useGitForwarding`.
 * Resolved transiently inside the queryFn so the token never enters the cache.
 */
export type ForwardForSource = (
	source: string,
) => Promise<GitForwardHeaders | undefined>;

interface SourcesListQueryParams {
	api: ApiClient;
	scope?: "global" | "project" | "all";
	projectRoot?: string;
	enabled?: boolean;
	staleTime?: number;
}

/** List the installed skill sources in a scope (offline, lock-only). */
export function sourcesListQueryOptions({
	api,
	scope = "global",
	projectRoot,
	enabled = true,
	staleTime = 30_000,
}: SourcesListQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.sources.list(scope, projectRoot),
		queryFn: (): Promise<SourcesListResponse> =>
			api.skills.getSources(scope, projectRoot),
		enabled,
		staleTime,
	});
}

interface SourceDiffQueryParams {
	api: ApiClient;
	source: string;
	scope?: "global" | "project" | "all";
	projectRoot?: string;
	gitRef?: string;
	enabled?: boolean;
	staleTime?: number;
	/**
	 * Optional forward-header resolver (remote mode). Omitted in Local mode.
	 * NOT part of the query key — the token is resolved transiently and must
	 * never be cached or keyed on.
	 */
	forwardForSource?: ForwardForSource;
}

/** Fetch a single source and diff each skill (3-state). Network-heavy. */
export function sourceDiffQueryOptions({
	api,
	source,
	scope = "global",
	projectRoot,
	gitRef,
	enabled = true,
	staleTime = 60_000,
	forwardForSource,
}: SourceDiffQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.sources.diff(
			source,
			scope,
			projectRoot,
			gitRef,
		),
		queryFn: async (): Promise<SourceDiffResponse> => {
			const headers = await forwardForSource?.(source);
			return api.skills.diffSource(
				{ scope, projectRoot, source, gitRef },
				headers,
			);
		},
		enabled: enabled && Boolean(source),
		staleTime,
	});
}
