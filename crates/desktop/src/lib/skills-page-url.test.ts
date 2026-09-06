import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	clearSelectionParams,
	parseScopeParam,
	resolveSourceRow,
	selectSkillParams,
	selectSourceParams,
	serializeScopeParam,
} from "./skills-page-url.ts";

test("parseScopeParam falls back to global for missing/unparseable input", () => {
	assert.deepEqual(parseScopeParam(null, new Set()), { scope: "global" });
	assert.deepEqual(parseScopeParam("", new Set()), { scope: "global" });
	assert.deepEqual(parseScopeParam("bogus", new Set()), {
		scope: "global",
	});
	// "project:" with nothing after it is not a real path.
	assert.deepEqual(parseScopeParam("project:", new Set()), {
		scope: "global",
	});
});

test("parseScopeParam resolves a registered project path", () => {
	const known = new Set(["/home/x/repo"]);
	assert.deepEqual(parseScopeParam("project:/home/x/repo", known), {
		scope: "project",
		projectPath: "/home/x/repo",
	});
});

test("parseScopeParam keeps project scope for an unregistered path (null projectPath, not a fallback to global)", () => {
	// This is the "select a project" empty state, not the global data root —
	// silently falling back to global would read the WRONG scope's data for
	// a stale or foreign URL.
	assert.deepEqual(parseScopeParam("project:/unknown", new Set()), {
		scope: "project",
		projectPath: null,
	});
});

test("serializeScopeParam round-trips with parseScopeParam", () => {
	const known = new Set(["/home/x/repo"]);
	assert.equal(serializeScopeParam("global", null), "global");
	assert.equal(
		serializeScopeParam("project", "/home/x/repo"),
		"project:/home/x/repo",
	);
	assert.deepEqual(
		parseScopeParam(serializeScopeParam("project", "/home/x/repo"), known),
		{ scope: "project", projectPath: "/home/x/repo" },
	);
});

test("serializeScopeParam falls back to global when a project scope has no path", () => {
	assert.equal(serializeScopeParam("project", null), "global");
});

test("selecting a skill clears source, and vice versa", () => {
	assert.deepEqual(selectSkillParams("my-skill"), {
		skill: "my-skill",
		source: null,
	});
	assert.deepEqual(selectSourceParams("owner/repo"), {
		skill: null,
		source: "owner/repo",
	});
	assert.deepEqual(clearSelectionParams(), { skill: null, source: null });
});

const rows = [
	{ source: "owner/repo", sourceUrl: "https://github.com/owner/repo.git" },
	{ source: "owner/other", sourceUrl: "https://github.com/owner/other.git" },
	// Local sources record no clone URL — sourceUrl is the empty-string
	// sentinel, and `source` (the local path) is what a caller must match.
	{ source: "/home/x/local-skills", sourceUrl: "" },
];

test("resolveSourceRow matches by sourceUrl (a clone URL from a deep link)", () => {
	assert.equal(
		resolveSourceRow(rows, "https://github.com/owner/repo.git"),
		rows[0],
	);
});

test("resolveSourceRow matches by the lock's bare source id (what group headers carry)", () => {
	assert.equal(resolveSourceRow(rows, "owner/other"), rows[1]);
});

test("resolveSourceRow matches a local source by its path even with an empty sourceUrl", () => {
	assert.equal(resolveSourceRow(rows, "/home/x/local-skills"), rows[2]);
});

test("resolveSourceRow returns null for no value or no match", () => {
	assert.equal(resolveSourceRow(rows, null), null);
	assert.equal(resolveSourceRow(rows, "nope"), null);
	// An empty string must never accidentally match a local row's empty
	// sourceUrl — "no source selected" is not "the local source".
	assert.equal(resolveSourceRow(rows, ""), null);
});

test("an ambiguous bare source id resolves to nothing, never a guess", () => {
	// One `owner/repo` served by two forges is two rows sharing the
	// host-blind id (see crates/skill-update/src/sources.rs). Answering with
	// either one opens a panel that updates and deletes skills against the
	// WRONG repository, so this must refuse instead of picking.
	const rows = [
		{ source: "owner/repo", sourceUrl: "https://github.com/owner/repo" },
		{ source: "owner/repo", sourceUrl: "https://gitlab.com/owner/repo" },
	];
	assert.equal(resolveSourceRow(rows, "owner/repo"), null);
	// The clone URL is still unique, so it keeps resolving both of them.
	assert.equal(
		resolveSourceRow(rows, "https://gitlab.com/owner/repo")?.sourceUrl,
		"https://gitlab.com/owner/repo",
	);
	assert.equal(
		resolveSourceRow(rows, "https://github.com/owner/repo")?.sourceUrl,
		"https://github.com/owner/repo",
	);
});

test("a bare id that only one row carries still resolves", () => {
	const rows = [
		{ source: "owner/repo", sourceUrl: "https://github.com/owner/repo" },
		{ source: "other/repo", sourceUrl: "https://github.com/other/repo" },
	];
	assert.equal(
		resolveSourceRow(rows, "other/repo")?.sourceUrl,
		"https://github.com/other/repo",
	);
});
