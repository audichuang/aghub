import assert from "node:assert/strict";
// No FE test runner is installed here; pure logic uses Node's runner, matching
// the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { queryKeys } from "./keys.ts";

// `updateChecksAll()` works as an invalidation/refetch target ONLY because it is
// a prefix of every concrete `updateChecks(...)` key. Reordering the concrete
// key's elements would silently stop the "Update available" badge from clearing
// after an applied update, with nothing else going red.
test("updateChecksAll prefixes every concrete update-check key", () => {
	for (const key of [
		queryKeys.skills.updateChecks(),
		queryKeys.skills.updateChecks("global"),
		queryKeys.skills.updateChecks("project", "/tmp/project"),
		queryKeys.skills.updateChecks("all"),
	]) {
		assert.deepEqual(
			key.slice(0, queryKeys.skills.updateChecksAll().length),
			[...queryKeys.skills.updateChecksAll()],
		);
	}
});
