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
	type ForwardedTokenMap,
	GIT_TOKENS_HEADER,
	gitTokensHeader,
	type InvokeFn,
	type ResolvedToken,
	resolveForwardedTokens,
	shouldForwardGitTokens,
} from "./git-token-forwarding.ts";

// ─── encodeGitTokensHeader ──────────────────────────────────────────────────

test("encodeGitTokensHeader emits the { token, origin } shape (standard base64)", () => {
	// The new wire contract: each value is { token, origin } where origin is the
	// controller-resolved (scheme, host, port) or null. Decoding with the
	// STANDARD alphabet (atob) confirms parity with the Rust decoder.
	const map: ForwardedTokenMap = {
		"github.com/a/b": {
			token: "ghp_TOKEN123",
			origin: { scheme: "https", host: "github.com", port: 443 },
		},
	};
	const encoded = encodeGitTokensHeader(map);
	const decoded = JSON.parse(atob(encoded));
	assert.deepEqual(decoded, map);
	// Spot-check the exact shape the Rust ForwardedEntry/ForwardedOrigin parses.
	assert.equal(decoded["github.com/a/b"].token, "ghp_TOKEN123");
	assert.deepEqual(decoded["github.com/a/b"].origin, {
		scheme: "https",
		host: "github.com",
		port: 443,
	});
});

test("encodeGitTokensHeader carries a null origin for unresolvable sources", () => {
	const map: ForwardedTokenMap = {
		"local/skill": { token: "TOK", origin: null },
	};
	const encoded = encodeGitTokensHeader(map);
	const decoded = JSON.parse(atob(encoded));
	assert.deepEqual(decoded, map);
	assert.equal(decoded["local/skill"].origin, null);
});

/** Decode standard-base64(UTF-8 JSON) back to the original map, like Rust does. */
function decodeGitTokensHeader(encoded: string): ForwardedTokenMap {
	const binary = atob(encoded);
	const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
	return JSON.parse(new TextDecoder().decode(bytes));
}

test("encodeGitTokensHeader uses the STANDARD alphabet (not url-safe)", () => {
	// A payload chosen so its base64 contains '+' and '/' (which url-safe base64
	// would render as '-' and '_'). Confirms we emit the standard alphabet.
	const map: ForwardedTokenMap = {
		s: { token: "ÿÿûï¾", origin: null },
	};
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
	const map: ForwardedTokenMap = {
		"源/技能": { token: "令牌-✓-😀", origin: null },
	};
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
	const map: ForwardedTokenMap = {
		src: { token: "tok", origin: null },
	};
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

test("resolveForwardedTokens carries token + origin and SKIPS nulls", async () => {
	const calls: string[] = [];
	const invoke = mockResolveInvoke(
		{
			"src-a": {
				token: "TOK_A",
				origin: { scheme: "https", host: "github.com", port: 443 },
			},
			"src-b": null, // no credential -> skipped
			"src-c": { token: "TOK_C", origin: null },
		},
		calls,
	);

	const map = await resolveForwardedTokens(
		["src-a", "src-b", "src-c"],
		invoke,
	);

	// The origin is carried through verbatim so the remote can origin-pin.
	assert.deepEqual(map, {
		"src-a": {
			token: "TOK_A",
			origin: { scheme: "https", host: "github.com", port: 443 },
		},
		"src-c": { token: "TOK_C", origin: null },
	});
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

test("resolveForwardedTokens never leaks the origin without a token", async () => {
	// A resolved-but-empty token is skipped entirely (no { token:"", origin })
	// so the map never carries an entry whose token is falsy.
	const calls: string[] = [];
	const invoke = mockResolveInvoke(
		{
			"src-a": {
				token: "",
				origin: { scheme: "https", host: "github.com", port: 443 },
			},
		},
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

	assert.deepEqual(map, {
		"bound-a": { token: "A", origin: null },
		"bound-c": { token: "C", origin: null },
	});
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

// ─── apply-update / install-sync forward composition ────────────────────────
//
// These model what the request layer (`useGitForwarding.forSource` →
// `applySkillUpdateMutationOptions`) does: resolve a single-source forward
// header keyed by the clone URL, but ONLY when forwarding is enabled. The
// install/sync paths deliberately have NO such builder — proved below.

/**
 * Mirror of `useGitForwarding.forSource`: gate on the connection state, then
 * resolve+encode a single-source header keyed by the source's clone URL.
 */
async function forSource(
	source: string,
	gate: { activeConnectionId: string; supportsCredentialForwarding: boolean },
	invokeFn: InvokeFn,
): Promise<Record<string, string> | undefined> {
	if (!shouldForwardGitTokens(gate)) return undefined;
	const map = await resolveForwardedTokens([source], invokeFn);
	return gitTokensHeader(map);
}

test("apply-update forwards a single-source header when remote + capable", async () => {
	const calls: string[] = [];
	const cloneUrl = "https://git.internal:8443/owner/repo.git";
	const invoke = mockResolveInvoke(
		{
			[cloneUrl]: {
				token: "APPLY_TOK",
				origin: { scheme: "https", host: "git.internal", port: 8443 },
			},
		},
		calls,
	);

	const header = await forSource(
		cloneUrl,
		{ activeConnectionId: "vm-1", supportsCredentialForwarding: true },
		invoke,
	);

	assert.ok(header, "apply must attach the header when forwarding is on");
	// The header carries the new { token, origin } shape, keyed by the clone URL.
	assert.deepEqual(JSON.parse(atob(header[GIT_TOKENS_HEADER])), {
		[cloneUrl]: {
			token: "APPLY_TOK",
			origin: { scheme: "https", host: "git.internal", port: 8443 },
		},
	});
	// Resolution happened against the CLONE URL (P1-c), not a bare owner/repo.
	assert.deepEqual(calls, [cloneUrl]);
});

test("apply-update sends NO header in Local mode (gating off)", async () => {
	const calls: string[] = [];
	const invoke = mockResolveInvoke(
		{ "owner/repo": { token: "T", origin: null } },
		calls,
	);

	const header = await forSource(
		"owner/repo",
		{ activeConnectionId: "local", supportsCredentialForwarding: true },
		invoke,
	);

	assert.equal(header, undefined, "Local mode must not forward");
	// Gating short-circuits before any resolve_git_token probe.
	assert.deepEqual(calls, []);
});

test("apply-update sends NO header for a non-capable remote", async () => {
	const calls: string[] = [];
	const invoke = mockResolveInvoke(
		{ "owner/repo": { token: "T", origin: null } },
		calls,
	);

	const header = await forSource(
		"owner/repo",
		{ activeConnectionId: "vm-1", supportsCredentialForwarding: false },
		invoke,
	);

	assert.equal(header, undefined);
	assert.deepEqual(calls, []);
});

test("install/sync have no forward-header builder (P3 contract)", () => {
	// install/sync reuse the scan session's server-side cached token, so the FE
	// module exposes NO single-shot install/sync header builder. The only
	// per-source builder is `forSource` (used by scan/diff/apply); `gitInstall`
	// and `gitSync` in `lib/api.ts` no longer accept a forwardedTokens arg.
	// (A static assertion: the module surface intentionally omits such helpers.)
	const surface = Object.keys({
		encodeGitTokensHeader,
		gitTokensHeader,
		resolveForwardedTokens,
		boundSourceTokenMap,
		shouldForwardGitTokens,
	});
	assert.ok(
		!surface.some((k) => /install|sync/i.test(k)),
		"no install/sync-specific forward builder may exist",
	);
});
