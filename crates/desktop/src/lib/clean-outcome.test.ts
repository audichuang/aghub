import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { cleanVerdict } from "./clean-outcome.ts";

// The whole point of the helper: `absent` must not read as cleaned. Flip that
// one arm back to "cleaned" and the reported bug returns — the row is counted
// as done while the lock entry that rebuilds it is still there.
test("absent is a lock-only leftover, never a successful clean", () => {
	assert.equal(cleanVerdict("absent"), "lock-only");
});

test("only removed counts as cleaned", () => {
	assert.equal(cleanVerdict("removed"), "cleaned");
});

// `kept` is `success: true` on the wire. Reading the boolean instead of the
// outcome is exactly how a still-installed skill got reported as cleaned.
test("kept and partial are still installed, despite success: true", () => {
	assert.equal(cleanVerdict("kept"), "still-installed");
	assert.equal(cleanVerdict("partial"), "still-installed");
});

test("an unknown or missing outcome is never treated as cleaned", () => {
	assert.equal(cleanVerdict("preview"), "still-installed");
	assert.equal(cleanVerdict(undefined), "still-installed");
	assert.equal(cleanVerdict(null), "still-installed");
});
