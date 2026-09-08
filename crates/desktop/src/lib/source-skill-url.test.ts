import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { sourceSkillUrl } from "./source-skill-url.ts";

test("opens a nested skill document on the selected ref", () => {
	assert.equal(
		sourceSkillUrl(
			"https://github.com/mattpocock/skills.git",
			"skills/in-progress/retro/SKILL.md",
			"main",
		),
		"https://github.com/mattpocock/skills/blob/main/skills/in-progress/retro/SKILL.md",
	);
	assert.equal(
		sourceSkillUrl(
			"https://github.com/o/r/",
			"a b/SKILL.md",
			"feature/docs",
		),
		"https://github.com/o/r/blob/feature%2Fdocs/a%20b/SKILL.md",
	);
	assert.equal(
		sourceSkillUrl("https://github.com/o/r", "SKILL.md"),
		"https://github.com/o/r/blob/HEAD/SKILL.md",
	);
});

test("does not open unsafe or unsupported sources and paths", () => {
	for (const source of [
		undefined,
		"javascript:alert(1)",
		"https://token@github.com/o/r",
		"https://github.com.evil/o/r",
		"https://gitlab.com/o/r",
	])
		assert.equal(sourceSkillUrl(source, "SKILL.md"), null);
	for (const path of ["../SKILL.md", "/SKILL.md", "a\\b/SKILL.md"])
		assert.equal(sourceSkillUrl("https://github.com/o/r", path), null);
});
