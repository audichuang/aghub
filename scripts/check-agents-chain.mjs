#!/usr/bin/env node
// Fail when an AGENTS.md load chain (root down to one directory) exceeds Codex's
// default 32 KiB `project_doc_max_bytes`. Codex truncates silently, losing the
// tail of the deepest file. Move a flow into a skill rather than trim wording.
// Run from the repo root.
import { execFileSync } from "node:child_process";
import { statSync } from "node:fs";
import { dirname } from "node:path";

const LIMIT = 32 * 1024;
const files = execFileSync("git", ["ls-files", "*AGENTS.md"], {
	encoding: "utf8",
})
	.split("\n")
	.filter((f) => f === "AGENTS.md" || f.endsWith("/AGENTS.md"));
const known = new Set(files);

let largest = 0;
let failed = false;
for (const file of files) {
	let total = 0;
	for (let dir = dirname(file); ; dir = dirname(dir)) {
		const candidate = dir === "." ? "AGENTS.md" : `${dir}/AGENTS.md`;
		if (known.has(candidate)) total += statSync(candidate).size;
		if (dir === ".") break;
	}
	largest = Math.max(largest, total);
	if (total > LIMIT) {
		console.error(`${file}: load chain is ${total} bytes, limit ${LIMIT}`);
		failed = true;
	}
}
if (failed) process.exit(1);
console.log(`AGENTS.md load chains ok, largest ${largest}/${LIMIT} bytes`);
