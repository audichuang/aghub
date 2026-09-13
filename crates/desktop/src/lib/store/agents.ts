import { getStore } from ".";

const LOCAL_DISABLED_AGENTS_KEY = "disabledAgents";

/** Mirrors `projectsKey`: the bare key stays Local's, remotes get a suffix. */
function disabledAgentsKey(connectionId: string): string {
	if (connectionId === "local") {
		return LOCAL_DISABLED_AGENTS_KEY;
	}
	return `${LOCAL_DISABLED_AGENTS_KEY}:${connectionId}`;
}

/**
 * The agent selection for one connection.
 *
 * A remote that has never been configured inherits Local's selection, so the
 * choice made here carries over the first time you connect; the first toggle
 * on that remote materializes its own key and the two diverge from then on.
 *
 * The `undefined` check is load-bearing and must not become `?? []` or a
 * length test: a remote where the user re-enabled every inherited agent stores
 * `[]`, and treating that as "unconfigured" would fall back to Local and
 * silently disable them again.
 */
export async function getDisabledAgents(
	connectionId: string,
): Promise<string[]> {
	const store = await getStore();
	const own = await store.get<string[]>(disabledAgentsKey(connectionId));
	if (own !== undefined && own !== null) return own;
	if (connectionId === "local") return [];
	return (await store.get<string[]>(LOCAL_DISABLED_AGENTS_KEY)) ?? [];
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
