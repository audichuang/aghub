import type { ReactNode } from "react";
import { createContext } from "react";
import type { AgentAvailabilityDto, AgentInfo } from "../generated/dto";

export interface AvailableAgent extends AgentInfo {
	availability: AgentAvailabilityDto;
	isDisabled: boolean;
	isUsable: boolean;
}

export interface AgentAvailabilityContextValue {
	availableAgents: AvailableAgent[];
	allAgents: AgentInfo[];
	isLoading: boolean;
	/** Both server queries SUCCEEDED. On failure the provider still renders
	 * children with EMPTY arrays, which reads as "this machine has no agents" —
	 * fine for a list, fatal for anything that records that as a fact. */
	agentsReady: boolean;
	refetch: () => void;
	refreshDisabledAgents: () => Promise<void>;
}

export const AgentAvailabilityContext =
	createContext<AgentAvailabilityContextValue | null>(null);

export interface AgentAvailabilityProviderProps {
	children: ReactNode;
}
