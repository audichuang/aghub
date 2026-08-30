import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	allTags,
	applyTagOp,
	matchesTagFilter,
	UNTAGGED,
	unionTags,
} from "./skill-tags.ts";

test("adding the same tag twice is idempotent", () => {
	let tags = applyTagOp({}, ["pdf"], "add", "work");
	tags = applyTagOp(tags, ["pdf"], "add", "work");
	assert.deepEqual(tags, { pdf: ["work"] });
});

test("removing a tag the skill does not carry is a no-op", () => {
	const before = { pdf: ["work"] };
	assert.deepEqual(applyTagOp(before, ["pdf"], "remove", "home"), before);
});

test("removing the last tag drops the key, so the skill reads as untagged", () => {
	const tags = applyTagOp({ pdf: ["work"] }, ["pdf"], "remove", "work");
	assert.deepEqual(tags, {});
	assert.equal(matchesTagFilter(tags.pdf, new Set([UNTAGGED])), true);
});

test("a blank tag is rejected and changes nothing", () => {
	const before = { pdf: ["work"] };
	assert.deepEqual(applyTagOp(before, ["pdf"], "add", "   "), before);
	assert.deepEqual(applyTagOp(before, ["pdf"], "add", ""), before);
});

test("a tag is trimmed before it is stored", () => {
	assert.deepEqual(applyTagOp({}, ["pdf"], "add", "  work "), {
		pdf: ["work"],
	});
});

test("one op spans the whole selection without touching other skills", () => {
	const tags = applyTagOp(
		{ other: ["keep"] },
		["pdf", "docx"],
		"add",
		"office",
	);
	assert.deepEqual(tags, {
		other: ["keep"],
		pdf: ["office"],
		docx: ["office"],
	});
});

test("applyTagOp does not mutate its input", () => {
	const before = { pdf: ["work"] };
	applyTagOp(before, ["pdf"], "add", "home");
	assert.deepEqual(before, { pdf: ["work"] });
});

test("tag filtering is AND, not OR", () => {
	const both = ["work", "office"];
	assert.equal(matchesTagFilter(both, new Set(["work"])), true);
	assert.equal(matchesTagFilter(both, new Set(["work", "office"])), true);
	assert.equal(matchesTagFilter(both, new Set(["work", "home"])), false);
	assert.equal(matchesTagFilter(["work"], new Set()), true);
});

test("untagged never matches a tagged skill", () => {
	assert.equal(matchesTagFilter(["work"], new Set([UNTAGGED])), false);
	assert.equal(matchesTagFilter([], new Set([UNTAGGED])), true);
	assert.equal(matchesTagFilter(undefined, new Set([UNTAGGED])), true);
	// "untagged AND work" is unsatisfiable, and says so.
	assert.equal(matchesTagFilter([], new Set([UNTAGGED, "work"])), false);
});

test("allTags and unionTags are sorted and de-duplicated", () => {
	const tags = { pdf: ["b", "a"], docx: ["a"] };
	assert.deepEqual(allTags(tags), ["a", "b"]);
	assert.deepEqual(unionTags(tags, ["pdf", "docx"]), ["a", "b"]);
	assert.deepEqual(unionTags(tags, ["docx"]), ["a"]);
	assert.deepEqual(unionTags(tags, ["missing"]), []);
});
