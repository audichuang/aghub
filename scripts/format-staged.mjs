#!/usr/bin/env node
// nano-staged formatter task: format staged files, but SKIP symbolic links.
// Skill aliases under .claude/skills are symlinks into .agents/skills; passing
// both the alias and its target would format the same file twice through two
// paths. nano-staged appends the staged file paths as argv; we drop the
// symlinks and run `oxfmt` on the rest (the symlink targets are formatted on
// their own when staged).
import { lstatSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const files = process.argv.slice(2).filter((f) => {
	try {
		return !lstatSync(f).isSymbolicLink();
	} catch {
		return false;
	}
});

if (files.length === 0) {
	process.exit(0);
}

// Point at the repo-local oxfmt CLI explicitly (no PATH/.cmd dependency) and
// run it with the current node binary. The package does not export its bin
// path, so require.resolve cannot reach it.
const oxfmtCli = fileURLToPath(
	new URL("../node_modules/oxfmt/bin/oxfmt", import.meta.url),
);
const result = spawnSync(process.execPath, [oxfmtCli, ...files], {
	stdio: "inherit",
});
process.exit(result.status ?? 1);
