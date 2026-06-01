import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed in this environment, and the
// task forbids adding dependencies, so this pure-logic test uses Node's
// built-in runner (`node --test --experimental-strip-types`). The antfu config
// enforces vitest over node:test, which does not apply here.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	baseUrlFromPort,
	LOCAL_CONNECTION,
	mergeConnections,
	projectStatus,
	type QueryStateLike,
} from "./connection-logic.ts";

test("LOCAL_CONNECTION has id 'local'", () => {
	assert.equal(LOCAL_CONNECTION.id, "local");
	assert.equal(LOCAL_CONNECTION.label, "Local");
});

test("mergeConnections prepends LOCAL_CONNECTION", () => {
	const remotes = [
		{ id: "a", label: "VM A", sshTarget: "vm-a" },
		{ id: "b", label: "VM B", sshTarget: "vm-b" },
	];
	const merged = mergeConnections(remotes);
	assert.equal(merged.length, 3);
	assert.equal(merged[0], LOCAL_CONNECTION);
	assert.equal(merged[1].id, "a");
	assert.equal(merged[2].id, "b");
});

test("mergeConnections with no remotes yields just Local", () => {
	const merged = mergeConnections([]);
	assert.deepEqual(merged, [LOCAL_CONNECTION]);
});

test("baseUrlFromPort builds the api v1 url", () => {
	assert.equal(baseUrlFromPort(5173), "http://localhost:5173/api/v1");
	assert.equal(baseUrlFromPort(0), "http://localhost:0/api/v1");
});

function makeState(partial: Partial<QueryStateLike>): QueryStateLike {
	return {
		isError: false,
		isPending: false,
		isFetching: false,
		data: undefined,
		...partial,
	};
}

test("projectStatus: error wins", () => {
	assert.equal(
		projectStatus(makeState({ isError: true, data: 100 })),
		"error",
	);
});

test("projectStatus: resolved port => connected", () => {
	assert.equal(projectStatus(makeState({ data: 100 })), "connected");
});

test("projectStatus: pending without data => connecting", () => {
	assert.equal(projectStatus(makeState({ isPending: true })), "connecting");
});

test("projectStatus: fetching without data => connecting", () => {
	assert.equal(projectStatus(makeState({ isFetching: true })), "connecting");
});

test("projectStatus: idle when nothing in flight and no data", () => {
	assert.equal(projectStatus(makeState({})), "idle");
});

test("projectStatus: data 0 is a valid connected port", () => {
	// 0 should never be a real port here, but the projection keys on
	// `typeof data === 'number'`, so this documents the behavior.
	assert.equal(projectStatus(makeState({ data: 0 })), "connected");
});
