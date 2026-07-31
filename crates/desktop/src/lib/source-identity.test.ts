import assert from "node:assert/strict";
// No FE test runner is installed here; pure logic uses Node's runner, matching
// the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { matchesLockSource } from "./source-identity.ts";

test("an origin row matches the host-blind lock source it contains", () => {
	assert.equal(
		matchesLockSource("github.com/owner/repo", "owner/repo"),
		true,
	);
	assert.equal(
		matchesLockSource("git.example.com:8443/owner/repo", "owner/repo"),
		true,
	);
	// A local source's row identity is its own spelling, unchanged.
	assert.equal(matchesLockSource("/opt/skills/a", "/opt/skills/a"), true);
});

test("a different repository never matches", () => {
	assert.equal(
		matchesLockSource("github.com/owner/repo", "other/repo"),
		false,
	);
	assert.equal(
		matchesLockSource("github.com/owner/repo", "owner/other"),
		false,
	);
	// Must not match on a partial segment: `repo` is not `owner/repo`.
	assert.equal(matchesLockSource("github.com/owner/repo", "repo"), false);
});
