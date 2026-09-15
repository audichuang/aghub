import { getStore } from ".";
import {
	disabledAgentsKey,
	LOCAL_DISABLED_AGENTS_KEY,
	resolveDisabledAgents,
} from "./disabled-agents.ts";

/**
 * The agent selection for one connection.
 *
 * The inheritance rule and the `undefined`-vs-`[]` trap live with
 * `resolveDisabledAgents`, which is where they are tested.
 */
export async function getDisabledAgents(
	connectionId: string,
): Promise<string[]> {
	const store = await getStore();
	const own = await store.get<string[]>(disabledAgentsKey(connectionId));
	// Local's own key IS the Local key — no second read, and no fallback loop.
	const local =
		connectionId === "local"
			? own
			: await store.get<string[]>(LOCAL_DISABLED_AGENTS_KEY);
	return resolveDisabledAgents(own, local);
}

async function setDisabledAgents(
	connectionId: string,
	agentIds: string[],
): Promise<void> {
	const store = await getStore();
	await store.set(disabledAgentsKey(connectionId), agentIds);
	await store.save();
}

export async function disableAgent(
	connectionId: string,
	agentId: string,
): Promise<void> {
	const disabled = await getDisabledAgents(connectionId);
	if (!disabled.includes(agentId)) {
		await setDisabledAgents(connectionId, [...disabled, agentId]);
	}
}

export async function enableAgent(
	connectionId: string,
	agentId: string,
): Promise<void> {
	const disabled = await getDisabledAgents(connectionId);
	await setDisabledAgents(
		connectionId,
		disabled.filter((id) => id !== agentId),
	);
}
