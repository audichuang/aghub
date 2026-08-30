import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	eligibleAgentIds,
	newToolPromptDelta,
	reconcileAddsForNewAgents,
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
