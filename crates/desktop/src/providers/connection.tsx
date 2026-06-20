import { Alert, AlertDialog, Button, Spinner, toast } from "@heroui/react";
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
import { asRemotePayload, remoteErrorMessage } from "../lib/remote-errors";
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
	"projects",
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

interface IncompatibleConnectionScreenProps {
	connection: Connection;
	remoteVersion: string | null;
	/** Flip the cached server port so the provider re-renders into the
	 * connected state (the tunnel is already open by then). */
	onRedeployed: (port: number) => void;
}

/**
 * Actionable replacement for the bare error screen when the remote `aghub-api`
 * is version-incompatible: shows both versions and a confirmed force-redeploy
 * that overwrites the remote binary with the desktop's bundled one.
 *
 * Self-contained (owns its mutation + confirm dialog) because the error branch
 * renders BEFORE the ConnectionContext boundary. The persistent banner is the
 * deliberate exception to the "errors via toast" rule; transient sub-failures
 * still toast, and a cross-platform refusal is surfaced inline (it is an
 * actionable "install manually" state, not a transient error).
 */
function IncompatibleConnectionScreen({
	connection,
	remoteVersion,
	onRedeployed,
}: IncompatibleConnectionScreenProps) {
	const [confirmOpen, setConfirmOpen] = useState(false);

	const { data: localVersion } = useQuery<string>({
		queryKey: ["local-api-version"],
		queryFn: () => invoke<string>("local_api_version"),
	});

	const { data: installSourceAvailable } = useQuery<boolean>({
		queryKey: ["remote-install-source-available"],
		queryFn: () => invoke<boolean>("remote_install_source_available"),
		staleTime: Number.POSITIVE_INFINITY,
	});

	const redeploy = useMutation({
		mutationFn: () =>
			invoke<number>("force_redeploy_remote", { connection }),
		onSuccess: (port) => {
			setConfirmOpen(false);
			onRedeployed(port);
		},
		onError: (error) => {
			setConfirmOpen(false);
			// Cross-platform refusal is rendered inline (persistent + actionable);
			// every other failure is transient -> toast.
			if (asRemotePayload(error)?.kind !== "crossPlatformRedeploy") {
				toast.danger(remoteErrorMessage(error));
			}
		},
	});

	const redeployError = asRemotePayload(redeploy.error);
	const crossPlatform =
		redeployError?.kind === "crossPlatformRedeploy" ? redeployError : null;

	return (
		<div className="flex h-screen flex-col items-center justify-center gap-4 px-6 text-center">
			<div className="max-w-md space-y-2">
				<h1 className="text-lg font-semibold text-foreground">
					Remote aghub-api is incompatible
				</h1>
				<p className="text-sm text-muted">
					{connection.label} is running aghub-api{" "}
					<span className="font-mono">
						{remoteVersion ?? "unknown"}
					</span>
					, but this desktop bundles{" "}
					<span className="font-mono">{localVersion ?? "…"}</span>.
				</p>
			</div>

			{crossPlatform ? (
				<p className="max-w-md text-xs text-danger">
					{crossPlatform.hint ??
						"The remote platform differs from this desktop, so its bundled binary cannot run there. Install aghub-api on the VM manually."}
				</p>
			) : installSourceAvailable === false ? (
				<Alert status="warning" className="max-w-md">
					<Alert.Indicator />
					<Alert.Content>
						<Alert.Description>
							Auto-deploy isn&apos;t available in this build —
							install aghub-api on the VM manually.
						</Alert.Description>
					</Alert.Content>
				</Alert>
			) : (
				<Button
					variant="primary"
					onPress={() => setConfirmOpen(true)}
					isDisabled={redeploy.isPending}
				>
					Force redeploy
				</Button>
			)}

			<AlertDialog.Backdrop
				isOpen={confirmOpen}
				onOpenChange={setConfirmOpen}
			>
				<AlertDialog.Container>
					<AlertDialog.Dialog className="sm:max-w-[420px]">
						<AlertDialog.CloseTrigger />
						<AlertDialog.Header>
							<AlertDialog.Icon status="danger" />
							<AlertDialog.Heading>
								Force redeploy?
							</AlertDialog.Heading>
						</AlertDialog.Header>
						<AlertDialog.Body>
							<p className="text-sm text-muted">
								This overwrites the remote aghub-api (including
								your own fork) with the desktop's bundled build,
								then restarts and reconnects. Continue?
							</p>
						</AlertDialog.Body>
						<AlertDialog.Footer>
							<Button
								slot="close"
								variant="tertiary"
								onPress={() => setConfirmOpen(false)}
								isDisabled={redeploy.isPending}
							>
								Cancel
							</Button>
							<Button
								variant="danger"
								onPress={() => redeploy.mutate()}
								isDisabled={redeploy.isPending}
							>
								{redeploy.isPending ? (
									<>
										<Spinner
											size="sm"
											color="current"
											className="mr-2"
										/>
										Redeploying…
									</>
								) : (
									"Redeploy"
								)}
							</Button>
						</AlertDialog.Footer>
					</AlertDialog.Dialog>
				</AlertDialog.Container>
			</AlertDialog.Backdrop>
		</div>
	);
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
		const payload = asRemotePayload(serverQuery.error);
		if (payload?.kind === "incompatible") {
			return (
				<IncompatibleConnectionScreen
					connection={activeConnection}
					remoteVersion={payload.remoteVersion ?? null}
					onRedeployed={(port) =>
						queryClient.setQueryData(["server", activeId], port)
					}
				/>
			);
		}
		return (
			<div className="flex h-screen items-center justify-center">
				<p className="text-sm text-danger">
					Failed to connect to {activeConnection.label}:{" "}
					{remoteErrorMessage(serverQuery.error)}
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
