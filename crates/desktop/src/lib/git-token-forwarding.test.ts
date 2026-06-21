import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed in this environment, and the
// task forbids adding dependencies, so this pure-logic test uses Node's
// built-in runner (`node --test --experimental-strip-types`). The antfu config
// enforces vitest over node:test, which does not apply here.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	boundSourceTokenMap,
	encodeGitTokensHeader,
	GIT_TOKENS_HEADER,
	gitTokensHeader,
	type InvokeFn,
	type ResolvedToken,
	resolveForwardedTokens,
	shouldForwardGitTokens,
} from "./git-token-forwarding.ts";

// ─── encodeGitTokensHeader ──────────────────────────────────────────────────

test("encodeGitTokensHeader round-trips via standard base64 (atob/JSON)", () => {
	const map = { "github.com/a/b": "ghp_TOKEN123" };
	const encoded = encodeGitTokensHeader(map);
	// Decode with the STANDARD alphabet (atob) to confirm parity with the Rust
	// decoder which uses the standard alphabet too.
	const decoded = JSON.parse(atob(encoded));
	assert.deepEqual(decoded, map);
});

/** Decode standard-base64(UTF-8 JSON) back to the original map, like Rust does. */
function decodeGitTokensHeader(encoded: string): Record<string, string> {
	const binary = atob(encoded);
	const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
	return JSON.parse(new TextDecoder().decode(bytes));
}

test("encodeGitTokensHeader uses the STANDARD alphabet (not url-safe)", () => {
	// A payload chosen so its base64 contains '+' and '/' (which url-safe base64
	// would render as '-' and '_'). Confirms we emit the standard alphabet.
	const map = { s: "ÿÿûï¾" };
	const encoded = encodeGitTokensHeader(map);
	assert.ok(
		encoded.includes("+") || encoded.includes("/"),
		`expected standard-alphabet chars in ${encoded}`,
	);
	assert.ok(!encoded.includes("-"), "url-safe '-' must not appear");
	assert.ok(!encoded.includes("_"), "url-safe '_' must not appear");
	// Decode through UTF-8 (the standard base64 carries UTF-8 bytes).
	assert.deepEqual(decodeGitTokensHeader(encoded), map);
});

test("encodeGitTokensHeader is UTF-8 safe for non-Latin1 content", () => {
	const map = { "源/技能": "令牌-✓-😀" };
	const encoded = encodeGitTokensHeader(map);
	assert.deepEqual(decodeGitTokensHeader(encoded), map);
});

test("encodeGitTokensHeader encodes an empty map as base64 of '{}'", () => {
	assert.equal(encodeGitTokensHeader({}), btoa("{}"));
});

// ─── gitTokensHeader ────────────────────────────────────────────────────────

test("gitTokensHeader returns undefined for an empty map (no header attached)", () => {
	assert.equal(gitTokensHeader({}), undefined);
});

test("gitTokensHeader builds the X-Aghub-Git-Tokens header for a non-empty map", () => {
	const map = { src: "tok" };
	const header = gitTokensHeader(map);
	assert.ok(header);
	assert.deepEqual(Object.keys(header), [GIT_TOKENS_HEADER]);
	assert.deepEqual(JSON.parse(atob(header[GIT_TOKENS_HEADER])), map);
});

// ─── shouldForwardGitTokens (gating predicate) ──────────────────────────────

test("gating: true only when remote AND capability advertised", () => {
	assert.equal(
		shouldForwardGitTokens({
			activeConnectionId: "vm-1",
			supportsCredentialForwarding: true,
		}),
		true,
	);
});

test("gating: false for the local connection even if capability is true", () => {
	assert.equal(
		shouldForwardGitTokens({
			activeConnectionId: "local",
			supportsCredentialForwarding: true,
		}),
		false,
	);
});

test("gating: false for a remote that does not advertise capability", () => {
	assert.equal(
		shouldForwardGitTokens({
			activeConnectionId: "vm-1",
			supportsCredentialForwarding: false,
		}),
		false,
	);
});

test("gating: false for local without capability", () => {
	assert.equal(
		shouldForwardGitTokens({
			activeConnectionId: "local",
			supportsCredentialForwarding: false,
		}),
		false,
	);
});

// ─── resolveForwardedTokens (invoke mocked) ─────────────────────────────────

/** Build a mock `invoke` that resolves `resolve_git_token` from a fixed table. */
function mockResolveInvoke(
	table: Record<string, ResolvedToken | null>,
	calls: string[],
): InvokeFn {
	return (async (cmd: string, args?: Record<string, unknown>) => {
		if (cmd === "resolve_git_token") {
			const source = String(args?.source);
			calls.push(source);
			return table[source] ?? null;
		}
		throw new Error(`unexpected invoke: ${cmd}`);
	}) as InvokeFn;
}

test("resolveForwardedTokens includes resolved tokens and SKIPS nulls", async () => {
	const calls: string[] = [];
	const invoke = mockResolveInvoke(
		{
			"src-a": { token: "TOK_A", origin: null },
			"src-b": null, // no credential -> skipped
			"src-c": { token: "TOK_C", origin: null },
		},
		calls,
	);

	const map = await resolveForwardedTokens(
		["src-a", "src-b", "src-c"],
		invoke,
	);

	assert.deepEqual(map, { "src-a": "TOK_A", "src-c": "TOK_C" });
	// All three sources were probed (the null one is just omitted from the map).
	assert.deepEqual(calls, ["src-a", "src-b", "src-c"]);
});

test("resolveForwardedTokens skips a resolved-but-empty token", async () => {
	const calls: string[] = [];
	const invoke = mockResolveInvoke(
		{ "src-a": { token: "", origin: null } },
		calls,
	);
	const map = await resolveForwardedTokens(["src-a"], invoke);
	assert.deepEqual(map, {});
});

test("resolveForwardedTokens on no sources yields an empty map", async () => {
	const invoke = mockResolveInvoke({}, []);
	assert.deepEqual(await resolveForwardedTokens([], invoke), {});
});

// ─── boundSourceTokenMap (check-updates assembly) ───────────────────────────

test("boundSourceTokenMap enumerates bound sources then resolves each", async () => {
	const calls: string[] = [];
	const invoke = (async (cmd: string, args?: Record<string, unknown>) => {
		if (cmd === "list_bound_sources") {
			return ["bound-a", "bound-b", "bound-c"];
		}
		if (cmd === "resolve_git_token") {
			const source = String(args?.source);
			calls.push(source);
			const table: Record<string, ResolvedToken | null> = {
				"bound-a": { token: "A", origin: null },
				"bound-b": null, // bound but no token now -> skipped
				"bound-c": { token: "C", origin: null },
			};
			return table[source] ?? null;
		}
		throw new Error(`unexpected invoke: ${cmd}`);
	}) as InvokeFn;

	const map = await boundSourceTokenMap(invoke);

	assert.deepEqual(map, { "bound-a": "A", "bound-c": "C" });
	assert.deepEqual(calls, ["bound-a", "bound-b", "bound-c"]);
});

test("boundSourceTokenMap returns an empty map when nothing is bound", async () => {
	const invoke = (async (cmd: string) => {
		if (cmd === "list_bound_sources") return [];
		throw new Error(`unexpected invoke: ${cmd}`);
	}) as InvokeFn;

	const map = await boundSourceTokenMap(invoke);
	assert.deepEqual(map, {});
});

test("boundSourceTokenMap header is omitted (undefined) when no bound tokens resolve", async () => {
	const invoke = (async (cmd: string) => {
		if (cmd === "list_bound_sources") return ["bound-a"];
		if (cmd === "resolve_git_token") return null;
		throw new Error(`unexpected invoke: ${cmd}`);
	}) as InvokeFn;

	const map = await boundSourceTokenMap(invoke);
	assert.deepEqual(map, {});
	// And the header builder turns an empty map into "no header".
	assert.equal(gitTokensHeader(map), undefined);
});
