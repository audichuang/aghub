import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { createSkillSearch } from "./skill-search.ts";

const GROUPS = [
	{ name: "notebooklm", description: "podcast and deep research" },
	{ name: "releasing-aghub", description: "cut a desktop + CLI release" },
	{ name: "verify", description: "runtime-verify the skill chain" },
];

test("a query matches on name and on description", () => {
	const fuse = createSkillSearch(GROUPS);
	assert.equal(fuse.search("notebook")[0]?.item.name, "notebooklm");
	assert.equal(fuse.search("podcast")[0]?.item.name, "notebooklm");
});

// The banner in skills.tsx and the filter in skill-list.tsx used to build their
// own Fuse with hand-copied options. If those drift, a skill the list is still
// showing gets told it fell out of the results, or the reverse. Both now go
// through `createSkillSearch`, so one index answers both questions — pinned
// here over a spread of queries including one that matches nothing.
test("the list filter and the open-skill check agree on every query", () => {
	const fuse = createSkillSearch(GROUPS);
	for (const query of ["notebook", "release", "verify", "zzz", "deep"]) {
		const listed = fuse.search(query).map((r) => r.item.name);
		for (const group of GROUPS) {
			const stillInResults = fuse
				.search(query)
				.some((r) => r.item.name === group.name);
			assert.equal(
				stillInResults,
				listed.includes(group.name),
				`"${query}" disagreed about ${group.name}`,
			);
		}
	}
});
