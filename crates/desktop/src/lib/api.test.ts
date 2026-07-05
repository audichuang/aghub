import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; use Node's built-in
// runner (`node --test --experimental-strip-types`), same as the sibling tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { createApi } from "./api.ts";

// Regression guard for the v2.4.0 desktop P0: the delete endpoints gate on
// `?confirm=true` (the backend does `confirm.unwrap_or(false)` => dry-run).
// mcps.delete / subAgents.delete take no request body, so the api client is
// the ONLY place confirm can be pinned — drop it and the delete silently
// no-ops while the caller still toasts success.

function stubFetchCapturingUrl(): { calls: URL[]; restore: () => void } {
	const calls: URL[] = [];
	const original = globalThis.fetch;
	globalThis.fetch = (async (input: RequestInfo | URL) => {
		const raw = input instanceof Request ? input.url : input.toString();
		calls.push(new URL(raw));
		return new Response("", { status: 200 });
	}) as typeof fetch;
	return {
		calls,
		restore: () => {
			globalThis.fetch = original;
		},
	};
}

test("mcps.delete sends confirm=true so the backend executes", async () => {
	const { calls, restore } = stubFetchCapturingUrl();
	try {
		await createApi("http://api.test/").mcps.delete(
			"my-server",
			"claude",
			"global",
		);
	} finally {
		restore();
	}
	assert.equal(calls.length, 1);
	assert.equal(calls[0].searchParams.get("confirm"), "true");
});

test("subAgents.delete sends confirm=true so the backend executes", async () => {
	const { calls, restore } = stubFetchCapturingUrl();
	try {
		await createApi("http://api.test/").subAgents.delete(
			"my-agent",
			"claude",
			"global",
		);
	} finally {
		restore();
	}
	assert.equal(calls.length, 1);
	assert.equal(calls[0].searchParams.get("confirm"), "true");
});
