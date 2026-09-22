import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
import { test } from "node:test";
import {
	disabledAgentsKey,
	LOCAL_DISABLED_AGENTS_KEY,
	resolveDisabledAgents,
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
