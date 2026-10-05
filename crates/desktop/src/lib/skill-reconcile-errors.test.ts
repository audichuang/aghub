import assert from "node:assert/strict";
import { test } from "node:test";
import { HTTPError } from "ky";
import {
	failedReconcileRowsMessage,
	isGoneSkillPath,
	isWholeBatchRefusal,
} from "./skill-reconcile-errors.ts";

test("isGoneSkillPath returns true only for 404 HTTP errors", () => {
	const err404 = new HTTPError(
		new Response(null, { status: 404 }),
		new Request("http://x"),
		{} as any,
	);
	assert.equal(isGoneSkillPath(err404), true);

	const err500 = new HTTPError(
		new Response(null, { status: 500 }),
		new Request("http://x"),
		{} as any,
	);
	assert.equal(isGoneSkillPath(err500), false);

	const genericErr = new Error("something went wrong");
	assert.equal(isGoneSkillPath(genericErr), false);

	assert.equal(isGoneSkillPath(null), false);
	assert.equal(isGoneSkillPath(undefined), false);
});

test("failedReconcileRowsMessage formats only failed rows", () => {
	const agentName = (id: string) =>
		id === "claude" ? "Claude" : id === "codex" ? "Codex" : "Cursor";

	const results = [
		{ agent: "codex", success: true, error: null },
		{
			agent: "claude",
			success: false,
			error: "Resource not found: skill 'x'",
		},
		{ agent: "cursor", success: true, error: null },
	];

	const msg = failedReconcileRowsMessage(results, agentName);
	assert.ok(msg);
	assert.ok(msg.includes("Claude: Resource not found: skill 'x'"));
	assert.ok(!msg.includes("Codex"));
	assert.ok(!msg.includes("Cursor"));

	const allSuccess = [
		{ agent: "claude", success: true, error: null },
		{ agent: "codex", success: true, error: null },
	];
	assert.equal(failedReconcileRowsMessage(allSuccess, agentName), null);

	assert.equal(failedReconcileRowsMessage([], agentName), null);
});

test("isWholeBatchRefusal matches only the preflight refusal", () => {
	assert.equal(
		isWholeBatchRefusal(
			new Error(
				"skill reconcile preflight failed; nothing was written: delete codex (global): ...",
			),
		),
		true,
	);
	assert.equal(
		isWholeBatchRefusal(
			new Error("Cursor: 1 path(s) could not be deleted: /x/demo"),
		),
		false,
	);
	assert.equal(isWholeBatchRefusal("nothing was written"), false);
	assert.equal(isWholeBatchRefusal(null), false);
});
