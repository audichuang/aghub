import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { MAX_BATCH_NAMES } from "../generated/dto/limits.ts";
import { chunkNames } from "./batch-names.ts";

const names = (count: number) =>
	Array.from({ length: count }, (_, index) => `skill-${index}`);

test("a batch-sized list stays one request", () => {
	assert.equal(chunkNames(names(MAX_BATCH_NAMES)).length, 1);
});

test("one over the cap splits rather than failing the whole update", () => {
	const all = names(MAX_BATCH_NAMES + 1);
	const chunks = chunkNames(all);
	assert.equal(chunks.length, 2);
	assert.ok(
		chunks.every((chunk) => chunk.length <= MAX_BATCH_NAMES),
		"no chunk may exceed what the server accepts",
	);
	// Every name is attempted exactly once, in request order.
	assert.deepEqual(chunks.flat(), all);
});

test("no names means no request", () => {
	assert.deepEqual(chunkNames([]), []);
});
