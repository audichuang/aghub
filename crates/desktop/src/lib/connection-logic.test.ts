import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed in this environment, and the
// task forbids adding dependencies, so this pure-logic test uses Node's
// built-in runner (`node --test --experimental-strip-types`). The antfu config
// enforces vitest over node:test, which does not apply here.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	baseUrlFromPort,
	type ConnectResult,
	deriveSupportsCredentialForwarding,
	LOCAL_CONNECTION,
	mergeConnections,
	projectStatus,
	type QueryStateLike,
	selectConnectionView,
} from "./connection-logic.ts";

test("LOCAL_CONNECTION has id 'local'", () => {
	assert.equal(LOCAL_CONNECTION.id, "local");
	assert.equal(LOCAL_CONNECTION.label, "Local");
});

test("mergeConnections prepends LOCAL_CONNECTION", () => {
	const remotes = [
		{ id: "a", label: "VM A", sshTarget: "vm-a" },
		{ id: "b", label: "VM B", sshTarget: "vm-b" },
	];
	const merged = mergeConnections(remotes);
	assert.equal(merged.length, 3);
	assert.equal(merged[0], LOCAL_CONNECTION);
	assert.equal(merged[1].id, "a");
	assert.equal(merged[2].id, "b");
});

test("mergeConnections with no remotes yields just Local", () => {
	const merged = mergeConnections([]);
	assert.deepEqual(merged, [LOCAL_CONNECTION]);
});

test("baseUrlFromPort builds the api v1 url", () => {
	assert.equal(baseUrlFromPort(5173), "http://localhost:5173/api/v1");
	assert.equal(baseUrlFromPort(0), "http://localhost:0/api/v1");
});

function makeState(partial: Partial<QueryStateLike>): QueryStateLike {
	return {
		isError: false,
		isPending: false,
		isFetching: false,
		data: undefined,
		...partial,
	};
}

test("projectStatus: error wins", () => {
	assert.equal(
		projectStatus(makeState({ isError: true, data: 100 })),
		"error",
	);
});

test("projectStatus: resolved port => connected", () => {
	assert.equal(projectStatus(makeState({ data: 100 })), "connected");
});

test("projectStatus: pending without data => connecting", () => {
	assert.equal(projectStatus(makeState({ isPending: true })), "connecting");
});

test("projectStatus: fetching without data => connecting", () => {
	assert.equal(projectStatus(makeState({ isFetching: true })), "connecting");
});

test("projectStatus: idle when nothing in flight and no data", () => {
	assert.equal(projectStatus(makeState({})), "idle");
});

test("projectStatus: data 0 is a valid connected port", () => {
	// 0 should never be a real port here, but the projection keys on
	// `typeof data === 'number'`, so this documents the behavior.
	assert.equal(projectStatus(makeState({ data: 0 })), "connected");
});

// ─── deriveSupportsCredentialForwarding (capability-gating race fix) ─────────
//
// The capability now rides on the SAME `connect_remote` result as the port, so
// it is available at the moment `baseUrl` resolves — there is no window where a
// forwarding-eligible query (sources diff, check-updates) runs unforwarded
// against a capable remote and caches an auth failure before a separate, later
// probe flips the flag. These prove the derivation reads the bring-up result,
// not a late probe, and stays fail-safe `false` otherwise.

const capable: ConnectResult = {
	port: 50001,
	supportsCredentialForwarding: true,
};
const notCapable: ConnectResult = {
	port: 50002,
	supportsCredentialForwarding: false,
};

test("forwarding: capable the instant the bring-up result is present (no late probe)", () => {
	// The data is the SAME object the port came from, so the capability is known
	// atomically with `baseUrl` — the race the fix closes.
	assert.equal(deriveSupportsCredentialForwarding("vm-1", capable), true);
});

test("forwarding: false for Local even if a result somehow says true", () => {
	assert.equal(
		deriveSupportsCredentialForwarding(LOCAL_CONNECTION.id, capable),
		false,
	);
});

test("forwarding: false for a remote whose bring-up did NOT advertise support", () => {
	assert.equal(deriveSupportsCredentialForwarding("vm-1", notCapable), false);
});

test("forwarding: fail-safe false while the remote bring-up is unresolved", () => {
	// No window: until the result lands (undefined/null), the gate is off — so a
	// first query that races ahead cannot forward to an unconfirmed remote.
	assert.equal(deriveSupportsCredentialForwarding("vm-1", undefined), false);
	assert.equal(deriveSupportsCredentialForwarding("vm-1", null), false);
});

test("selectConnectionView: connected -> ready", () => {
	assert.equal(selectConnectionView("connected", false), "ready");
});

test("selectConnectionView: connecting/idle -> pending", () => {
	assert.equal(selectConnectionView("connecting", false), "pending");
	assert.equal(selectConnectionView("idle", false), "pending");
});

test("selectConnectionView: error -> error", () => {
	assert.equal(selectConnectionView("error", false), "error");
});

test("selectConnectionView: incompatible wins over generic error", () => {
	assert.equal(selectConnectionView("error", true), "incompatible");
});

test("selectConnectionView: isIncompatible ignored unless status is error", () => {
	assert.equal(selectConnectionView("connected", true), "ready");
	assert.equal(selectConnectionView("connecting", true), "pending");
});
