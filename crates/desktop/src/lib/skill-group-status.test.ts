import assert from "node:assert/strict";
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { SkillUpdateResponse } from "../generated/dto";
import {
	sharedUncheckableReason,
	uncheckableTooltipKey,
} from "./skill-group-status.ts";

function statusMap(
	entries: [string, SkillUpdateResponse][],
): Map<string, SkillUpdateResponse> {
	return new Map(entries);
}

test("empty group has no shared reason", () => {
	assert.deepEqual(sharedUncheckableReason([], new Map()), {
		kind: "none",
	});
});

test("every skill uncheckable for the same non-auth reason rolls up", () => {
	const statuses = statusMap([
		[
			"a",
			{
				name: "a",
				scope: "global",
				status: "uncheckable",
				reason: "network",
			},
		],
		[
			"b",
			{
				name: "b",
				scope: "global",
				status: "uncheckable",
				reason: "network",
			},
		],
	]);
	assert.deepEqual(sharedUncheckableReason(["a", "b"], statuses), {
		kind: "other",
		reason: "network",
	});
});

test("every skill uncheckable for auth rolls up to the auth kind", () => {
	const statuses = statusMap([
		[
			"a",
			{
				name: "a",
				scope: "global",
				status: "uncheckable",
				reason: "auth",
			},
		],
		[
			"b",
			{
				name: "b",
				scope: "global",
				status: "uncheckable",
				reason: "auth",
			},
		],
	]);
	assert.deepEqual(sharedUncheckableReason(["a", "b"], statuses), {
		kind: "auth",
	});
});

test("mixed reasons do not roll up", () => {
	const statuses = statusMap([
		[
			"a",
			{
				name: "a",
				scope: "global",
				status: "uncheckable",
				reason: "auth",
			},
		],
		[
			"b",
			{
				name: "b",
				scope: "global",
				status: "uncheckable",
				reason: "network",
			},
		],
	]);
	assert.deepEqual(sharedUncheckableReason(["a", "b"], statuses), {
		kind: "none",
	});
});

test("a checkable skill in the group blocks the rollup", () => {
	const statuses = statusMap([
		[
			"a",
			{
				name: "a",
				scope: "global",
				status: "uncheckable",
				reason: "auth",
			},
		],
		["b", { name: "b", scope: "global", status: "upToDate" }],
	]);
	assert.deepEqual(sharedUncheckableReason(["a", "b"], statuses), {
		kind: "none",
	});
});

test("a skill with no status yet (check has not run) blocks the rollup", () => {
	const statuses = statusMap([
		[
			"a",
			{
				name: "a",
				scope: "global",
				status: "uncheckable",
				reason: "auth",
			},
		],
	]);
	assert.deepEqual(sharedUncheckableReason(["a", "b"], statuses), {
		kind: "none",
	});
});

test("uncheckableTooltipKey maps known reasons and falls back for unknown ones", () => {
	assert.equal(uncheckableTooltipKey("auth"), "skillUncheckableAuth");
	assert.equal(uncheckableTooltipKey("network"), "skillUncheckableNetwork");
	assert.equal(uncheckableTooltipKey("local"), "skillUncheckableLocal");
	assert.equal(uncheckableTooltipKey("ssh"), "skillUncheckableUnsupported");
	assert.equal(
		uncheckableTooltipKey("unsupportedScheme"),
		"skillUncheckableUnsupported",
	);
	assert.equal(uncheckableTooltipKey("noPath"), "skillUncheckableNoPath");
	assert.equal(uncheckableTooltipKey("timeout"), "skillUncheckableTimeout");
	assert.equal(
		uncheckableTooltipKey("something-else"),
		"skillUncheckableGeneric",
	);
});
