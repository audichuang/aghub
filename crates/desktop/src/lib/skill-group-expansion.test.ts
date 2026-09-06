import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	isGroupExpanded,
	toggleGroupExpansion,
} from "./skill-group-expansion.ts";

test("a group nobody has touched is open", () => {
	// The regression this guards: an empty override map is exactly the state
	// on the very first render, when no group list exists yet. Defaulting to
	// closed there renders an empty list for every source on the page.
	assert.equal(isGroupExpanded(new Map(), "owner/repo"), true);
	assert.equal(isGroupExpanded(new Map(), "__unrecorded__"), true);
});

test("toggling an untouched group closes it, and toggling again reopens it", () => {
	const closed = toggleGroupExpansion(new Map(), "owner/repo");
	assert.equal(isGroupExpanded(closed, "owner/repo"), false);

	const reopened = toggleGroupExpansion(closed, "owner/repo");
	assert.equal(isGroupExpanded(reopened, "owner/repo"), true);
});

test("toggling one group leaves every other group alone", () => {
	const closed = toggleGroupExpansion(new Map(), "a/one");
	assert.equal(isGroupExpanded(closed, "a/one"), false);
	assert.equal(isGroupExpanded(closed, "b/two"), true);
});

test("toggling does not mutate the map it was given", () => {
	const before = new Map<string, boolean>();
	toggleGroupExpansion(before, "owner/repo");
	assert.equal(before.size, 0, "React state must not be mutated in place");
});
