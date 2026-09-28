import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
import { test } from "node:test";
import {
	disabledAgentsKey,
	LOCAL_DISABLED_AGENTS_KEY,
	loadDisabledAgents,
	resolveDisabledAgents,
	resolveLegacyDisabledAgents,
} from "./disabled-agents.ts";

test("Local keeps the bare key so existing data needs no migration", () => {
	assert.equal(disabledAgentsKey("local"), LOCAL_DISABLED_AGENTS_KEY);
});

test("every remote gets its own key", () => {
	assert.equal(disabledAgentsKey("vm-1"), "disabledAgents:vm-1");
	assert.notEqual(disabledAgentsKey("vm-1"), disabledAgentsKey("vm-2"));
});

// THE regression this file exists for. A remote where the user re-enabled
// every inherited agent stores `[]`. `?? []` or `own.length > 0` both treat
// that as "never configured", fall back to Local, and silently disable the
// agents the user just turned on.
test("an empty own selection is configured, not unset", () => {
	assert.deepEqual(resolveDisabledAgents([], ["claude", "codex"]), []);
});

test("an unset remote inherits Local's selection", () => {
	assert.deepEqual(resolveDisabledAgents(undefined, ["claude"]), ["claude"]);
	assert.deepEqual(resolveDisabledAgents(null, ["claude"]), ["claude"]);
});

test("a configured remote keeps its own selection", () => {
	assert.deepEqual(resolveDisabledAgents(["codex"], ["claude"]), ["codex"]);
});

// Local passes its own value as the fallback, so both arguments are the same
// read: unset means `[]`, and `[]` stays `[]` rather than looping back.
test("Local resolves from its own value alone", () => {
	assert.deepEqual(resolveDisabledAgents(undefined, undefined), []);
	assert.deepEqual(resolveDisabledAgents([], []), []);
	assert.deepEqual(resolveDisabledAgents(["amp"], ["amp"]), ["amp"]);
});

function source(
	stored: { agents: string[]; configured: boolean } | null,
	legacy: string[] | null,
) {
	const writes: string[][] = [];
	return {
		writes,
		read: async () => stored,
		write: async (agents: string[]) => {
			writes.push(agents);
			return { agents };
		},
		legacy: async () => legacy,
		knownIds: new Set(["claude", "copilot", "gemini"]),
	};
}

test("a configured server wins over any legacy selection", async () => {
	const s = source({ agents: ["gemini"], configured: true }, ["copilot"]);
	assert.deepEqual(await loadDisabledAgents(s), ["gemini"]);
	assert.deepEqual(s.writes, []);
});

test("an unconfigured server is seeded once from legacy, stale ids dropped", async () => {
	const s = source({ agents: [], configured: false }, ["copilot", "gone"]);
	assert.deepEqual(await loadDisabledAgents(s), ["copilot"]);
	assert.deepEqual(s.writes, [["copilot"]]);
});

test("nothing to seed leaves the server untouched", async () => {
	const s = source({ agents: [], configured: false }, null);
	assert.deepEqual(await loadDisabledAgents(s), []);
	assert.deepEqual(s.writes, []);
});

test("an older server without the endpoint falls back to legacy", async () => {
	const s = source(null, ["copilot"]);
	assert.deepEqual(await loadDisabledAgents(s), ["copilot"]);
	assert.deepEqual(s.writes, []);
});

test("legacy read distinguishes never-saved from an empty selection", () => {
	assert.equal(resolveLegacyDisabledAgents(undefined, undefined), null);
	assert.deepEqual(resolveLegacyDisabledAgents([], undefined), []);
	assert.deepEqual(resolveLegacyDisabledAgents(undefined, ["claude"]), [
		"claude",
	]);
});
