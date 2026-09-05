import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; use Node's built-in
// runner, same as the sibling tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	DEFAULT_CONTEXT_WINDOW,
	estimateSkillContextCost,
	estimateTokens,
	listingBudgetChars,
	MAX_DESCRIPTION_CHARS,
	skillEntryChars,
} from "./skill-context-cost.ts";

test("one entry is '- name: description'", () => {
	// "- " + "pdf" + ": " + "Read PDFs" = 2 + 3 + 2 + 9
	assert.equal(skillEntryChars("pdf", "Read PDFs"), 16);
});

test("a description longer than the cap is counted at the cap", () => {
	const long = "x".repeat(MAX_DESCRIPTION_CHARS + 500);
	assert.equal(skillEntryChars("pdf", long), 3 + 4 + MAX_DESCRIPTION_CHARS);
});

test("budget is 8000 chars at a 200k window and scales with it", () => {
	assert.equal(listingBudgetChars(DEFAULT_CONTEXT_WINDOW), 8000);
	assert.equal(listingBudgetChars(1_000_000), 40_000);
});

test("Chinese text costs far more tokens per character than English", () => {
	// 20 Han characters are ~20 tokens; 20 English characters are ~4. A single
	// chars/N constant would report these as equal, understating the Chinese
	// listing by about 4x — and Chinese descriptions are normal here.
	const han = estimateTokens("這是一個用來測試的中文技能描述文字內容範例");
	const latin = estimateTokens("this is twenty-plus ch");
	assert.ok(
		han > latin * 3,
		`expected Han (${han}) to cost far more than Latin (${latin})`,
	);
	assert.ok(han >= 20, `21 Han chars should be ~21 tokens, got ${han}`);
});

test("total joins entries with newlines and estimates tokens", () => {
	const cost = estimateSkillContextCost([
		{ name: "a", description: "one" }, // 1 + 4 + 3 = 8
		{ name: "b", description: "two" }, // 8
	]);
	assert.equal(cost.skillCount, 2);
	assert.equal(cost.totalChars, 8 + 8 + 1); // + one newline
	assert.ok(cost.totalTokens > 0 && cost.totalTokens < 17);
	assert.equal(cost.overBudgetChars, 0);
	assert.equal(cost.minDemotedSkills, 0);
});

test("the same skill reachable twice is one line, not two", () => {
	// A skill in a shared referrer dir comes back once per agent that reads
	// it; that is still ONE line in that agent's listing.
	const cost = estimateSkillContextCost([
		{ name: "a", description: "one" },
		{ name: "a", description: "one" },
	]);
	assert.equal(cost.skillCount, 1);
	assert.equal(cost.totalChars, 8);
});

test("over budget reports how many descriptions must be dropped, at minimum", () => {
	// 20 skills x ~1000 chars each blows a 8000-char budget.
	const skills = Array.from({ length: 20 }, (_, i) => ({
		name: `skill-${i}`,
		description: "d".repeat(1000),
	}));
	const cost = estimateSkillContextCost(skills);
	assert.ok(cost.overBudgetChars > 0, "should be over budget");
	assert.ok(cost.minDemotedSkills > 0, "should demote at least one skill");
	// Dropping every description must be enough to fit, so the count can never
	// exceed the number of skills.
	assert.ok(cost.minDemotedSkills <= skills.length);

	// Freeing exactly `minDemotedSkills` largest descriptions must clear the
	// overage — otherwise the number understates the damage.
	const freed = cost.minDemotedSkills * (1000 + 2);
	assert.ok(freed >= cost.overBudgetChars);
});

test("an empty set costs nothing", () => {
	const cost = estimateSkillContextCost([]);
	assert.equal(cost.totalChars, 0);
	assert.equal(cost.totalTokens, 0);
	assert.equal(cost.minDemotedSkills, 0);
});
