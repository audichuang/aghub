import assert from "node:assert/strict";
import { test } from "node:test";
import { splitDeleteTargets } from "./skill-delete-targets.ts";

const items = [
	{ agent: "claude", source: "global" },
	{ agent: "cursor", source: "global" },
	{ agent: "opencode", source: "global" },
	{ agent: null, source: "global" },
];
const managed = new Set(["claude"]);

test("a disabled agent is not named unless the user opts in", () => {
	const { named, unmanaged } = splitDeleteTargets(items, managed, false);
	assert.deepEqual(
		named.map((item) => item.agent),
		["claude"],
	);
	assert.deepEqual(
		unmanaged.map((item) => item.agent),
		["cursor", "opencode"],
	);
});

test("opting in names the disabled agents too, managed rows first", () => {
	const { named } = splitDeleteTargets(items, managed, true);
	assert.deepEqual(
		named.map((item) => item.agent),
		["claude", "cursor", "opencode"],
	);
});

test("a row with no agent is never named", () => {
	const {
		named,
		managed: m,
		unmanaged,
	} = splitDeleteTargets(
		items,
		new Set(["claude", "cursor", "opencode"]),
		true,
	);
	assert.equal(named.length, 3);
	assert.equal(m.length, 3);
	assert.equal(unmanaged.length, 0);
});
