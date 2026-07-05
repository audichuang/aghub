import { Alert, AlertDialog, Button, Spinner, toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { info } from "@tauri-apps/plugin-log";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type {
	ConnectionProviderProps,
	ConnectionContextValue,
	ConnectResult,
	TestResult,
} from "../contexts/connection";
import { ConnectionContext } from "../contexts/connection";
import { ServerContext } from "../contexts/server";
import {
	baseUrlFromPort,
	deriveSupportsCredentialForwarding,
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
 * bleeds into another's. Deliberately excludes ["connections"] and the
 * ["server", ...] cache; the selected target's server cache is reset
 * separately so the switch enters a visible bring-up state instead of
 * rendering against a stale tunnel port.
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
	/** Flip the cached server bring-up result so the provider re-renders into
	 * the connected state (the tunnel is already open by then). Carries the
	 * forwarding capability so it lands atomically with the port. */
	onRedeployed: (result: ConnectResult) => void;
}

interface ConnectionPendingScreenProps {
	connection: Connection;
	onUseLocal: () => void;
}

interface ConnectionErrorScreenProps {
	connection: Connection;
	message: string;
	isRetrying: boolean;
	onRetry: () => void;
	onUseLocal: () => void;
}

function ConnectionPendingScreen({
	connection,
	onUseLocal,
}: ConnectionPendingScreenProps) {
	const { t } = useTranslation();
	const [elapsedSeconds, setElapsedSeconds] = useState(0);
	const isLocal = connection.id === LOCAL_CONNECTION.id;
	const displayLabel = isLocal ? t("connLocal") : connection.label;
	const target = isLocal ? "localhost" : connection.sshTarget;
	const steps = isLocal
		? [
				{
					key: "local-api",
					label: t("connPendingStepLocalApi"),
					detail: t("connPendingStepLocalApiDetail"),
				},
				{
					key: "local-data",
					label: t("connPendingStepData"),
					detail: t("connPendingStepLocalDataDetail"),
				},
			]
		: [
				{
					key: "ssh",
					label: t("connPendingStepSsh"),
					detail: t("connPendingStepSshDetail"),
				},
				{
					key: "api",
					label: t("connPendingStepApi"),
					detail: t("connPendingStepApiDetail"),
				},
				{
					key: "install",
					label: t("connPendingStepInstall"),
					detail: t("connPendingStepInstallDetail"),
				},
				{
					key: "data",
					label: t("connPendingStepData"),
					detail: t("connPendingStepRemoteDataDetail"),
				},
			];

	useEffect(() => {
		const startedAt = Date.now();
		const interval = window.setInterval(() => {
			setElapsedSeconds(Math.floor((Date.now() - startedAt) / 1000));
		}, 1000);
		return () => window.clearInterval(interval);
	}, [connection.id]);

	return (
		<div className="flex h-screen items-center justify-center bg-background px-6">
			<div className="w-full max-w-xl rounded-lg border border-border bg-surface px-5 py-5 shadow-sm">
				<div className="flex items-start gap-4">
					<div className="mt-0.5 flex size-10 shrink-0 items-center justify-center rounded-md bg-surface-secondary">
						<Spinner size="sm" />
					</div>
					<div className="min-w-0 flex-1">
						<p className="text-xs font-medium uppercase text-muted">
							{t("connPendingEyebrow")}
						</p>
						<h1 className="mt-1 text-lg font-semibold text-foreground">
							{isLocal
								? t("connPendingLocalTitle")
								: t("connPendingRemoteTitle", {
										label: displayLabel,
									})}
						</h1>
						<p className="mt-2 text-sm leading-6 text-muted">
							{isLocal
								? t("connPendingLocalDescription")
								: t("connPendingRemoteDescription")}
						</p>
					</div>
				</div>

				<div className="mt-5 grid gap-3 border-t border-border pt-4 text-xs sm:grid-cols-2">
					<div>
						<p className="font-medium text-muted">
							{t("connPendingTarget")}
						</p>
						<p className="mt-1 truncate font-mono text-foreground">
							{target}
						</p>
					</div>
					<div>
						<p className="font-medium text-muted">
							{t("connPendingElapsed")}
						</p>
						<p className="mt-1 font-mono text-foreground">
							{t("connPendingElapsedValue", {
								seconds: elapsedSeconds,
							})}
						</p>
					</div>
				</div>

				<div className="mt-5 space-y-3">
					{steps.map((step, index) => (
						<div key={step.key} className="flex gap-3">
							<span
								className="mt-1 flex size-5 shrink-0 items-center justify-center rounded-full border border-border text-[10px] font-medium text-muted"
								aria-hidden="true"
							>
								{index + 1}
							</span>
							<div className="min-w-0">
								<p className="text-sm font-medium text-foreground">
									{step.label}
								</p>
								<p className="mt-0.5 break-words text-xs leading-5 text-muted">
									{step.detail}
								</p>
							</div>
						</div>
					))}
				</div>

				{elapsedSeconds >= 90 && (
					<div className="mt-5 rounded-md border border-warning/30 bg-warning/10 px-3 py-2 text-xs leading-5 text-warning">
						{t("connPendingLongWait", {
							seconds: elapsedSeconds,
						})}
					</div>
				)}

				{!isLocal && (
					<div className="mt-5 flex justify-end border-t border-border pt-4">
						<Button variant="tertiary" onPress={onUseLocal}>
							{t("connUseLocal")}
						</Button>
					</div>
				)}
			</div>
		</div>
	);
}

function ConnectionErrorScreen({
	connection,
	message,
	isRetrying,
	onRetry,
	onUseLocal,
}: ConnectionErrorScreenProps) {
	const { t } = useTranslation();
	const isLocal = connection.id === LOCAL_CONNECTION.id;
	const displayLabel = isLocal ? t("connLocal") : connection.label;

	return (
		<div className="flex h-screen items-center justify-center bg-background px-6">
			<div className="w-full max-w-xl rounded-lg border border-danger/30 bg-surface px-5 py-5 shadow-sm">
				<p className="text-xs font-medium uppercase text-danger">
					{t("connStatusError")}
				</p>
				<h1 className="mt-1 text-lg font-semibold text-foreground">
					{t("connErrorTitle", { label: displayLabel })}
				</h1>
				<p className="mt-2 break-words text-sm leading-6 text-muted">
					{message}
				</p>
				<div className="mt-5 flex flex-wrap justify-end gap-2 border-t border-border pt-4">
					{!isLocal && (
						<Button variant="tertiary" onPress={onUseLocal}>
							{t("connUseLocal")}
						</Button>
					)}
					<Button
						variant="primary"
						onPress={onRetry}
						isDisabled={isRetrying}
					>
						{isRetrying && (
							<Spinner
								size="sm"
								color="current"
								className="mr-2"
							/>
						)}
						{t("connRetry")}
					</Button>
				</div>
			</div>
		</div>
	);
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
			invoke<ConnectResult>("force_redeploy_remote", { connection }),
		onSuccess: (result) => {
			setConfirmOpen(false);
			onRedeployed(result);
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
	const { t } = useTranslation();
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

	// Resolve the active connection's bring-up result: local -> start_server
	// (port only, never forwards), remote -> connect_remote (port AND the
	// git-credential-forwarding capability, resolved together at bring-up
	// time). Keyed by the active connection id so each host has its own cached
	// bring-up result. Returning the capability HERE — atomically with the
	// port — closes the race where the first remote git-auth queries could run
	// unforwarded against a capable remote and cache an auth failure before a
	// separate, later capability probe flipped to `true`.
	const serverQuery = useQuery<ConnectResult>({
		queryKey: ["server", activeId],
		queryFn: async (): Promise<ConnectResult> => {
			if (activeConnection.id === LOCAL_CONNECTION.id) {
				const port = await invoke<number>("start_server");
				// Local is structurally non-forwarding.
				return { port, supportsCredentialForwarding: false };
			}
			return invoke<ConnectResult>("connect_remote", {
				connection: activeConnection,
			});
		},
	});

	const port = serverQuery.data?.port ?? null;
	const status = projectStatus({ ...serverQuery, data: port });
	const baseUrl = port === null ? null : baseUrlFromPort(port);

	// Fail-safe: forward only when the active connection is a remote whose
	// bring-up result confirmed support. Local (and any unresolved/old/error
	// bring-up) yields `false`. Derived from `serverQuery.data` so it is known
	// the moment `baseUrl` is — never lagging behind a separate probe.
	const supportsCredentialForwarding = deriveSupportsCredentialForwarding(
		activeId,
		serverQuery.data,
	);

	const setActive = useCallback(
		(id: string) => {
			// Event handler (NOT a useEffect): drop per-host data caches so the
			// new target re-fetches cleanly. Also drop that target's bring-up
			// cache so a reused remote port cannot hide an active reconnect or
			// auto-install behind stale page content.
			for (const ns of DATA_NAMESPACES) {
				queryClient.removeQueries({ queryKey: [ns] });
			}
			queryClient.removeQueries({
				queryKey: ["server", id],
				exact: true,
			});
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

	const reinstallRemoteApi = useCallback(
		(connection: Connection) =>
			invoke<TestResult>("reinstall_remote_api", { connection }),
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
					const droppedExists = connections.some(
						(c) => c.id === connectionId,
					);
					toast.danger(t("connDisconnectedToast"), {
						description: t("connDisconnectedToastDesc"),
						...(droppedExists && {
							actionProps: {
								children: t("connReconnect"),
								onPress: () => setActive(connectionId),
							},
						}),
					});
					setActive(LOCAL_CONNECTION.id);
				}
			},
		);
		return () => {
			void unlisten.then((fn) => fn());
		};
	}, [activeId, connections, setActive, t]);

	// Pull-fallback: when the window regains focus and the active connection
	// is a remote, verify the tunnel is still live. If the backend reports it
	// gone, silently fall back to Local — the remote-disconnected event may
	// have been missed while the window was hidden.
	useEffect(() => {
		if (activeId === LOCAL_CONNECTION.id) return;
		const handleFocus = () => {
			void invoke<boolean>("remote_status", {
				connectionId: activeId,
			}).then((alive) => {
				if (!alive) {
					void info(
						`remote_status: tunnel gone for ${activeId}, falling back to Local`,
					);
					setActive(LOCAL_CONNECTION.id);
				}
			});
		};
		window.addEventListener("focus", handleFocus);
		return () => window.removeEventListener("focus", handleFocus);
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
					onRedeployed={(result) =>
						queryClient.setQueryData<ConnectResult>(
							["server", activeId],
							result,
						)
					}
				/>
			);
		}
		return (
			<ConnectionErrorScreen
				connection={activeConnection}
				message={remoteErrorMessage(serverQuery.error)}
				isRetrying={serverQuery.isFetching}
				onRetry={() => {
					void serverQuery.refetch();
				}}
				onUseLocal={() => setActive(LOCAL_CONNECTION.id)}
			/>
		);
	}

	if (baseUrl === null || port === null) {
		return (
			<ConnectionPendingScreen
				key={activeConnection.id}
				connection={activeConnection}
				onUseLocal={() => setActive(LOCAL_CONNECTION.id)}
			/>
		);
	}

	const connectionValue: ConnectionContextValue = {
		connections,
		activeId,
		activeConnection,
		status,
		port,
		baseUrl,
		supportsCredentialForwarding,
		setActive,
		addConnection,
		updateConnection,
		removeConnection,
		testConnection,
		reinstallRemoteApi,
		disconnect,
	};

	return (
		<ConnectionContext value={connectionValue}>
			<ServerContext value={{ port, baseUrl }}>{children}</ServerContext>
		</ConnectionContext>
	);
}
