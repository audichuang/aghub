import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
import { test } from "node:test";

/**
 * Adding an agent has one step nothing checks: `crates/desktop/src/assets/agent/
 * <id>.svg`. `agent-icons.tsx` resolves it through `import.meta.glob`, so a
 * missing file is not a build, lint, typecheck or test error — it silently
 * falls through to a first-letter avatar. `jetbrains-ai` shipped in that state
 * for several releases (its asset is spelled `jetbrains_ai.svg`, which is why
 * the lookup grew a `-` → `_` fallback).
 *
 * The roster is the one source of agent ids (`agent_roster!` in
 * `crates/agents/src/agents/mod.rs`), so read it rather than restate it — a
 * list copied here would rot on the next roster edit, which is the failure
 * this test exists to prevent.
 */
const ROSTER = new URL("../../../agents/src/agents/mod.rs", import.meta.url)
	.pathname;
const ASSETS = new URL("../assets/agent/", import.meta.url).pathname;

/** `Variant => "id", module, ["alias", …];` — the id literal is what we need. */
function rosterIds(): string[] {
	const src = readFileSync(ROSTER, "utf8");
	return [...src.matchAll(/^\s*\w+\s*=>\s*"([a-z0-9_-]+)"/gm)].map(
		(m) => m[1],
	);
}

/** The same two spellings `AgentIcon` tries, in the same order. */
function hasAsset(id: string): boolean {
	return (
		existsSync(`${ASSETS}${id}.svg`) ||
		existsSync(`${ASSETS}${id.replaceAll("-", "_")}.svg`)
	);
}

test("the roster parses to a plausible number of agents", () => {
	// Without this the test below is vacuous: a regex that stopped matching
	// yields an empty list and "every agent has an icon" passes trivially.
	assert.ok(
		rosterIds().length >= 20,
		`parsed ${rosterIds().length} agent ids out of agent_roster!, expected ` +
			"at least 20 — the roster macro's row shape probably changed, so " +
			"this guard is checking nothing. Fix the regex.",
	);
});

test("every roster agent has an icon asset", () => {
	const missing = rosterIds().filter((id) => !hasAsset(id));
	assert.deepEqual(
		missing,
		[],
		`these agents have no SVG under src/assets/agent/, so AgentIcon renders ` +
			`a first-letter avatar with nothing failing: ${missing.join(", ")}. ` +
			"Add <id>.svg (a dash may also be spelled with an underscore).",
	);
});
