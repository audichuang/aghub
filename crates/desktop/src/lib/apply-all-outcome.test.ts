import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
import { test } from "node:test";
import type { ApplySkillUpdateResponse } from "../generated/dto";
import type { ApplyAllOutcome } from "../hooks/use-apply-all-skill-updates";
import { applyAllToast } from "./apply-all-outcome.ts";

const t = (key: string, options?: { count?: number }) =>
	`${key}(${options?.count ?? ""})`;

function row(
	name: string,
	error: string | null = null,
): ApplySkillUpdateResponse {
	return {
		success: error === null,
		name,
		scope: "global",
		updatedHash: error === null ? "h" : null,
		paths: [],
		error,
	};
}

function outcome(
	results: ApplySkillUpdateResponse[],
	extra: Partial<ApplyAllOutcome> = {},
): ApplyAllOutcome {
	const failures = results.filter((r) => !r.success);
	return {
		results,
		failures,
		updated: results.length - failures.length,
		unconfirmed: false,
		definiteFailureCount: 0,
		...extra,
	};
}

// The incident: a failed row was shown for 4s without naming the skill, and
// the status strip kept saying "1 可更新" — nobody saw why.
test("failed rows name every skill and stay on screen", () => {
	const toast = applyAllToast(
		outcome([
			row("hindsight-coding-agent"),
			row("last30days", "Failed to fetch source repository"),
			row("orca-cli", "Skill source changed; refresh Sources and retry"),
		]),
		t,
	);

	assert.equal(toast.kind, "danger");
	assert.equal(toast.title, "sourceUpdateSomeFailedMany(2)");
	assert.equal(
		toast.description,
		"last30days: Failed to fetch source repository\n" +
			"orca-cli: Skill source changed; refresh Sources and retry",
	);
	assert.equal(toast.persistent, true);
});

// A later batch's transport error used to replace the report, so rows an
// earlier batch had already answered as failed were never mentioned.
test("an unconfirmed run still reports the failures it did confirm", () => {
	const toast = applyAllToast(
		outcome(
			[
				row("hindsight-coding-agent"),
				row("last30days", "Failed to fetch"),
			],
			{ unconfirmed: true, failureDescription: "Request timed out" },
		),
		t,
	);

	assert.equal(toast.kind, "danger");
	assert.equal(toast.title, "sourceUpdatePartialUnconfirmed(1)");
	assert.equal(
		toast.description,
		"last30days: Failed to fetch\nRequest timed out",
	);
	assert.equal(toast.persistent, true);
});

test("a definite 4xx with no rows still counts as a failure", () => {
	const toast = applyAllToast(
		outcome([], {
			definiteFailureCount: 3,
			failureDescription: "names must not exceed 256 per batch",
		}),
		t,
	);

	assert.equal(toast.title, "sourceUpdateSomeFailedMany(3)");
	assert.equal(toast.description, "names must not exceed 256 per batch");
});

test("a clean run is a transient success", () => {
	const toast = applyAllToast(outcome([row("a"), row("b")]), t);

	assert.deepEqual(toast, {
		kind: "success",
		title: "sourceUpdatesApplied(2)",
		description: undefined,
		persistent: false,
	});
});
