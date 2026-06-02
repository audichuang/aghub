import { queryOptions } from "@tanstack/react-query";
import type { SourceDiffResponse, SourcesListResponse } from "../generated/dto";
import type { ApiClient } from "./client";
import { queryKeys } from "./keys";

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
}: SourceDiffQueryParams) {
	return queryOptions({
		queryKey: queryKeys.skills.sources.diff(
			source,
			scope,
			projectRoot,
			gitRef,
		),
		queryFn: (): Promise<SourceDiffResponse> =>
			api.skills.diffSource({ scope, projectRoot, source, gitRef }),
		enabled: enabled && Boolean(source),
		staleTime,
	});
}
