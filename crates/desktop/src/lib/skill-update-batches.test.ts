import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; use Node's built-in
// runner, same as the sibling tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { SkillUpdateResponse } from "../generated/dto";
import type { LockSourceEntry } from "./skill-update-batches.ts";
import {
	batchedSkillCount,
	groupUpdatesBySource,
} from "./skill-update-batches.ts";

function lockEntry(
	name: string,
	source: string,
	sourceUrl: string,
): LockSourceEntry {
	return { name, source, sourceUrl };
}

/** A project-lock row: no `sourceUrl` field at all. */
function projectLockEntry(name: string, source: string): LockSourceEntry {
	return { name, source };
}

function updatable(name: string): SkillUpdateResponse {
	return {
		name,
		scope: "global",
		status: "updateAvailable",
		current: "aaaaaaaa",
		available: "bbbbbbbb",
	};
}

test("one batch per source; renamed is excluded from every batch", () => {
	const statuses: SkillUpdateResponse[] = [
		updatable("alpha"),
		updatable("beta"),
		updatable("gamma"),
		updatable("delta"),
		updatable("epsilon"),
		{ name: "zeta", scope: "global", status: "renamed", newName: "zeta-2" },
		{ name: "eta", scope: "global", status: "upToDate" },
	];
	const lock = [
		lockEntry("alpha", "o/one", "https://github.com/o/one.git"),
		lockEntry("beta", "o/one", "https://github.com/o/one.git"),
		lockEntry("gamma", "o/two", "https://github.com/o/two.git"),
		lockEntry("delta", "o/three", "https://gitlab.com/o/three.git"),
		lockEntry("epsilon", "o/three", "https://gitlab.com/o/three.git"),
		lockEntry("zeta", "o/one", "https://github.com/o/one.git"),
		lockEntry("eta", "o/one", "https://github.com/o/one.git"),
	];

	const { batches, renamed, unresolved } = groupUpdatesBySource(
		statuses,
		lock,
		"global",
		null,
	);

	// One request per source — not one per skill, and not one for everything.
	assert.equal(batches.length, 3);
	assert.equal(batchedSkillCount(batches), 5);
	assert.deepEqual(unresolved, []);
	assert.deepEqual(renamed, ["zeta"]);

	const bySource = new Map(batches.map((b) => [b.source, b.names]));
	assert.deepEqual(bySource.get("https://github.com/o/one.git"), [
		"alpha",
		"beta",
	]);
	assert.deepEqual(bySource.get("https://github.com/o/two.git"), ["gamma"]);
	assert.deepEqual(bySource.get("https://gitlab.com/o/three.git"), [
		"delta",
		"epsilon",
	]);

	// The rename transaction is a different endpoint; it must not ride along.
	for (const batch of batches) {
		assert.ok(!batch.names.includes("zeta"));
		assert.ok(!batch.names.includes("eta"));
	}
});

test("two forges serving the same owner/repo stay two batches", () => {
	// `source` is host-blind, so grouping on it would merge two different
	// origins into one request against whichever URL happened to win.
	const statuses = [updatable("alpha"), updatable("beta")];
	const lock = [
		lockEntry("alpha", "o/repo", "https://github.com/o/repo.git"),
		lockEntry("beta", "o/repo", "https://gitlab.com/o/repo.git"),
	];

	const { batches } = groupUpdatesBySource(statuses, lock, "global", null);
	assert.equal(batches.length, 2);
});

test("an updatable skill with no lock entry is reported, not batched", () => {
	// The lock read paths fail OPEN, so this is reachable. Batching it would
	// send `source: undefined` and 400 the whole run.
	const { batches, unresolved } = groupUpdatesBySource(
		[updatable("orphan")],
		[],
		"global",
		null,
	);
	assert.deepEqual(batches, []);
	assert.deepEqual(unresolved, ["orphan"]);
});

test("project scope carries its root into every batch", () => {
	const { batches } = groupUpdatesBySource(
		[updatable("alpha")],
		[lockEntry("alpha", "o/one", "https://github.com/o/one.git")],
		"project",
		"/repo",
	);
	assert.equal(batches[0].scope, "project");
	assert.equal(batches[0].projectRoot, "/repo");
});

test("an entry with no recorded URL falls back to the host-blind source", () => {
	const local = lockEntry("alpha", "/local/path", "");
	const { batches } = groupUpdatesBySource(
		[updatable("alpha")],
		[local],
		"global",
		null,
	);
	assert.equal(batches[0].source, "/local/path");
});

test("a project lock row, which records no URL, still groups", () => {
	const { batches, unresolved } = groupUpdatesBySource(
		[updatable("alpha"), updatable("beta")],
		[projectLockEntry("alpha", "o/one"), projectLockEntry("beta", "o/one")],
		"project",
		"/repo",
	);
	assert.deepEqual(unresolved, []);
	assert.equal(batches.length, 1);
	assert.equal(batches[0].source, "o/one");
});
