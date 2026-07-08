import type { ReactNode } from "react";
import { createContext, use } from "react";
import type { ConnectionStatus, ConnectResult } from "../lib/connection-logic";
import type { Connection } from "../lib/store";

export type { ConnectResult };

export interface ConnectionContextValue {
	/** Local + persisted remotes, Local always first. */
	connections: Connection[];
	/** The active connection id (default "local"). */
	activeId: string;
	/** The active connection, resolved from `connections`. */
	activeConnection: Connection;
	/** Projected 4-state status of the active connection's bring-up. */
	status: ConnectionStatus;
	/** Resolved local tunnel / server port, or null while connecting. */
	port: number | null;
	/** Resolved api baseUrl, or null while connecting. */
	baseUrl: string | null;
	/**
	 * Whether the ACTIVE connection is a remote that advertises controller-side
	 * git-credential forwarding (the `X-Aghub-Git-Tokens` header). Always
	 * `false` for Local and fail-safe `false` whenever support cannot be
	 * confirmed. Task 6 gates header injection on this.
	 */
	supportsCredentialForwarding: boolean;
	/** Switch active connection (clears per-host data caches first). */
	setActive: (id: string) => void;
	addConnection: (connection: Omit<Connection, "id">) => Promise<Connection>;
	updateConnection: (connection: Connection) => Promise<Connection>;
	removeConnection: (id: string) => Promise<void>;
	/** Probe a connection without mutating active state. */
	testConnection: (connection: Connection) => Promise<TestResult>;
	/** Force-reinstall remote aghub-api, then return the fresh probe result. */
	reinstallRemoteApi: (connection: Connection) => Promise<TestResult>;
	/** Tear down a remote tunnel + remote server. */
	disconnect: (id: string) => Promise<void>;
	/** Most recent raw connection bring-up error, or null when there is none. */
	connectError: unknown;
	/** Retry the current connection bring-up. */
	retryConnect: () => void;
	/** Whether the current connection bring-up is fetching/retrying. */
	isRetryingConnect: boolean;
	/** Write a successful incompatible redeploy result back into server cache. */
	applyConnectResult: (result: ConnectResult) => void;
}

/** Mirrors the Rust `TestResult` returned by `test_connection`. */
export interface TestResult {
	reachable: boolean;
	apiPresent: boolean;
	apiVersion: string | null;
	compatible: boolean;
	message: string;
	/**
	 * The remote `aghub-api` advertises controller-side git-credential
	 * forwarding (the `X-Aghub-Git-Tokens` header), probed over SSH via
	 * `--capabilities`. Fail-safe: `false` whenever support cannot be
	 * confirmed (old binary, transport failure, missing marker). Task 6 gates
	 * forwarding on this being `true`.
	 */
	supportsCredentialForwarding: boolean;
	installAttempted: boolean;
	installSucceeded: boolean;
	installMessage?: string | null;
}

export const ConnectionContext = createContext<ConnectionContextValue | null>(
	null,
);

export function useConnectionContext(): ConnectionContextValue {
	const ctx = use(ConnectionContext);
	if (!ctx) {
		throw new Error(
			"useConnection must be used within <ConnectionProvider>",
		);
	}
	return ctx;
}

export interface ConnectionProviderProps {
	children: ReactNode;
}
