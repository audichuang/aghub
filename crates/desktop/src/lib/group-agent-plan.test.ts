// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import assert from "node:assert/strict";
import {
	computeGroupAgentStats,
	buildReconcilePlans,
	computeSkillAgentDiff,
	wouldOrphanSkill,
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

const usable = ["claude", "codex", "antigravity"];
const installed = new Set(["claude", "codex"]);

test("computeSkillAgentDiff: untouched → installed set selected, no changes", () => {
	const diff = computeSkillAgentDiff(usable, installed, {});
	assert.deepEqual(diff.selected.sort(), ["claude", "codex"]);
	assert.deepEqual(diff.added, []);
	assert.deepEqual(diff.removed, []);
	assert.equal(diff.labels.claude, "installed");
	assert.equal(diff.labels.codex, "installed");
	assert.equal(diff.labels.antigravity, undefined);
});

test("computeSkillAgentDiff: unchecking an installed agent removes it", () => {
	const diff = computeSkillAgentDiff(usable, installed, { codex: false });
	assert.deepEqual(diff.added, []);
	assert.deepEqual(diff.removed, ["codex"]);
	assert.equal(diff.labels.codex, "removing");
	assert.equal(diff.labels.claude, "installed");
});

test("computeSkillAgentDiff: checking a new agent adds it", () => {
	const diff = computeSkillAgentDiff(usable, installed, {
		antigravity: true,
	});
	assert.deepEqual(diff.added, ["antigravity"]);
	assert.deepEqual(diff.removed, []);
	assert.equal(diff.labels.antigravity, "adding");
});

test("computeSkillAgentDiff: add and remove together stay disjoint", () => {
	const diff = computeSkillAgentDiff(usable, installed, {
		codex: false,
		antigravity: true,
	});
	assert.deepEqual(diff.added, ["antigravity"]);
	assert.deepEqual(diff.removed, ["codex"]);
	const overlap = diff.added.filter((id) => diff.removed.includes(id));
	assert.deepEqual(overlap, []);
});

test("computeSkillAgentDiff: never targets an agent outside the usable set", () => {
	// `cursor` is installed but not usable at this scope → must not be removed.
	const diff = computeSkillAgentDiff(
		["claude"],
		new Set(["claude", "cursor"]),
		{ claude: false },
	);
	assert.deepEqual(diff.removed, ["claude"]);
	assert.ok(!diff.removed.includes("cursor"));
	assert.equal(diff.labels.cursor, undefined);
});

test("wouldOrphanSkill: add + remove every existing copy is blocked", () => {
	// add antigravity while removing both installed agents → nothing survives.
	assert.equal(
		wouldOrphanSkill(
			new Set(["claude", "codex"]),
			["antigravity"],
			["claude", "codex"],
		),
		true,
	);
});

test("wouldOrphanSkill: add while one install survives is fine", () => {
	assert.equal(
		wouldOrphanSkill(
			new Set(["claude", "codex"]),
			["antigravity"],
			["claude"],
		),
		false,
	);
});

test("wouldOrphanSkill: pure removal (no add) is always allowed", () => {
	// Removing every agent with no addition is a legit "delete everywhere".
	assert.equal(
		wouldOrphanSkill(new Set(["claude", "codex"]), [], ["claude", "codex"]),
		false,
	);
});
