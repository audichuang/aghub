import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
import { test } from "node:test";
import {
	PreferenceNotReadError,
	preferenceWriteBasis,
	writePreference,
} from "./preference-write.ts";

const FALLBACK = ["a", "b"];

// THE finding. A failed read leaves `data` undefined and the UI renders the
// fallback; the bug was letting the next write treat that fallback as the
// user's saved state. Reverting this to `cached ?? fallback` unconditionally
// makes this go green the wrong way.
test("a read that did not succeed has no basis, whatever is cached", () => {
	assert.equal(preferenceWriteBasis(false, undefined, FALLBACK), null);
	assert.equal(
		preferenceWriteBasis(false, ["stale"], FALLBACK),
		null,
		"stale cache from an earlier success is not a fresh read either",
	);
});

// A store that has never been written to is a real, complete answer — not a
// gap. Refusing here would make the settings panel unusable on first run.
test("a successful read of an empty store falls back, and that is a basis", () => {
	assert.deepEqual(preferenceWriteBasis(true, undefined, FALLBACK), FALLBACK);
	assert.deepEqual(preferenceWriteBasis(true, [], FALLBACK), []);
});

test("a successful read returns what was read", () => {
	assert.deepEqual(preferenceWriteBasis(true, ["x"], FALLBACK), ["x"]);
});

// The toast has to tell these apart: a failed READ is retried by reloading,
// a failed SAVE by pressing the control again.
test("the refusal names the preference and is its own error type", () => {
	const error = new PreferenceNotReadError("sidebar");
	assert.ok(error instanceof PreferenceNotReadError);
	assert.equal(error.preference, "sidebar");
	assert.match(error.message, /sidebar/);
});

test("a successful write leaves the cache on the new value", async () => {
	const cache: string[][] = [];
	const saved: string[][] = [];
	await writePreference({
		previous: ["old"],
		next: ["new"],
		setCache: (v) => cache.push(v),
		save: async (v) => {
			saved.push(v);
		},
	});
	assert.deepEqual(cache, [["new"]]);
	assert.deepEqual(saved, [["new"]]);
});

// The half that is easy to get wrong: restoring the query cache fixes the
// pixels, but the Tauri store's `set()` has already put the rejected value in
// its in-memory map, where the next successful save from anywhere else flushes
// it to disk. The rollback must go back through `save`.
test("a failed save restores the cache AND rewrites the previous value", async () => {
	const cache: string[][] = [];
	const attempted: string[][] = [];
	await assert.rejects(
		writePreference({
			previous: ["old"],
			next: ["new"],
			setCache: (v) => cache.push(v),
			save: async (v) => {
				attempted.push(v);
				if (v[0] === "new") throw new Error("disk full");
			},
		}),
		/disk full/,
		"the caller hears about what it tried to do, not about the rollback",
	);
	assert.deepEqual(cache, [["new"], ["old"]]);
	assert.deepEqual(
		attempted,
		[["new"], ["old"]],
		"the previous value goes back through save, not just into the cache",
	);
});

test("a rollback that also fails still reports the original error", async () => {
	await assert.rejects(
		writePreference({
			previous: ["old"],
			next: ["new"],
			setCache: () => {},
			save: async () => {
				throw new Error("disk full");
			},
		}),
		/disk full/,
	);
});
