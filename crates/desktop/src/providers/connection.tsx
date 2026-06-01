import { Spinner, toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { info } from "@tauri-apps/plugin-log";
import { useCallback, useEffect, useMemo, useState } from "react";
import type {
	ConnectionProviderProps,
	ConnectionContextValue,
	TestResult,
} from "../contexts/connection";
import { ConnectionContext } from "../contexts/connection";
import { ServerContext } from "../contexts/server";
import {
	baseUrlFromPort,
	LOCAL_CONNECTION,
	mergeConnections,
	projectStatus,
} from "../lib/connection-logic";
import type { Connection } from "../lib/store";
import {
	addConnection as addConnectionToStore,
	getConnections,
	removeConnection as removeConnectionFromStore,
	updateConnection as updateConnectionInStore,
} from "../lib/store";

/**
 * Per-host data query namespaces (roots from requests/keys.ts) that must be
 * dropped when switching the active connection, so one host's data never
 * bleeds into another's. Deliberately excludes ["connections"] and
 * ["server", ...] which are connection-management state, not per-host data.
 */
const DATA_NAMESPACES = [
	"skills",
	"mcps",
	"agents",
	"sub-agents",
	"plugins",
	"credentials",
	"inference-providers",
	"integrations",
	"market",
] as const;

interface RemoteDisconnectedPayload {
	connectionId: string;
}

export function ConnectionProvider({ children }: ConnectionProviderProps) {
	const queryClient = useQueryClient();
	const [activeId, setActiveId] = useState<string>(LOCAL_CONNECTION.id);

	const { data: remotes = [] } = useQuery<Connection[]>({
		queryKey: ["connections"],
		queryFn: getConnections,
	});

	const connections = useMemo(() => mergeConnections(remotes), [remotes]);

	const activeConnection = useMemo(
		() => connections.find((c) => c.id === activeId) ?? LOCAL_CONNECTION,
		[connections, activeId],
	);

	// Resolve the active connection's local port: local -> start_server,
	// remote -> connect_remote (returns the local tunnel port). Keyed by the
	// active connection id so each host has its own cached bring-up result.
	const serverQuery = useQuery<number>({
		queryKey: ["server", activeId],
		queryFn: () => {
			if (activeConnection.id === LOCAL_CONNECTION.id) {
				return invoke<number>("start_server");
			}
			return invoke<number>("connect_remote", {
				connection: activeConnection,
			});
		},
	});

	const status = projectStatus(serverQuery);
	const port = serverQuery.data ?? null;
	const baseUrl = port === null ? null : baseUrlFromPort(port);

	const setActive = useCallback(
		(id: string) => {
			// Event handler (NOT a useEffect): drop per-host data caches so the
			// new target re-fetches cleanly, then commit the active id.
			for (const ns of DATA_NAMESPACES) {
				queryClient.removeQueries({ queryKey: [ns] });
			}
			setActiveId(id);
		},
		[queryClient],
	);

	const addMutation = useMutation({
		mutationFn: addConnectionToStore,
		onSuccess: () =>
			queryClient.invalidateQueries({ queryKey: ["connections"] }),
	});

	const updateMutation = useMutation({
		mutationFn: updateConnectionInStore,
		onSuccess: () =>
			queryClient.invalidateQueries({ queryKey: ["connections"] }),
	});

	const removeMutation = useMutation({
		mutationFn: removeConnectionFromStore,
		onSuccess: () =>
			queryClient.invalidateQueries({ queryKey: ["connections"] }),
	});

	const addConnection = useCallback(
		(connection: Omit<Connection, "id">) =>
			addMutation.mutateAsync(connection),
		[addMutation],
	);

	const updateConnection = useCallback(
		(connection: Connection) => updateMutation.mutateAsync(connection),
		[updateMutation],
	);

	const removeConnection = useCallback(
		(id: string) => removeMutation.mutateAsync(id),
		[removeMutation],
	);

	const testConnection = useCallback(
		(connection: Connection) =>
			invoke<TestResult>("test_connection", { connection }),
		[],
	);

	const disconnect = useCallback(
		(id: string) => invoke<void>("disconnect_remote", { connectionId: id }),
		[],
	);

	// The ONE legitimate useEffect: subscribing to a real external event
	// stream (the Tauri 'remote-disconnected' event fired when a tunnel dies
	// unexpectedly). This is not data fetching or state syncing — it is an
	// external subscription with required cleanup, so useEffect is correct.
	useEffect(() => {
		const unlisten = listen<RemoteDisconnectedPayload>(
			"remote-disconnected",
			(event) => {
				const { connectionId } = event.payload;
				void info(`Remote tunnel disconnected: ${connectionId}`);
				if (connectionId === activeId) {
					toast.danger("Remote connection lost. Switched to Local.");
					setActive(LOCAL_CONNECTION.id);
				}
			},
		);
		return () => {
			void unlisten.then((fn) => fn());
		};
	}, [activeId, setActive]);

	// A failed bring-up (e.g. local start_server fails, or connect_remote
	// throws a RemoteError) must not spin forever — surface it like the old
	// ServerProvider did. The richer toast/reconnect UI is W6.
	if (serverQuery.isError) {
		return (
			<div className="flex h-screen items-center justify-center">
				<p className="text-sm text-danger">
					Failed to connect to {activeConnection.label}:{" "}
					{String(serverQuery.error)}
				</p>
			</div>
		);
	}

	if (baseUrl === null || port === null) {
		return (
			<div className="flex h-screen items-center justify-center">
				<Spinner size="lg" />
			</div>
		);
	}

	const connectionValue: ConnectionContextValue = {
		connections,
		activeId,
		activeConnection,
		status,
		port,
		baseUrl,
		setActive,
		addConnection,
		updateConnection,
		removeConnection,
		testConnection,
		disconnect,
	};

	return (
		<ConnectionContext value={connectionValue}>
			<ServerContext value={{ port, baseUrl }}>{children}</ServerContext>
		</ConnectionContext>
	);
}
