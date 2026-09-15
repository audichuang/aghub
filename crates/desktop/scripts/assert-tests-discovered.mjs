// The frontend gate cannot detect its own disappearance.
//
// `node --test <glob>` exits 0 when the glob matches NOTHING — "pass 0, fail 0"
// is a success. So a rename, a moved directory, or a narrowed glob turns the
// whole frontend gate (preflight AND ci.yml's lint-frontend) into a no-op that
// still reports green, and the source-scan guards under src/lib — the only
// thing that would have caught the 3.2.x HeroUI bump that shipped every
// Checkbox and Switch broken — stop being consulted with nothing to say so.
//
// This runs BEFORE the test runner in the `test` script and fails loudly
// instead. It counts what the runner's own globs will reach, so the two cannot
// drift apart silently.

import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import process from "node:process";

// Deliberately well under the real count (32) so ordinary deletions don't trip
// it; this is a floor against the gate VANISHING, not a coverage target.
const FLOOR = 25;
const SRC = new URL("../src/", import.meta.url).pathname;

/** Everything `src/**\/*.test.ts` and `src/**\/*.test.tsx` reach. */
function testFiles(dir) {
	const out = [];
	for (const entry of readdirSync(dir)) {
		const full = join(dir, entry);
		if (statSync(full).isDirectory()) out.push(...testFiles(full));
		else if (entry.endsWith(".test.ts") || entry.endsWith(".test.tsx"))
			out.push(full);
	}
	return out;
}

const found = testFiles(SRC);
if (found.length < FLOOR) {
	console.error(
		`Frontend test discovery found ${found.length} file(s) under src/, ` +
			`expected at least ${FLOOR}.\n` +
			"`node --test` exits 0 on an empty glob, so this would otherwise " +
			"be a silent green. Either the test files moved (fix the globs in " +
			"package.json's `test` script) or they were deleted (say so " +
			"deliberately and lower the FLOOR in this file).",
	);
	process.exit(1);
}
