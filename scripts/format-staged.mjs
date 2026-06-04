#!/usr/bin/env node
// nano-staged prettier task: format staged files, but SKIP symbolic links.
// Prettier errors out ("is a symbolic link") when a symlink path is passed
// explicitly, and this repo's per-crate CLAUDE.md files are symlinks to
// AGENTS.md. nano-staged appends the staged file paths as argv; we drop the
// symlinks and run `prettier --write` on the rest (the symlink targets are
// formatted on their own when staged).
import { lstatSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";

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

// Resolve the repo-local prettier CLI explicitly (no PATH/.cmd dependency) and
// run it with the current node binary.
const require = createRequire(import.meta.url);
const prettierCli = require.resolve("prettier/bin/prettier.cjs");
const result = spawnSync(process.execPath, [prettierCli, "--write", ...files], {
	stdio: "inherit",
});
process.exit(result.status ?? 1);
