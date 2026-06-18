import assert from "node:assert/strict";
// No FE test runner is installed here; pure selection logic uses Node's runner,
// matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	allSkillPaths,
	selectedSkills,
	toggleSkillPath,
} from "./source-skill-selection.ts";

const skills = [
	{ name: "cad-viewer", skillPath: "plugins/cad/cad-viewer/SKILL.md" },
	{ name: "implicit-cad", skillPath: "plugins/cad/implicit-cad/SKILL.md" },
	{ name: "sdf", skillPath: "plugins/cad/sdf/SKILL.md" },
];

test("allSkillPaths returns every visible skill path", () => {
	assert.deepEqual(allSkillPaths(skills), [
		"plugins/cad/cad-viewer/SKILL.md",
		"plugins/cad/implicit-cad/SKILL.md",
		"plugins/cad/sdf/SKILL.md",
	]);
});

test("toggleSkillPath adds and removes a path without mutating input", () => {
	const selected = new Set(["plugins/cad/cad-viewer/SKILL.md"]);
	const added = toggleSkillPath(selected, "plugins/cad/sdf/SKILL.md");
	const removed = toggleSkillPath(added, "plugins/cad/cad-viewer/SKILL.md");

	assert.deepEqual([...selected], ["plugins/cad/cad-viewer/SKILL.md"]);
	assert.deepEqual([...added].sort(), [
		"plugins/cad/cad-viewer/SKILL.md",
		"plugins/cad/sdf/SKILL.md",
	]);
	assert.deepEqual([...removed], ["plugins/cad/sdf/SKILL.md"]);
});

test("selectedSkills preserves visible list order", () => {
	const selected = new Set([
		"plugins/cad/sdf/SKILL.md",
		"plugins/cad/cad-viewer/SKILL.md",
	]);

	assert.deepEqual(
		selectedSkills(skills, selected).map((skill) => skill.name),
		["cad-viewer", "sdf"],
	);
});
