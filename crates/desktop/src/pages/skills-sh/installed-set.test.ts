import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	buildInstalledSet,
	installedKey,
	isSkillInstalled,
} from "./installed-set.ts";

test("same name with a different source is not installed", () => {
	const set = buildInstalledSet([
		{ source: "github/anthropics/skills", name: "pdf" },
	]);
	assert.equal(
		isSkillInstalled(set, "github/anthropics/skills", "pdf"),
		true,
	);
	assert.equal(isSkillInstalled(set, "github/other/skills", "pdf"), false);
	assert.equal(
		isSkillInstalled(set, "github/anthropics/skills", "pptx"),
		false,
	);
});

test("installedKey is source then name, matching MarketSkill fields", () => {
	assert.equal(
		installedKey("github/obra/superpowers", "tdd"),
		"obra/superpowers|tdd",
	);
});

test("a lock entry (owner/repo) matches the market spelling (github/owner/repo)", () => {
	// What the lock actually stores after install: resolve_remote_source turns
	// `github/obra/superpowers` into `obra/superpowers`. Without the prefix
	// strip, no installed market skill ever shows its chip.
	const set = buildInstalledSet([
		{ source: "obra/superpowers", name: "systematic-debugging" },
	]);
	assert.equal(
		isSkillInstalled(
			set,
			"github/obra/superpowers",
			"systematic-debugging",
		),
		true,
	);
	assert.equal(
		isSkillInstalled(set, "github/obra/other", "systematic-debugging"),
		false,
	);
});
