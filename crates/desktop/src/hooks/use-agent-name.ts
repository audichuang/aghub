import { useCallback } from "react";
import { formatAgentName } from "../lib/utils";
import { useAgentAvailability } from "./use-agent-availability";

/**
 * Resolve an agent id to the roster's display_name (e.g. "omp" -> "Oh My Pi").
 * Falls back to the id-derived label for ids the roster does not carry, such as
 * the "default" sentinel used for agent-less entries.
 */
export function useAgentName(): (agentId: string) => string {
	const { allAgents } = useAgentAvailability();
	return useCallback(
		(agentId: string) =>
			allAgents.find((agent) => agent.id === agentId)?.display_name ??
			formatAgentName(agentId),
		[allAgents],
	);
}
