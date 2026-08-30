import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { backgroundUpdateNews } from "./use-last-skill-check.ts";

const at = (iso: string) => ({ finishedAt: iso, updateAvailable: 3 });

test("no sidecar, or no updates, shows nothing", () => {
	assert.equal(backgroundUpdateNews(null, null), null);
	assert.equal(backgroundUpdateNews(undefined, null), null);
	assert.equal(
		backgroundUpdateNews({ finishedAt: "2026-08-31T09:00:00Z" }, null),
		null,
	);
	assert.equal(
		backgroundUpdateNews(
			{ finishedAt: "2026-08-31T09:00:00Z", updateAvailable: 0 },
			null,
		),
		null,
	);
});

test("a background run the app has not caught up with is news", () => {
	assert.equal(backgroundUpdateNews(at("2026-08-31T09:00:00Z"), null), 3);
	assert.equal(
		backgroundUpdateNews(
			at("2026-08-31T09:00:00Z"),
			new Date("2026-08-31T08:00:00Z"),
		),
		3,
	);
});

test("a sidecar older than the app's own check is stale, not news", () => {
	assert.equal(
		backgroundUpdateNews(
			at("2026-08-31T09:00:00Z"),
			new Date("2026-08-31T10:00:00Z"),
		),
		null,
	);
	// Same instant counts as already seen.
	assert.equal(
		backgroundUpdateNews(
			at("2026-08-31T09:00:00Z"),
			new Date("2026-08-31T09:00:00Z"),
		),
		null,
	);
});

test("an unparseable timestamp is never news", () => {
	assert.equal(
		backgroundUpdateNews({ finishedAt: "nope", updateAvailable: 5 }, null),
		null,
	);
	assert.equal(backgroundUpdateNews({ updateAvailable: 5 }, null), null);
});
