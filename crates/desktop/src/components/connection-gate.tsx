import type { ReactNode } from "react";
import { ServerContext } from "../contexts/server";
import { useConnection } from "../hooks/use-connection";
import {
	LOCAL_CONNECTION,
	selectConnectionView,
} from "../lib/connection-logic";
import { asRemotePayload, remoteErrorMessage } from "../lib/remote-errors";
import { AgentAvailabilityProvider } from "../providers/agent-availability";
import {
	ConnectionErrorScreen,
	ConnectionPendingScreen,
	IncompatibleConnectionScreen,
} from "../providers/connection";

/**
 * Content-area connection gate. Ready pages only mount once both ServerContext
 * and AgentAvailabilityProvider are available; cold start remains handled by
 * ConnectionProvider.
 */
export function ConnectionGate({ children }: { children: ReactNode }) {
	const {
		status,
		port,
		baseUrl,
		activeConnection,
		setActive,
		connectError,
		retryConnect,
		isRetryingConnect,
		applyConnectResult,
	} = useConnection();

	const payload = asRemotePayload(connectError);
	const isIncompatible = payload?.kind === "incompatible";
	const view = selectConnectionView(status, isIncompatible);

	if (view === "ready" && port !== null && baseUrl !== null) {
		return (
			<ServerContext value={{ port, baseUrl }}>
				<AgentAvailabilityProvider>
					{children}
				</AgentAvailabilityProvider>
			</ServerContext>
		);
	}

	if (view === "incompatible") {
		return (
			<IncompatibleConnectionScreen
				connection={activeConnection}
				remoteVersion={payload?.remoteVersion ?? null}
				onRedeployed={applyConnectResult}
			/>
		);
	}

	if (view === "error") {
		return (
			<ConnectionErrorScreen
				connection={activeConnection}
				message={remoteErrorMessage(connectError)}
				isRetrying={isRetryingConnect}
				onRetry={retryConnect}
				onUseLocal={() => setActive(LOCAL_CONNECTION.id)}
			/>
		);
	}

	return (
		<ConnectionPendingScreen
			key={activeConnection.id}
			connection={activeConnection}
			onUseLocal={() => setActive(LOCAL_CONNECTION.id)}
		/>
	);
}
