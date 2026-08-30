#!/usr/bin/env node
/**
 * Freshness check for the bundled skills-sh featured catalog.
 *
 * The catalog is hand-maintained data pointing at OTHER people's repos, so it
 * rots silently: a folder is renamed, a skill moves out, a `name:` changes. A
 * dead entry is not a cosmetic bug — clicking the card calls the install API,
 * which answers SKILLS_NOT_FOUND.
 *
 * The check that matters is against SKILL.md FRONTMATTER, not the folder name:
 * `select_repo_skills` matches on the frontmatter `name:` (see
 * `crates/skill/src/install.rs`), and the two do differ in the wild
 * (vercel-labs/agent-skills ships `react-best-practices/` whose name is
 * `vercel-react-best-practices`).
 *
 * Network + auth are required, so this is a `just` recipe, not a unit test.
 * Uses `gh` so tokens stay in the user's existing GitHub CLI login.
 */

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const CATALOG = join(
	dirname(fileURLToPath(import.meta.url)),
	"../crates/desktop/src/data/featured-skills.json",
);

function gh(path) {
	return JSON.parse(
		execFileSync("gh", ["api", path], {
			encoding: "utf8",
			maxBuffer: 64 * 1024 * 1024,
		}),
	);
}

/** The `name:` a repo's SKILL.md declares — what install actually matches. */
function frontmatterName(markdown) {
	const fence = markdown.match(/^---\s*\n([\s\S]*?)\n---\s*(\n|$)/);
	if (!fence) return null;
	const name = fence[1].match(/^name:\s*(.+?)\s*$/m);
	return name ? name[1].replace(/^["']|["']$/g, "").trim() : null;
}

function requireGh() {
	try {
		execFileSync("gh", ["auth", "status"], { stdio: "ignore" });
	} catch {
		console.error(
			"needs the GitHub CLI: install `gh` and run `gh auth login` " +
				"(unauthenticated requests run out of quota part-way through)",
		);
		process.exit(2);
	}
}

function main() {
	requireGh();
	const catalog = JSON.parse(readFileSync(CATALOG, "utf8"));
	const bySource = new Map();
	for (const skill of catalog.skills) {
		if (!bySource.has(skill.source)) bySource.set(skill.source, []);
		bySource.get(skill.source).push(skill.name);
	}

	let dead = 0;
	for (const [source, wanted] of bySource) {
		const [, owner, repo] = source.split("/");
		let tree;
		try {
			tree = gh(`repos/${owner}/${repo}/git/trees/HEAD?recursive=1`);
		} catch {
			console.error(`✗ ${source}: repository unreachable (renamed? private?)`);
			dead += wanted.length;
			continue;
		}
		const paths = tree.tree
			.map((entry) => entry.path)
			.filter((path) => path.endsWith("SKILL.md"));

		const real = new Set();
		for (const path of paths) {
			let blob;
			try {
				blob = gh(`repos/${owner}/${repo}/contents/${path}`);
			} catch {
				continue;
			}
			const body = Buffer.from(blob.content, "base64").toString("utf8");
			const name = frontmatterName(body);
			if (name) real.add(name);
		}

		const missing = wanted.filter((name) => !real.has(name));
		if (missing.length === 0) {
			console.log(`✓ ${source}: ${wanted.length} entries live`);
			continue;
		}
		dead += missing.length;
		console.error(`✗ ${source}: ${missing.join(", ")} not in the repo`);
		console.error(`  available: ${[...real].sort().join(", ")}`);
	}

	if (dead > 0) {
		console.error(
			`\n${dead} featured entr${dead === 1 ? "y" : "ies"} would fail to install.`,
		);
		process.exit(1);
	}
	console.log(`\nAll ${catalog.skills.length} featured entries install-able.`);
}

main();
