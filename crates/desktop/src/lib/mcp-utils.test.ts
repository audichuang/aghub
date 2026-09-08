import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	buildTransportFromForm,
	formatArgs,
	getImportedMcpTransportType,
	parseArgs,
} from "./mcp-utils.ts";

const ROUND_TRIPS = [
	["hello world", "second"],
	["-y", "@modelcontextprotocol/server-filesystem"],
	["--dir", "/p q"],
	['a"b'],
	["--flag=a b", "'quoted'", "back\\slash"],
	["C:\\Users\\x"],
	["C:\\Program Files\\node.exe"],
	["a\"b'c"],
	['say "hi"', "it's"],
	[""],
	[],
];

for (const argv of ROUND_TRIPS) {
	test(`argv survives format→parse: ${JSON.stringify(argv)}`, () => {
		assert.deepEqual(parseArgs(formatArgs(argv)), argv);
	});
}

test("a hand-typed quoted arg keeps its boundary", () => {
	assert.deepEqual(parseArgs('"hello world" second'), [
		"hello world",
		"second",
	]);
});

// A bare backslash outside quotes is literal: a Windows path typed straight
// into the field must come back byte-for-byte.
test("a bare Windows path is not treated as escapes", () => {
	assert.deepEqual(parseArgs("C:\\Users\\x --flag"), [
		"C:\\Users\\x",
		"--flag",
	]);
});

test("a hand-typed quoted Windows path survives", () => {
	assert.deepEqual(parseArgs('"C:\\Program Files\\node.exe" --flag'), [
		"C:\\Program Files\\node.exe",
		"--flag",
	]);
});

test("plain space-separated args still split (the common paste)", () => {
	assert.deepEqual(parseArgs("  -y   pkg@1  "), ["-y", "pkg@1"]);
});

// The regression that mattered: opening an existing server in the edit form and
// saving it back without touching the args field must not change its argv.
test("edit round-trip leaves an untouched args field alone", () => {
	const original = ["--dir", "/p q", "--name", 'say "hi"'];
	const transport = buildTransportFromForm("stdio", {
		command: "npx",
		args: formatArgs(original),
	});
	assert.equal(transport?.type, "stdio");
	assert.deepEqual(
		transport?.type === "stdio" ? transport.args : null,
		original,
	);
});

// Every spelling an agent's own config may use for streamable HTTP must import
// as streamable_http, not silently downgrade to SSE.
for (const type of [
	"http",
	"streamable_http",
	"streamable-http",
	"streamableHttp",
]) {
	test(`import maps type "${type}" to streamable_http`, () => {
		assert.equal(
			getImportedMcpTransportType({
				type,
				url: "https://example.invalid/mcp",
			}),
			"streamable_http",
		);
	});
}

// Some agents key the transport as `transport`, as a bare string or as
// `{ type }`; pasting one of those configs must not downgrade it to SSE either.
for (const transport of ["http", "streamable-http"]) {
	test(`import maps transport "${transport}" to streamable_http`, () => {
		assert.equal(
			getImportedMcpTransportType({
				transport,
				url: "https://example.invalid/mcp",
			}),
			"streamable_http",
		);
	});
}

test("a nested transport object is read too", () => {
	assert.equal(
		getImportedMcpTransportType({
			transport: { type: "http" },
			url: "https://example.invalid/mcp",
		}),
		"streamable_http",
	);
});

// `type` is the dialect this form writes, so it wins over a foreign `transport`.
test("an explicit type outranks transport", () => {
	assert.equal(
		getImportedMcpTransportType({
			type: "sse",
			transport: "http",
			url: "https://example.invalid/sse",
		}),
		"sse",
	);
});

test("a url with no type is still SSE", () => {
	assert.equal(
		getImportedMcpTransportType({ url: "https://example.invalid/sse" }),
		"sse",
	);
});

test("an explicit sse type stays SSE", () => {
	assert.equal(
		getImportedMcpTransportType({
			type: "sse",
			url: "https://example.invalid/sse",
		}),
		"sse",
	);
});
