import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { filterGroupsByAgent } from "./skill-agent-filter.ts";

const groups = [
	{
		name: "shared",
		items: [{ agent: "claude" }, { agent: "codex" }, { agent: "cursor" }],
	},
	{ name: "claude-only", items: [{ agent: "claude" }] },
	{ name: "codex-only", items: [{ agent: "codex" }] },
];

test("selects the groups an agent reads", () => {
	assert.deepEqual(
		filterGroupsByAgent(groups, "claude").map((g) => g.name),
		["shared", "claude-only"],
	);
	assert.deepEqual(
		filterGroupsByAgent(groups, null).map((g) => g.name),
		["shared", "claude-only", "codex-only"],
	);
});

test("a surviving group keeps EVERY member, not just the filtered agent", () => {
	// Narrowing `items` here would make the manage-agents dialog show a skill
	// installed for three agents as belonging to one — and that dialog writes.
	const [shared] = filterGroupsByAgent(groups, "claude");
	assert.equal(shared.items.length, 3);
	assert.deepEqual(
		shared.items.map((i) => i.agent),
		["claude", "codex", "cursor"],
	);
});
