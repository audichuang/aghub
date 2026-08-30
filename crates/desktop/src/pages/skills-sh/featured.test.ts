import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	asMarketSkill,
	INSTALL_SOURCE_RE,
	isInstallShapedSource,
	parseFeaturedCatalog,
} from "./featured.ts";

const catalogPath = join(
	dirname(fileURLToPath(import.meta.url)),
	"../../data/featured-skills.json",
);

function loadShippedCatalog(): unknown {
	return JSON.parse(readFileSync(catalogPath, "utf8"));
}

test("shipped featured catalog sources are install-shaped github/owner/repo", () => {
	const skills = parseFeaturedCatalog(loadShippedCatalog());
	assert.ok(skills.length >= 20, `want ≥20, got ${skills.length}`);
	assert.ok(skills.length <= 40, `want ≤40, got ${skills.length}`);
	for (const skill of skills) {
		assert.match(skill.source, INSTALL_SOURCE_RE);
		assert.equal(skill.source.includes("/tree/"), false);
		assert.equal(skill.source.includes("http"), false);
		const market = asMarketSkill(skill);
		assert.equal(market.source, skill.source);
		assert.equal(market.name, skill.name);
		assert.equal(market.slug, skill.slug);
		assert.equal(typeof market.installs, "number");
	}
});

test("shipped featured catalog has no duplicate source|name (React key + double card)", () => {
	const skills = parseFeaturedCatalog(loadShippedCatalog());
	const keys = skills.map((skill) => `${skill.source}|${skill.name}`);
	assert.equal(new Set(keys).size, keys.length, `duplicate in ${keys}`);
});

test("isInstallShapedSource rejects tree URLs and incomplete sources", () => {
	assert.equal(isInstallShapedSource("github/anthropics/skills"), true);
	assert.equal(
		isInstallShapedSource(
			"https://github.com/anthropics/skills/tree/main/skills/pdf",
		),
		false,
	);
	assert.equal(
		isInstallShapedSource("github/anthropics/skills/tree/main/skills/pdf"),
		false,
	);
	assert.equal(isInstallShapedSource("github/anthropics"), false);
	assert.equal(isInstallShapedSource("anthropics/skills"), false);
});

test("parseFeaturedCatalog throws on a tree-URL source", () => {
	assert.throws(
		() =>
			parseFeaturedCatalog({
				skills: Array.from({ length: 20 }, (_, i) => ({
					name: `skill-${i}`,
					slug: `skill-${i}`,
					summary: "ok",
					source:
						i === 0
							? "github/org/repo/tree/main/skills/foo"
							: "github/org/repo",
				})),
			}),
		/non-install source/,
	);
});
