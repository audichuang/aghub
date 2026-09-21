import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { bulkFailureItemsLabel } from "./bulk-errors.ts";

test("bulkFailureItemsLabel includes target, agent, and failure reason", () => {
	assert.deepEqual(
		bulkFailureItemsLabel([
			{
				name: "nas-container",
				agent: "claude",
				error: "Blocked by security scan",
			},
		]),
		{
			count: 1,
			items: "nas-container (claude): Blocked by security scan",
		},
	);
});
