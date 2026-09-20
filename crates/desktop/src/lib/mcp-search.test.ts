import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { createMcpSearch, mcpGroupMatchesSearch } from "./mcp-search.ts";

const GROUPS = [
	{
		mergeKey: "stdio:context7",
		items: [{ name: "context7", source: "npx", agent: "claude" }],
	},
	{
		mergeKey: "http:linear",
		items: [{ name: "linear", source: "remote", agent: "cursor" }],
	},
	{
		mergeKey: "stdio:playwright",
		items: [{ name: "playwright", source: "npx", agent: "codex" }],
	},
];

// THE bug found by extracting this seam: the list searched `items.0.name`,
// and Fuse walks an array rather than indexing it, so the literal "0" was
// looked up as a property of each ELEMENT and was always undefined. Every
// query returned nothing — the search box emptied the list on the first
// keystroke and never found a server. Revert `items.name` to `items.0.name`
// and all three assertions below go red.
test("a query matches on name, source and agent", () => {
	const fuse = createMcpSearch(GROUPS);
	assert.equal(fuse.search("context")[0]?.item.mergeKey, "stdio:context7");
	assert.equal(fuse.search("linear")[0]?.item.mergeKey, "http:linear");
	assert.equal(fuse.search("cursor")[0]?.item.mergeKey, "http:linear");
});

// Walking every element is the right answer, not a workaround: a merged
// group's members differ by AGENT, so a query naming a non-first member's
// agent must still find the group.
test("a query finds an agent that is not the group's first member", () => {
	const fuse = createMcpSearch([
		{
			mergeKey: "stdio:shared",
			items: [
				{ name: "shared", source: "npx", agent: "claude" },
				{ name: "shared", source: "npx", agent: "windsurf" },
			],
		},
	]);
	assert.equal(fuse.search("windsurf")[0]?.item.mergeKey, "stdio:shared");
});

// THE finding this seam exists for: the list said "no matching servers" while
// the right panel kept edit / delete / duplicate aimed at a server outside the
// results, with nothing on screen saying so. The banner's condition and the
// list's filter must therefore be the SAME index — pinned over a spread of
// queries including one that matches nothing.
test("the list filter and the open-server check agree on every query", () => {
	const fuse = createMcpSearch(GROUPS);
	for (const query of ["context", "linear", "npx", "QA-NO-SERVER", "play"]) {
		const listed = fuse.search(query).map((r) => r.item.mergeKey);
		for (const group of GROUPS) {
			assert.equal(
				mcpGroupMatchesSearch(fuse, query, group.mergeKey),
				listed.includes(group.mergeKey),
				`"${query}" disagreed about ${group.mergeKey}`,
			);
		}
	}
});

// The QA fixture verbatim: a selected server, a query nothing matches.
test("a query with no results puts the open server outside them", () => {
	const fuse = createMcpSearch(GROUPS);
	assert.equal(fuse.search('QA-NO-SERVER-<>&"測試').length, 0);
	assert.equal(
		mcpGroupMatchesSearch(fuse, 'QA-NO-SERVER-<>&"測試', "stdio:context7"),
		false,
		"the banner must show — the list is showing nothing",
	);
});

// An untouched search box is the page's default state; a banner that shows
// there would be permanent furniture rather than an explanation.
test("an empty or whitespace query leaves nothing outside the results", () => {
	const fuse = createMcpSearch(GROUPS);
	assert.equal(mcpGroupMatchesSearch(fuse, "", "stdio:context7"), true);
	assert.equal(mcpGroupMatchesSearch(fuse, "   ", "stdio:context7"), true);
	assert.equal(
		mcpGroupMatchesSearch(fuse, "linear", null),
		true,
		"nothing is open, so nothing is outside anything",
	);
});
