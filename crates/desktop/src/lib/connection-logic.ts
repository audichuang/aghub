/**
 * Pure connection logic — NO React / Tauri imports.
 *
 * Everything here is synchronous and side-effect free so it can be unit
 * tested with Node's built-in test runner (`node --test`). The only import
 * is a type-only import (erased at build/strip time), so this module pulls
 * in no runtime dependencies.
 */
import type { Connection } from "./store/types";

/**
 * The implicit Local connection. Always present, never persisted to the
 * store. Its baseUrl is derived from the `start_server` port at runtime.
 */
export const LOCAL_CONNECTION: Connection = {
	id: "local",
	label: "Local",
	sshTarget: "",
};

/**
 * The full ordered connection list shown in the switcher: Local first,
 * followed by the user's persisted remotes.
 */
export function mergeConnections(remotes: Connection[]): Connection[] {
	return [LOCAL_CONNECTION, ...remotes];
}

/** Build the api baseUrl for a resolved local tunnel / server port. */
export function baseUrlFromPort(port: number): string {
	return `http://localhost:${port}/api/v1`;
}

/** The 4-state status the FE projects from a react-query state. */
export type ConnectionStatus = "idle" | "connecting" | "connected" | "error";

/**
 * Minimal shape of the bits of a react-query result we project from.
 * Declared structurally so this module stays free of @tanstack imports.
 */
export interface QueryStateLike {
	isError: boolean;
	isPending: boolean;
	isFetching: boolean;
	data: number | null | undefined;
}

/**
 * Project a react-query result into the 4-state connection status.
 *
 * - error wins over everything (a failed bring-up).
 * - a resolved port (data) means connected.
 * - pending/fetching with no port yet means connecting.
 * - otherwise idle (not yet attempted / disabled query).
 */
export function projectStatus(queryState: QueryStateLike): ConnectionStatus {
	if (queryState.isError) return "error";
	if (typeof queryState.data === "number") return "connected";
	if (queryState.isPending || queryState.isFetching) return "connecting";
	return "idle";
}
