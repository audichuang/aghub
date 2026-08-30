import { getStore } from ".";

const KEY = "lastKnownAvailableAgents";

export async function getLastKnownAvailableAgents(): Promise<string[] | null> {
	const store = await getStore();
	const value = await store.get<string[]>(KEY);
	if (value === undefined || value === null) return null;
	return value;
}

export async function setLastKnownAvailableAgents(
	agentIds: string[],
): Promise<void> {
	const store = await getStore();
	await store.set(KEY, agentIds);
	await store.save();
}
