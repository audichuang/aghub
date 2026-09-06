import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	eligibleAgentIds,
	linkAttemptOutcome,
	newToolPromptDelta,
	reconcileAddsForNewAgents,
	sawLockedSkills,
	type NewToolPromptAgent,
} from "./new-tool-prompt.ts";

function agent(
	over: Partial<NewToolPromptAgent> & Pick<NewToolPromptAgent, "id">,
): NewToolPromptAgent {
	return {
		isAvailable: true,
		isDisabled: false,
		skillMutableGlobal: true,
		needsLink: true,
		...over,
	};
}

test("missing lastKnown is seed-only and does not prompt", () => {
	const result = newToolPromptDelta({
		lastKnown: null,
		agents: [
			agent({ id: "claude" }),
			agent({ id: "cursor", needsLink: false }),
		],
	});
	assert.equal(result.kind, "seedOnly");
	assert.deepEqual(result.seed, ["claude"]);
});

test("NativeReader cursor is excluded from the prompt set", () => {
	const result = newToolPromptDelta({
		lastKnown: [],
		agents: [
			agent({ id: "cursor", needsLink: false }),
			agent({ id: "claude" }),
		],
	});
	assert.equal(result.kind, "prompt");
	if (result.kind !== "prompt") return;
	assert.deepEqual(result.ids, ["claude"]);
	assert.deepEqual(result.seed, ["claude"]);
});

test("disabled agents are excluded", () => {
	const result = newToolPromptDelta({
		lastKnown: [],
		agents: [agent({ id: "claude", isDisabled: true })],
	});
	assert.equal(result.kind, "quiet");
	assert.deepEqual(result.seed, []);
});

test("unavailable or non-mutable agents are excluded", () => {
	assert.deepEqual(
		eligibleAgentIds([
			agent({ id: "gone", isAvailable: false }),
			agent({ id: "zed", skillMutableGlobal: false }),
		]),
		[],
	);
});

test("uninstall then reappear prompts again", () => {
	const withClaude = [agent({ id: "claude" })];
	const first = newToolPromptDelta({
		lastKnown: ["claude"],
		agents: withClaude,
	});
	assert.equal(first.kind, "quiet");
	assert.deepEqual(first.seed, ["claude"]);

	const uninstalled = newToolPromptDelta({
		lastKnown: first.seed,
		agents: [],
	});
	assert.equal(uninstalled.kind, "quiet");
	assert.deepEqual(uninstalled.seed, []);

	const reappeared = newToolPromptDelta({
		lastKnown: uninstalled.seed,
		agents: withClaude,
	});
	assert.equal(reappeared.kind, "prompt");
	if (reappeared.kind !== "prompt") return;
	assert.deepEqual(reappeared.ids, ["claude"]);
});

test("reconcileAddsForNewAgents skips agents that already hold the skill", () => {
	const plans = reconcileAddsForNewAgents(
		[
			{ name: "pdf", agent: "codex" },
			{ name: "pdf", agent: "claude" },
			{ name: "pptx", agent: "codex" },
		],
		["claude", "kilocode"],
		new Set(["pdf", "pptx"]),
	);
	assert.deepEqual(plans, [
		{ name: "pdf", sourceAgent: "codex", added: ["kilocode"] },
		{
			name: "pptx",
			sourceAgent: "codex",
			added: ["claude", "kilocode"],
		},
	]);
});

test("a skill the lock does not own is never reconciled", () => {
	// A hand-made ~/.claude/skills/private-notes shows up in discovery but is
	// not managed by aghub. Reconciling it would promote it into the shared
	// Master and link it to other agents — a migration nobody asked for.
	const plans = reconcileAddsForNewAgents(
		[
			{ name: "pdf", agent: "codex" },
			{ name: "private-notes", agent: "claude" },
		],
		["kilocode"],
		new Set(["pdf"]),
	);
	assert.deepEqual(plans, [
		{ name: "pdf", sourceAgent: "codex", added: ["kilocode"] },
	]);
});

test("an empty lock produces no plans at all", () => {
	assert.deepEqual(
		reconcileAddsForNewAgents(
			[{ name: "private-notes", agent: "claude" }],
			["kilocode"],
			new Set(),
		),
		[],
	);
});

test("an agent that already reads every managed skill is a success, not an error", () => {
	// omp reads `~/.agents/skills` on top of its own dir, so discovery already
	// lists every lock-owned skill under it and no plan has anything to add.
	// Treating that as an error left the modal open forever: only a success
	// advances `lastKnown`.
	const skills = [
		{ name: "pdf", agent: "omp" },
		{ name: "pptx", agent: "omp" },
	];
	const locked = new Set(["pdf", "pptx"]);
	const plans = reconcileAddsForNewAgents(skills, ["omp"], locked);
	assert.deepEqual(plans, []);
	assert.equal(
		linkAttemptOutcome({
			lockedCount: locked.size,
			attempted: 0,
			failed: 0,
			sawLockedSkills: sawLockedSkills(skills, locked),
		}),
		"success",
	);
});

test("no discovery row for any locked skill stays retryable", () => {
	const locked = new Set(["pdf"]);
	assert.equal(
		linkAttemptOutcome({
			lockedCount: locked.size,
			attempted: 0,
			failed: 0,
			sawLockedSkills: sawLockedSkills([], locked),
		}),
		"retry",
	);
});

test("a discovery row without an agent is not a usable source", () => {
	assert.equal(
		sawLockedSkills([{ name: "pdf", agent: null }], new Set(["pdf"])),
		false,
	);
});

test("link outcomes report failure and partial failure", () => {
	const base = { lockedCount: 3, sawLockedSkills: true };
	assert.equal(
		linkAttemptOutcome({ ...base, attempted: 3, failed: 3 }),
		"failed",
	);
	assert.equal(
		linkAttemptOutcome({ ...base, attempted: 3, failed: 1 }),
		"partial",
	);
	assert.equal(
		linkAttemptOutcome({ ...base, attempted: 3, failed: 0 }),
		"success",
	);
});

test("an empty lock is a success even with nothing discovered", () => {
	assert.equal(
		linkAttemptOutcome({
			lockedCount: 0,
			attempted: 0,
			failed: 0,
			sawLockedSkills: false,
		}),
		"success",
	);
});
