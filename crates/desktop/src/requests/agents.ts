import { queryOptions, useQuery } from "@tanstack/react-query";
import type { AgentSkillCoverageDto } from "../generated/dto";
import type { AgentScope } from "../lib/agent-capabilities";
import { useApi } from "../hooks/use-api";
import type { ApiClient } from "./client";
import { queryKeys } from "./keys";

interface AgentsQueryParams {
	api: ApiClient;
}

export function agentsListQueryOptions({ api }: AgentsQueryParams) {
	return queryOptions({
		queryKey: queryKeys.agents.list(),
		queryFn: () => api.agents.list(),
	});
}

export function agentAvailabilityQueryOptions({ api }: AgentsQueryParams) {
	return queryOptions({
		queryKey: queryKeys.agents.availability(),
		queryFn: () => api.agents.availability(),
	});
}

export function agentSkillCoverageQueryOptions({
	api,
	scope,
	projectRoot,
}: {
	api: ApiClient;
	scope: AgentScope;
	projectRoot?: string | null;
}) {
	return queryOptions({
		queryKey: queryKeys.agents.coverage(scope, projectRoot),
		queryFn: () => api.agents.skillCoverage(scope, projectRoot ?? null),
	});
}

/**
 * Coverage DTOs for the active scope, keyed by agent id. Re-queries on scope
 * change (the global vs project NativeReader sets differ -- see the classifier).
 */
export function useSkillCoverage(
	scope: AgentScope,
	projectRoot?: string | null,
): {
	coverage: Record<string, AgentSkillCoverageDto>;
	isLoading: boolean;
	/** The query SUCCEEDED. A failed one yields an empty `coverage`, which is
	 * indistinguishable from "no agent needs a link" — anything that persists a
	 * decision from coverage must gate on this, not on `!isLoading`. */
	isSuccess: boolean;
} {
	const api = useApi();
	const { data, isLoading, isSuccess } = useQuery(
		agentSkillCoverageQueryOptions({ api, scope, projectRoot }),
	);
	const coverage: Record<string, AgentSkillCoverageDto> = {};
	for (const entry of data ?? []) coverage[entry.id] = entry;
	return { coverage, isLoading, isSuccess };
}
