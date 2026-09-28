import { isHTTPError } from "ky";
import { getStore } from ".";
import type { ApiClient } from "../../requests/client";
import {
	disabledAgentsKey,
	LOCAL_DISABLED_AGENTS_KEY,
	loadDisabledAgents,
	resolveDisabledAgents,
	resolveLegacyDisabledAgents,
} from "./disabled-agents.ts";

/**
 * The selection saved in this app before the server owned it. Read only to
 * seed a server that has none, or to serve an older remote whose aghub-api has
 * no `/agents/disabled` route.
 */
async function getLegacyDisabledAgents(
	connectionId: string,
): Promise<string[] | null> {
	const store = await getStore();
	const own = await store.get<string[]>(disabledAgentsKey(connectionId));
	// Local's own key IS the Local key — no second read, and no fallback loop.
	const local =
		connectionId === "local"
			? own
			: await store.get<string[]>(LOCAL_DISABLED_AGENTS_KEY);
	return resolveLegacyDisabledAgents(own, local);
}

async function setLegacyDisabledAgents(
	connectionId: string,
	agentIds: string[],
): Promise<void> {
	const store = await getStore();
	await store.set(disabledAgentsKey(connectionId), agentIds);
	await store.save();
}

/** `null` when the connected aghub-api predates the setting. */
async function readServer(api: ApiClient) {
	try {
		return await api.agents.disabled();
	} catch (error) {
		if (isHTTPError(error) && error.response.status === 404) return null;
		throw error;
	}
}

/** The effective selection for the active connection. */
export function getDisabledAgents(
	api: ApiClient,
	connectionId: string,
	knownIds: ReadonlySet<string>,
): Promise<string[]> {
	return loadDisabledAgents({
		read: () => readServer(api),
		write: (agents) => api.agents.setDisabled(agents),
		legacy: () => getLegacyDisabledAgents(connectionId),
		knownIds,
	});
}

/** Turn one agent on or off, on the server when it has the setting. */
export async function setAgentDisabled(
	api: ApiClient,
	connectionId: string,
	agentId: string,
	disabled: boolean,
): Promise<void> {
	const stored = await readServer(api);
	const current =
		stored?.agents ??
		resolveDisabledAgents(await getLegacyDisabledAgents(connectionId), []);
	const next = disabled
		? [...new Set([...current, agentId])]
		: current.filter((id) => id !== agentId);
	if (stored === null) {
		await setLegacyDisabledAgents(connectionId, next);
	} else {
		await api.agents.setDisabled(next);
	}
}
