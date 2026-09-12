import assert from "node:assert/strict";
// This project uses Node's built-in test runner.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { findSkillCollisions } from "./skill-collision.ts";

test("reports selected same-name skills with their locations", () => {
	const result = findSkillCollisions(
		["agent-reach", "other"],
		[
			{
				name: "agent-reach",
				agent: "openclaw",
				source_path: "~/.openclaw/skills/agent-reach",
			},
			{
				name: "agent-reach",
				agent: "claude",
				source_path: "~/.claude/skills/agent-reach",
			},
			{ name: "other", agent: "cursor", source_path: null },
		],
	);

	assert.deepEqual(
		result.map(({ name, locations }) => [name, locations.length]),
		[
			["agent-reach", 2],
			["other", 1],
		],
	);
	assert.equal(result[0]?.requiresAdvisory, true);
});

test("does not infer a collision from unselected or duplicate rows", () => {
	const row = {
		name: "agent-reach",
		agent: "openclaw",
		source_path: "~/.openclaw/skills/agent-reach",
	};
	assert.deepEqual(findSkillCollisions(["different"], [row]), []);
	assert.equal(
		findSkillCollisions(["agent-reach"], [row, row])[0]?.locations.length,
		1,
	);
	assert.equal(
		findSkillCollisions(
			["agent-reach"],
			[{ ...row, canonical_path: "/tmp/.aghub/agent-reach" }],
		)[0]?.requiresAdvisory,
		false,
	);
});
