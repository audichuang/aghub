import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { MAX_BATCH_NAMES } from "../generated/dto/limits.ts";
import { chunkNames, sendInBatches } from "./batch-names.ts";

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

test("an oversized list becomes several requests, each within the cap", async () => {
	const all = names(MAX_BATCH_NAMES + 1);
	const sent: string[][] = [];
	const results = await sendInBatches(all, async (chunk) => {
		sent.push(chunk);
		return chunk.map((name) => ({ name, success: true }));
	});

	assert.equal(sent.length, 2, "one oversized request would be refused");
	assert.ok(sent.every((chunk) => chunk.length <= MAX_BATCH_NAMES));
	// Rows come back for every name, once, in request order — a caller counts
	// failures against this, so a dropped or duplicated batch misreports.
	assert.deepEqual(
		results.map((row) => row.name),
		all,
	);
});

test("a throw on a later chunk still delivers the rows earlier chunks returned", async () => {
	// The server writes each chunk before answering the next, so rows from a
	// chunk that succeeded describe skills that ARE updated on disk. Losing
	// them makes the caller report already-updated skills as failures.
	const all = names(MAX_BATCH_NAMES + 1);
	const delivered: { name: string; success: boolean }[] = [];
	let sent = 0;

	await assert.rejects(
		sendInBatches<{ name: string; success: boolean }>(
			all,
			async (chunk) => {
				sent += 1;
				if (sent > 1) throw new Error("403 Forbidden");
				return chunk.map((name) => ({ name, success: true }));
			},
			(rows) => delivered.push(...rows),
		),
		/403/,
	);

	assert.equal(sent, 2, "the second chunk must have been attempted");
	assert.equal(
		delivered.length,
		MAX_BATCH_NAMES,
		"the first chunk's rows must survive the second chunk's failure",
	);
	assert.deepEqual(
		delivered.map((row) => row.name),
		all.slice(0, MAX_BATCH_NAMES),
	);
});
