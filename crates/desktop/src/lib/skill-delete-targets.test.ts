import assert from "node:assert/strict";
import { test } from "node:test";
import {
	buildBulkDeleteRequests,
	collectUnmanagedDeleteTargets,
	splitDeleteTargets,
} from "./skill-delete-targets.ts";

const items = [
	{ agent: "claude", source: "global" },
	{ agent: "cursor", source: "global" },
	{ agent: "opencode", source: "global" },
	{ agent: null, source: "global" },
];
const managed = new Set(["claude"]);

test("a disabled agent is not named unless the user opts in", () => {
	const { named, unmanaged } = splitDeleteTargets(items, managed, false);
	assert.deepEqual(
		named.map((item) => item.agent),
		["claude"],
	);
	assert.deepEqual(
		unmanaged.map((item) => item.agent),
		["cursor", "opencode"],
	);
});

test("opting in names the disabled agents too, managed rows first", () => {
	const { named } = splitDeleteTargets(items, managed, true);
	assert.deepEqual(
		named.map((item) => item.agent),
		["claude", "cursor", "opencode"],
	);
});

test("a row with no agent is never named", () => {
	const {
		named,
		managed: m,
		unmanaged,
	} = splitDeleteTargets(
		items,
		new Set(["claude", "cursor", "opencode"]),
		true,
	);
	assert.equal(named.length, 3);
	assert.equal(m.length, 3);
	assert.equal(unmanaged.length, 0);
});

test("bulk delete: disabled agent is not named and its source_path delete is not sent without consent", () => {
	const groups = [
		{
			key: "my-skill",
			items: [
				{
					name: "my-skill",
					agent: "claude",
					source: "global",
					source_path: "/home/user/.claude/skills/my-skill",
				},
				{
					name: "my-skill",
					agent: "cursor",
					source: "global",
					source_path: "/home/user/.cursor/skills/my-skill",
				},
			],
		},
	];
	const managed = new Set(["claude"]);

	const requests = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		managedAgentIds: managed,
		includeUnmanaged: false,
	});

	assert.deepEqual(
		requests.map((r) => r.agent),
		["claude"],
	);
	assert.deepEqual(requests[0].agents, ["claude"]);
	assert.equal(requests[0].sourcePath, "/home/user/.claude/skills/my-skill");
	assert.ok(!requests.some((r) => r.agent === "cursor"));
	assert.ok(
		!requests.some(
			(r) => r.sourcePath === "/home/user/.cursor/skills/my-skill",
		),
	);
	assert.ok(!requests.some((r) => r.agents.includes("cursor")));
});

test("bulk delete: disabled agent is named and its source_path delete is sent when consent is ticked", () => {
	const groups = [
		{
			key: "my-skill",
			items: [
				{
					name: "my-skill",
					agent: "claude",
					source: "global",
					source_path: "/home/user/.claude/skills/my-skill",
				},
				{
					name: "my-skill",
					agent: "cursor",
					source: "global",
					source_path: "/home/user/.cursor/skills/my-skill",
				},
			],
		},
	];
	const managed = new Set(["claude"]);

	const requests = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		managedAgentIds: managed,
		includeUnmanaged: true,
	});

	assert.deepEqual(
		requests.map((r) => r.agent),
		["claude", "cursor"],
	);
	assert.deepEqual(requests[0].agents, ["claude", "cursor"]);
	assert.deepEqual(requests[1].agents, ["claude", "cursor"]);
	assert.equal(requests[0].sourcePath, "/home/user/.claude/skills/my-skill");
	assert.equal(requests[1].sourcePath, "/home/user/.cursor/skills/my-skill");
});

test("bulk delete: shared source_path is deduplicated but carries consented agents", () => {
	const groups = [
		{
			key: "shared-skill",
			items: [
				{
					name: "shared-skill",
					agent: "claude",
					source: "global",
					source_path: "/shared/skills/shared-skill",
				},
				{
					name: "shared-skill",
					agent: "cursor",
					source: "global",
					source_path: "/shared/skills/shared-skill",
				},
			],
		},
	];
	const managed = new Set(["claude"]);

	const withoutConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		managedAgentIds: managed,
		includeUnmanaged: false,
	});
	assert.equal(withoutConsent.length, 1);
	assert.deepEqual(withoutConsent[0].agents, ["claude"]);

	const withConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		managedAgentIds: managed,
		includeUnmanaged: true,
	});
	assert.equal(withConsent.length, 1);
	assert.deepEqual(withConsent[0].agents, ["claude", "cursor"]);
});

test("collectUnmanagedDeleteTargets returns unmanaged items across skill groups", () => {
	const groups = [
		{
			key: "skill-1",
			items: [
				{ name: "skill-1", agent: "claude", source: "global" },
				{ name: "skill-1", agent: "cursor", source: "global" },
			],
		},
		{
			key: "skill-2",
			items: [{ name: "skill-2", agent: "opencode", source: "global" }],
		},
	];
	const managed = new Set(["claude"]);
	const unmanaged = collectUnmanagedDeleteTargets(groups, managed, "skill");
	assert.deepEqual(
		unmanaged.map((item) => item.agent),
		["cursor", "opencode"],
	);
});
