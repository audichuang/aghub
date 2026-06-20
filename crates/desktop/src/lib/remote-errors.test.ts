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
