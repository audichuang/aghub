import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; use Node's built-in
// runner (`node --test --experimental-strip-types`), same as the sibling tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { bulkApplyTimeoutMs, createApi } from "./api.ts";

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
		// `{}` rather than an empty body: the mcp/sub-agent deletes discard
		// the response, but `skills.delete` parses it, and an empty body makes
		// ky throw before the assertions on the captured URL are reached.
		return new Response("{}", { status: 200 });
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

// `all_agents` is spelled in exactly two places — here and
// `DeleteSkillParams` in crates/api/src/routes/skills.rs. Rename either one
// and the delete silently falls back to `unwrap_or(false)`: a single-agent
// removal that the server answers `kept` whenever another agent still reads
// the shared master, so the source page's clean-up fails on every skill more
// than one agent holds. The query string is asserted whole, so a reorder of
// the `searchParams` spread fails this too.
test("skills.delete sends scope, confirm and all_agents", async () => {
	const { calls, restore } = stubFetchCapturingUrl();
	try {
		await createApi("http://api.test/").skills.delete(
			"claude",
			"my-skill",
			"global",
			undefined,
			true,
		);
	} finally {
		restore();
	}
	assert.equal(calls.length, 1);
	assert.equal(calls[0].pathname, "/agents/claude/skills/my-skill");
	assert.equal(
		calls[0].searchParams.toString(),
		"scope=global&confirm=true&all_agents=true",
	);
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
test("skills.applyUpdates sends one forwarded batch request", async () => {
	const original = globalThis.fetch;
	const calls: Array<{
		url: URL;
		headers: Headers;
		body: unknown;
	}> = [];
	globalThis.fetch = (async (
		input: RequestInfo | URL,
		init?: RequestInit,
	) => {
		const request =
			input instanceof Request ? input : new Request(input, init);
		calls.push({
			url: new URL(request.url),
			headers: request.headers,
			body: await request.clone().json(),
		});
		return new Response(JSON.stringify({ results: [] }), {
			status: 200,
			headers: { "Content-Type": "application/json" },
		});
	}) as typeof fetch;
	try {
		const response = await createApi(
			"http://api.test/",
		).skills.applyUpdates(
			{
				source: "https://git.example/owner/repo.git",
				names: ["alpha", "beta"],
				scope: "project",
				projectRoot: "/tmp/project",
				confirm: true,
			},
			{ "X-Aghub-Git-Tokens": "forwarded" },
		);
		assert.deepEqual(response, { results: [] });
	} finally {
		globalThis.fetch = original;
	}

	assert.equal(calls.length, 1);
	assert.equal(calls[0].url.pathname, "/skills/apply-updates");
	assert.equal(calls[0].headers.get("X-Aghub-Git-Tokens"), "forwarded");
	assert.deepEqual(calls[0].body, {
		source: "https://git.example/owner/repo.git",
		names: ["alpha", "beta"],
		scope: "project",
		projectRoot: "/tmp/project",
		confirm: true,
	});
});

test("the bulk apply timeout scales with the batch and stays finite", () => {
	assert.equal(bulkApplyTimeoutMs(1), 150_000);
	assert.equal(bulkApplyTimeoutMs(10), 420_000);
	// The cap engages at 26 and never lets the budget become unbounded — an
	// unsettling request leaves the UI unable to report anything at all.
	assert.equal(bulkApplyTimeoutMs(26), 900_000);
	assert.equal(bulkApplyTimeoutMs(500), 900_000);
});
