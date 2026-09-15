import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { TransportDto } from "../generated/dto";
import type { DeepLinkImportIntent } from "./deep-link.ts";
import { formatTransportSummary, parseDeepLink } from "./deep-link.ts";

/**
 * `parseDeepLink` is a trust boundary: the OS hands it a URL any application
 * (or web page) can mint, and a `type=mcp` payload decodes straight into an
 * MCP server definition carrying `command`, `args` and `env`. `isTransportDto`
 * is the only thing between that payload and the install modal, and none of it
 * was covered — a loosened check there is invisible to typecheck (the payload
 * is `unknown` until the predicate narrows it) and to lint.
 */

/** Build the `payload` query value the way a deep link carries it. */
function encodePayload(value: unknown): string {
	return Buffer.from(JSON.stringify(value), "utf8")
		.toString("base64")
		.replaceAll("+", "-")
		.replaceAll("/", "_")
		.replace(/=+$/, "");
}

function mcpLink(value: unknown): string {
	return `aghub://import?type=mcp&payload=${encodePayload(value)}`;
}

/** Assert the parse succeeded and hand back the narrowed intent. */
function intentOf(rawUrl: string): DeepLinkImportIntent {
	const result = parseDeepLink(rawUrl);
	assert.equal(result.ok, true, `expected ${rawUrl} to parse`);
	if (!result.ok) throw new Error("unreachable");
	return result.intent;
}

/** A payload object as it travels over the wire: untyped until validated. */
const STDIO_PAYLOAD: unknown = {
	type: "stdio",
	command: "npx",
	args: ["-y", "srv"],
};

// ── the envelope ────────────────────────────────────────────────────────────

test("a string that is not a URL is rejected", () => {
	assert.deepEqual(parseDeepLink("not a url"), {
		ok: false,
		error: "deepLinkInvalidUrl",
	});
});

test("only the aghub: scheme and the import route are accepted", () => {
	for (const raw of [
		// Wrong route, right scheme.
		"aghub://export?type=skill&source=o/r&name=a",
		"aghub://import-evil?type=skill&source=o/r&name=a",
		// Wrong scheme, right route — the host IS "import", so these isolate
		// the protocol check. Without them the scheme guard can be deleted
		// outright and every case above still passes on the route check alone.
		"https://import/?type=skill&source=o/r&name=a",
		"http://import/?type=skill&source=o/r&name=a",
		"file://import/?type=skill&source=o/r&name=a",
		"javascript://import/?type=skill&source=o/r&name=a",
		// Wrong scheme AND wrong route.
		"https://example.com/import?type=skill&source=o/r&name=a",
	]) {
		assert.deepEqual(
			parseDeepLink(raw),
			{ ok: false, error: "deepLinkUnsupported" },
			raw,
		);
	}
});

test("the route is read from the host or the path", () => {
	// `aghub://import?…` puts it in hostname, `aghub:/import?…` in pathname.
	assert.equal(
		intentOf("aghub://import?type=skill&source=o/r&name=alpha").kind,
		"skill-market-install",
	);
	assert.equal(
		intentOf("aghub:/import?type=skill&source=o/r&name=alpha").kind,
		"skill-market-install",
	);
});

test("an unknown type is rejected by name", () => {
	assert.deepEqual(parseDeepLink("aghub://import?type=plugin"), {
		ok: false,
		error: "deepLinkUnsupportedType",
	});
});

// ── skills ──────────────────────────────────────────────────────────────────

const SKILL_URL =
	"aghub://import?type=skill&source=%20owner%2Frepo%20&name=%20alpha%20&title=%20T%20&author=A&description=D";

test("a skill link carries its trimmed fields", () => {
	assert.deepEqual(intentOf(SKILL_URL), {
		kind: "skill-market-install",
		rawUrl: SKILL_URL,
		source: "owner/repo",
		name: "alpha",
		title: "T",
		author: "A",
		description: "D",
	});
});

test("a skill link needs both source and name", () => {
	for (const raw of [
		"aghub://import?type=skill&name=alpha",
		"aghub://import?type=skill&source=o/r",
		"aghub://import?type=skill&source=%20%20&name=alpha",
		"aghub://import?type=skill&source=o/r&name=%20%20",
		"aghub://import?type=skill",
	]) {
		assert.deepEqual(
			parseDeepLink(raw),
			{ ok: false, error: "deepLinkInvalidSkill" },
			raw,
		);
	}
});

test("blank optional fields become undefined, not empty strings", () => {
	const intent = intentOf(
		"aghub://import?type=skill&source=o/r&name=a&title=%20&author=",
	);
	assert.equal(intent.kind, "skill-market-install");
	if (intent.kind !== "skill-market-install") return;
	assert.equal(intent.title, undefined);
	assert.equal(intent.author, undefined);
});

// ── MCP payloads: the part that installs an executable ──────────────────────

test("a well-formed stdio payload round-trips", () => {
	const intent = intentOf(
		mcpLink({ name: "srv", transport: STDIO_PAYLOAD, timeout: 30 }),
	);
	assert.equal(intent.kind, "mcp-config-install");
	if (intent.kind !== "mcp-config-install") return;
	assert.equal(intent.name, "srv");
	assert.deepEqual(intent.transport, STDIO_PAYLOAD);
	assert.equal(intent.timeout, 30);
});

test("both remote transports are accepted with a url", () => {
	for (const type of ["sse", "streamable_http"]) {
		const intent = intentOf(
			mcpLink({
				name: "r",
				transport: { type, url: "https://e.test/mcp" },
			}),
		);
		assert.equal(intent.kind, "mcp-config-install");
		if (intent.kind !== "mcp-config-install") continue;
		assert.equal(intent.transport.type, type);
	}
});

// THE guard. Every one of these is a payload an attacker-controlled link can
// mint; each must be refused rather than handed to the install modal.
test("a malformed transport is refused, never installed", () => {
	const bad: Array<[string, unknown]> = [
		["no transport at all", { name: "x" }],
		["no name", { transport: STDIO_PAYLOAD }],
		["non-string name", { name: 1, transport: STDIO_PAYLOAD }],
		[
			"unknown transport type",
			{ name: "x", transport: { type: "exec", command: "sh" } },
		],
		["missing type", { name: "x", transport: { command: "sh" } }],
		["stdio with no command", { name: "x", transport: { type: "stdio" } }],
		[
			"stdio with non-string command",
			{ name: "x", transport: { type: "stdio", command: ["sh"] } },
		],
		[
			"stdio args not an array",
			{
				name: "x",
				transport: { type: "stdio", command: "sh", args: "-c" },
			},
		],
		[
			"stdio args holding a non-string",
			{
				name: "x",
				transport: { type: "stdio", command: "sh", args: ["-c", 1] },
			},
		],
		[
			"stdio env holding a non-string",
			{
				name: "x",
				transport: { type: "stdio", command: "sh", env: { A: 1 } },
			},
		],
		["remote with no url", { name: "x", transport: { type: "sse" } }],
		[
			"remote with a non-string url",
			{ name: "x", transport: { type: "sse", url: 7 } },
		],
		[
			"remote headers holding a non-string",
			{
				name: "x",
				transport: {
					type: "sse",
					url: "https://e.test",
					headers: { A: [] },
				},
			},
		],
		[
			"non-number timeout",
			{ name: "x", transport: STDIO_PAYLOAD, timeout: "30" },
		],
		["transport is null", { name: "x", transport: null }],
		["payload is an array", [{ name: "x", transport: STDIO_PAYLOAD }]],
		["payload is a bare string", "whatever"],
	];
	for (const [label, value] of bad) {
		assert.deepEqual(
			parseDeepLink(mcpLink(value)),
			{ ok: false, error: "deepLinkInvalidMcp" },
			label,
		);
	}
});

// Characterization, not endorsement: `isTransportDto` asserts `value is
// TransportDto`, but TransportDto REQUIRES `args`/`env`/`timeout` (stdio) and
// `headers`/`timeout` (remote) — nullable, yet present. The predicate treats
// them as optional, so a partial payload is narrowed to a type it does not
// satisfy and the consumer reads `undefined` where TS promises a value.
// Nothing breaks today (`formatTransportSummary` guards `args`, and the API
// re-validates), and tightening it would reject links people already mint —
// so this pins what the code DOES, and is the place to change if that call is
// ever revisited.
test("a partial-but-typed transport is accepted on purpose (see comment)", () => {
	const intent = intentOf(
		mcpLink({ name: "x", transport: { type: "stdio", command: "sh" } }),
	);
	assert.equal(intent.kind, "mcp-config-install");
	if (intent.kind !== "mcp-config-install") return;
	assert.deepEqual(intent.transport, { type: "stdio", command: "sh" });
});

test("an mcp link needs a payload, and garbage never throws", () => {
	for (const raw of [
		"aghub://import?type=mcp",
		"aghub://import?type=mcp&payload=",
		"aghub://import?type=mcp&payload=%20",
		"aghub://import?type=mcp&payload=!!!not-base64!!!",
		"aghub://import?type=mcp&payload=bm90IGpzb24", // valid base64, not JSON
	]) {
		assert.deepEqual(
			parseDeepLink(raw),
			{ ok: false, error: "deepLinkInvalidMcp" },
			raw,
		);
	}
});

test("base64url payloads decode without standard-base64 padding", () => {
	// `-` and `_` must map back to `+` and `/`, and the padding is re-added —
	// a payload whose length is not a multiple of 4 must still decode.
	const intent = intentOf(
		mcpLink({ name: "ünï—çode", transport: STDIO_PAYLOAD }),
	);
	assert.equal(intent.kind, "mcp-config-install");
	if (intent.kind !== "mcp-config-install") return;
	assert.equal(intent.name, "ünï—çode");
});

// ── display helper ──────────────────────────────────────────────────────────

test("formatTransportSummary shows the command line or the url", () => {
	const stdio = (args: string[]): TransportDto => ({
		type: "stdio",
		command: "srv",
		args,
		env: null,
		timeout: null,
	});
	assert.equal(
		formatTransportSummary({
			type: "stdio",
			command: "npx",
			args: ["-y", "srv"],
			env: null,
			timeout: null,
		}),
		"npx -y srv",
	);
	assert.equal(formatTransportSummary(stdio([])), "srv");
	assert.equal(
		formatTransportSummary({
			type: "sse",
			url: "https://e.test/mcp",
			headers: null,
			timeout: null,
		}),
		"https://e.test/mcp",
	);
	assert.equal(
		formatTransportSummary({
			type: "streamable_http",
			url: "https://e.test/h",
			headers: null,
			timeout: null,
		}),
		"https://e.test/h",
	);
});
