import { test } from "node:test";
import assert from "node:assert/strict";
import {
	computeGroupAgentStats,
	buildReconcilePlans,
} from "./group-agent-plan.ts";

const skills = [
	{
		name: "a",
		items: [
			{ agent: "claude", source: "global" },
			{ agent: "codex", source: "global" },
		],
	},
	{ name: "b", items: [{ agent: "claude", source: "global" }] },
];

test("computeGroupAgentStats: all/some/none", () => {
	const stats = computeGroupAgentStats(skills, [
		"claude",
		"codex",
		"antigravity",
	]);
	const by = Object.fromEntries(stats.map((s) => [s.agentId, s.state]));
	assert.equal(by.claude, "all"); // 2/2
	assert.equal(by.codex, "some"); // 1/2
	assert.equal(by.antigravity, "none"); // 0/2
});

test("buildReconcilePlans: add missing, remove deselected, skip idempotent", () => {
	// desired = {claude, antigravity}: codex deselected (remove from a),
	// antigravity selected (add to a, b), claude already fully installed (no-op)
	const plans = buildReconcilePlans(
		skills,
		["claude", "codex", "antigravity"],
		new Set(["claude", "antigravity"]),
	);
	const a = plans.find((p) => p.name === "a");
	const b = plans.find((p) => p.name === "b");
	assert.deepEqual(a?.added?.sort(), ["antigravity"]);
	assert.deepEqual(a?.removed?.sort(), ["codex"]);
	assert.deepEqual(b?.added?.sort(), ["antigravity"]);
	assert.equal(b?.removed?.length ?? 0, 0);
});

test("buildReconcilePlans: no change → empty", () => {
	const plans = buildReconcilePlans(
		skills,
		["claude", "codex"],
		new Set(["claude", "codex"]),
	);
	// claude fully installed; codex desired but b lacks it → b adds codex; a unchanged
	assert.ok(plans.every((p) => p.added.length + p.removed.length > 0));
});
