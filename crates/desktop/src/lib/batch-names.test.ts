import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { chunkNames, MAX_BATCH_NAMES } from "./batch-names.ts";

test("a batch-sized list stays one request", () => {
	const names = Array.from({ length: MAX_BATCH_NAMES }, (_, i) => `s${i}`);
	assert.equal(chunkNames(names).length, 1);
});

test("one over the cap splits rather than failing the whole update", () => {
	const names = Array.from(
		{ length: MAX_BATCH_NAMES + 1 },
		(_, i) => `s${i}`,
	);
	const chunks = chunkNames(names);
	assert.equal(chunks.length, 2);
	assert.equal(chunks[0]?.length, MAX_BATCH_NAMES);
	assert.equal(chunks[1]?.length, 1);
	// Every name is attempted exactly once, in request order.
	assert.deepEqual(chunks.flat(), names);
});

test("no names means no request", () => {
	assert.deepEqual(chunkNames([]), []);
});
