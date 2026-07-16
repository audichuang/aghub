import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	buildCoverageRows,
	groupResourcesByName,
	planCellToggle,
} from "./coverage-matrix.ts";

test("groupResourcesByName collapses per-agent rows and drops unusable agents", () => {
	const rows = groupResourcesByName(
		[
			{ name: "b", agent: "claude" },
			{ name: "a", agent: "claude" },
			{ name: "a", agent: "cursor" },
			{ name: "a", agent: "ghost" }, // not in usable set → dropped
		],
		["claude", "cursor"],
	);
	// Sorted by name; "ghost" install dropped but the resource still appears.
	assert.deepEqual(
		rows.map((r) => r.name),
		["a", "b"],
	);
	assert.deepEqual([...rows[0].installedAgents].sort(), ["claude", "cursor"]);
	assert.deepEqual([...rows[1].installedAgents], ["claude"]);
});

test("buildCoverageRows marks installed and applicable per cell", () => {
	const rows = buildCoverageRows(
		[{ name: "a", installedAgents: ["claude"] }],
		["claude", "cursor", "codex"],
		new Set(["claude", "cursor"]), // codex cannot carry this kind
	);
	const cellByAgent = new Map(rows[0].cells.map((c) => [c.agentId, c]));
	assert.deepEqual(cellByAgent.get("claude"), {
		agentId: "claude",
		applicable: true,
		installed: true,
	});
	assert.deepEqual(cellByAgent.get("cursor"), {
		agentId: "cursor",
		applicable: true,
		installed: false,
	});
	assert.deepEqual(cellByAgent.get("codex"), {
		agentId: "codex",
		applicable: false,
		installed: false,
	});
});

test("planCellToggle adds a missing agent from an existing source", () => {
	const plan = planCellToggle(new Set(["claude"]), "cursor");
	assert.deepEqual(plan, {
		kind: "reconcile",
		sourceAgent: "claude",
		added: ["cursor"],
		removed: [],
	});
});

test("planCellToggle removes an installed agent when others survive", () => {
	const plan = planCellToggle(new Set(["claude", "cursor"]), "cursor");
	assert.equal(plan.kind, "reconcile");
	if (plan.kind !== "reconcile") return;
	assert.deepEqual(plan.removed, ["cursor"]);
	assert.deepEqual(plan.added, []);
	// Source must be a surviving install, never the agent being removed.
	assert.equal(plan.sourceAgent, "claude");
});

test("planCellToggle BLOCKS removing the only remaining install (no reconcile)", () => {
	// The data-safety guard: a single stray click must not fully uninstall a
	// resource. This test fails if the guard regresses to a reconcile plan.
	const plan = planCellToggle(new Set(["claude"]), "claude");
	assert.deepEqual(plan, { kind: "blocked", reason: "last-install" });
});

test("planCellToggle is a no-op when the resource has no install to copy from", () => {
	const plan = planCellToggle(new Set(), "claude");
	assert.deepEqual(plan, { kind: "noop" });
});
