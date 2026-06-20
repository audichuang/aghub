import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed in this environment, and the
// task forbids adding dependencies, so this pure-logic test uses Node's
// built-in runner (`node --test --experimental-strip-types`). The antfu config
// enforces vitest over node:test, which does not apply here.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { remoteErrorMessage, remoteOutputSummary } from "./remote-errors.ts";

test("remoteErrorMessage: remoteDirectoryFailed with message", () => {
	assert.equal(
		remoteErrorMessage({
			kind: "remoteDirectoryFailed",
			message: "not a directory",
		}),
		"not a directory",
	);
});

test("remoteErrorMessage: remoteDirectoryFailed without message falls back", () => {
	assert.equal(
		remoteErrorMessage({ kind: "remoteDirectoryFailed" }),
		"Remote directory browsing failed.",
	);
});

test("remoteOutputSummary: strips ANSI and returns last non-empty line", () => {
	assert.equal(remoteOutputSummary("\x1B[31mfirst\x1B[0m\n\nlast\n"), "last");
});

test("remoteOutputSummary: returns last non-empty line from plain multiline string", () => {
	assert.equal(
		remoteOutputSummary("Host key failed\nBatchMode is set"),
		"BatchMode is set",
	);
});

// ---------------------------------------------------------------------------
// Exhaustive coverage: every known Rust RemoteError kind must produce a
// friendly (non-JSON) string. If a new kind is added on the Rust side without
// a matching case here, this test will catch the regression by asserting that
// the returned string is NOT the raw JSON.stringify of the payload.
// ---------------------------------------------------------------------------

const KNOWN_KINDS = [
	"unreachable",
	"remoteApiMissing",
	"incompatible",
	"crossPlatformRedeploy",
	"startTimeout",
	"tunnelFailed",
	"deployFailed",
	"remoteDirectoryFailed",
	"alreadyConnecting",
	"internal",
] as const;

for (const kind of KNOWN_KINDS) {
	test(`remoteErrorMessage: ${kind} is handled (not raw JSON)`, () => {
		// Minimal payload — only kind set so the default arm cannot
		// accidentally succeed via message/stderr/hint.
		const payload = { kind };
		const result = remoteErrorMessage(payload);
		// Must not stringify the bare payload (that is the default-arm
		// fallback, meaning the kind hit the default case).
		assert.notEqual(
			result,
			JSON.stringify(payload),
			`kind "${kind}" hit the default/stringify fallback — add an explicit case`,
		);
		// Must be a non-empty string.
		assert.ok(result.length > 0, `kind "${kind}" returned empty string`);
	});
}

test("remoteErrorMessage: default arm prefers message over stringify", () => {
	// An unknown kind with a message field should return the message,
	// not the JSON serialisation.
	const payload = {
		kind: "__unknown_future_kind__",
		message: "human readable",
	};
	assert.equal(remoteErrorMessage(payload), "human readable");
});

test("remoteErrorMessage: default arm prefers stderr when message absent", () => {
	const payload = { kind: "__unknown__", stderr: "stderr line" };
	assert.equal(remoteErrorMessage(payload), "stderr line");
});

test("remoteErrorMessage: default arm prefers hint when message+stderr absent", () => {
	const payload = { kind: "__unknown__", hint: "install hint" };
	assert.equal(remoteErrorMessage(payload), "install hint");
});

test("remoteErrorMessage: default arm falls back to JSON.stringify when no human fields", () => {
	const payload = { kind: "__unknown__" };
	assert.equal(remoteErrorMessage(payload), JSON.stringify(payload));
});
