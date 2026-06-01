import { getStore } from ".";
import type { Connection } from "./types";

export async function getConnections(): Promise<Connection[]> {
	const store = await getStore();
	return (await store.get<Connection[]>("connections")) ?? [];
}

export async function addConnection(
	connection: Omit<Connection, "id">,
): Promise<Connection> {
	const store = await getStore();
	const connections = await getConnections();
	const newConnection: Connection = {
		...connection,
		id: crypto.randomUUID(),
	};
	await store.set("connections", [...connections, newConnection]);
	await store.save();
	return newConnection;
}

export async function updateConnection(
	connection: Connection,
): Promise<Connection> {
	const store = await getStore();
	const connections = await getConnections();
	await store.set(
		"connections",
		connections.map((c) => (c.id === connection.id ? connection : c)),
	);
	await store.save();
	return connection;
}

export async function removeConnection(id: string): Promise<void> {
	const store = await getStore();
	const connections = await getConnections();
	await store.set(
		"connections",
		connections.filter((c) => c.id !== id),
	);
	await store.save();
}
