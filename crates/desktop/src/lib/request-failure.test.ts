import assert from "node:assert/strict";
// No FE test runner is installed here; pure logic uses Node's runner, matching
// the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { HTTPError, TimeoutError } from "ky";
import { describeRequestFailure } from "./request-failure.ts";

function httpError(status: number, message: string) {
	const request = new Request("http://127.0.0.1:1234/api/v1/skills");
	const error = new HTTPError(
		new Response(null, { status }),
		request,
		// ky only reads `retry`/`method` off this at construction.
		{ method: "POST" } as never,
	);
	error.message = message;
	return error;
}

test("a 4xx proves the mutation did not run, and carries the server message", () => {
	const view = describeRequestFailure(
		httpError(400, "confirm=true is required"),
	);
	assert.equal(view.definite, true);
	assert.equal(view.description, "confirm=true is required");
});

test("a 5xx proves nothing about whether the mutation ran", () => {
	assert.equal(
		describeRequestFailure(httpError(500, "boom")).definite,
		false,
	);
});

test("a timeout is never definite and never leaks its internal URL", () => {
	const view = describeRequestFailure(
		new TimeoutError(new Request("http://127.0.0.1:1234/api/v1/skills")),
	);
	assert.equal(view.definite, false);
	assert.equal(view.description, undefined);
});

test("a transport failure is never definite", () => {
	const view = describeRequestFailure(new TypeError("Failed to fetch"));
	assert.equal(view.definite, false);
	assert.equal(view.description, undefined);
});
