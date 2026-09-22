import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
import { test } from "node:test";
import { canStartUpdateWork, downloadPercent } from "./app-update.ts";

// Tauri's `Started` event carries `contentLength?`, so "unknown total" is a
// normal state. Returning 0 there shows a bar frozen at the left for a
// download that is really moving; NaN renders as "NaN%".
test("an unknown or zero total is indeterminate, not zero", () => {
	assert.equal(downloadPercent(1024, undefined), null);
	assert.equal(downloadPercent(1024, 0), null);
	assert.equal(downloadPercent(0, undefined), null);
});

test("a known total rounds to a whole percent", () => {
	assert.equal(downloadPercent(0, 1000), 0);
	assert.equal(downloadPercent(504, 1000), 50);
	assert.equal(downloadPercent(1000, 1000), 100);
});

// A proxy can deliver more bytes than it announced; "103%" reads as a bug in
// the updater rather than as a nearly-finished download.
test("more bytes than announced still reports 100", () => {
	assert.equal(downloadPercent(1300, 1000), 100);
});

// THE finding: the panel's mutations were local, so leaving the page destroyed
// the observer while the download continued, and coming back offered a button
// that started a SECOND download of the same update.
test("work cannot start while a check or download is already running", () => {
	assert.equal(canStartUpdateWork("checking"), false);
	assert.equal(canStartUpdateWork("downloading"), false);
});

// Terminal: the bytes are staged and only a restart is left.
test("work cannot start once an update is installed", () => {
	assert.equal(canStartUpdateWork("installed"), false);
});

test("an idle, checked, available or failed flow can start work", () => {
	assert.equal(canStartUpdateWork("idle"), true);
	assert.equal(
		canStartUpdateWork("up-to-date"),
		true,
		"'Check again' has to work",
	);
	assert.equal(canStartUpdateWork("available"), true);
	assert.equal(
		canStartUpdateWork("error"),
		true,
		"a failed check or download must be retryable",
	);
});
