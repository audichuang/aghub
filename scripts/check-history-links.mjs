#!/usr/bin/env node
// Fail when a `docs/history/<file>.md#<anchor>` reference in a tracked file
// points at a missing file or heading, or when one history file has two
// headings with the same slug. Run from the repo root.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";

// GitHub-style heading slug.
const slug = (h) =>
	h
		.trim()
		.toLowerCase()
		.replace(/[^\p{L}\p{N}\s_-]/gu, "")
		.replace(/\s/g, "-");

const anchors = new Map();
const errors = [];
function anchorsOf(file) {
	if (anchors.has(file)) return anchors.get(file);
	const seen = new Set();
	let fenced = false;
	for (const line of readFileSync(file, "utf8").split("\n")) {
		if (/^\s*(```|~~~)/.test(line)) fenced = !fenced;
		const m = !fenced && /^#{1,6}\s+(.*)$/.exec(line);
		if (!m) continue;
		const s = slug(m[1]);
		if (seen.has(s)) errors.push(`${file}: duplicate heading slug #${s}`);
		seen.add(s);
	}
	anchors.set(file, seen);
	return seen;
}

let out = "";
try {
	out = execFileSync(
		"git",
		[
			"grep",
			"--untracked", // new files are checked before they are committed
			"-nIoE",
			"docs/history/[A-Za-z0-9_.-]+\\.md(#[A-Za-z0-9_-]+)?",
			"--",
			".",
			":!docs/history/README.md",
		],
		{ encoding: "utf8" },
	);
} catch (e) {
	if (e.status !== 1) throw e; // 1 = no matches
}

for (const line of out.split("\n").filter(Boolean)) {
	const [loc, lineNo, ref] = line.split(/:(\d+):/);
	const [file, anchor] = ref.split("#");
	if (!existsSync(file)) errors.push(`${loc}:${lineNo}: missing file ${ref}`);
	else if (anchor && !anchorsOf(file).has(anchor))
		errors.push(`${loc}:${lineNo}: missing anchor ${ref}`);
}

// Duplicate slugs matter even in a file nothing links to yet.
for (const f of execFileSync(
	"git",
	["ls-files", "-co", "--exclude-standard", "docs/history/*.md"],
	{
		encoding: "utf8",
	},
)
	.split("\n")
	.filter(Boolean))
	if (existsSync(f)) anchorsOf(f);

for (const e of errors) console.error(e);
if (errors.length) process.exit(1);
