import assert from "node:assert/strict";
import { test } from "node:test";
import {
	buildBulkDeleteRequests,
	collectUnmanagedDeleteTargets,
	splitDeleteTargets,
	type BackendHolders,
} from "../requests/delete-skill.ts";

const items = [
	{ agent: "claude", source: "global" },
	{ agent: "cursor", source: "global" },
	{ agent: "opencode", source: "global" },
	{ agent: null, source: "global" },
];
const managed: BackendHolders = {
	managed: ["claude"],
	unmanaged: ["cursor", "opencode"],
	still_read_by_unmanaged: ["cursor", "opencode"],
};

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
		{
			managed: ["claude", "cursor", "opencode"],
			unmanaged: [],
			still_read_by_unmanaged: [],
		},
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
	const managed: BackendHolders = {
		managed: ["claude"],
		unmanaged: ["cursor"],
		still_read_by_unmanaged: ["cursor"],
	};

	const { requests, skippedGroupKeys } = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
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
	assert.deepEqual(skippedGroupKeys, []);
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
	const managed: BackendHolders = {
		managed: ["claude"],
		unmanaged: ["cursor"],
		still_read_by_unmanaged: ["cursor"],
	};

	const { requests, skippedGroupKeys } = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
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
	assert.deepEqual(skippedGroupKeys, []);
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
	const managed: BackendHolders = {
		managed: ["claude"],
		unmanaged: ["cursor"],
		still_read_by_unmanaged: ["cursor"],
	};

	const withoutConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
		includeUnmanaged: false,
	});
	assert.equal(withoutConsent.requests.length, 1);
	assert.deepEqual(withoutConsent.requests[0].agents, ["claude"]);
	assert.deepEqual(withoutConsent.skippedGroupKeys, []);

	const withConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
		includeUnmanaged: true,
	});
	assert.equal(withConsent.requests.length, 1);
	assert.deepEqual(withConsent.requests[0].agents, ["claude", "cursor"]);
	assert.deepEqual(withConsent.skippedGroupKeys, []);
});

test("bulk delete: a group held only by a disabled agent is skipped without consent and requested with consent", () => {
	const groups = [
		{
			key: "orphan-skill",
			items: [
				{
					name: "orphan-skill",
					agent: "cursor",
					source: "global",
					source_path: "/home/user/.cursor/skills/orphan-skill",
				},
			],
		},
	];
	const managed: BackendHolders = {
		managed: [],
		unmanaged: ["cursor"],
		still_read_by_unmanaged: ["cursor"],
	};

	// No consent: appears in skippedGroupKeys, produces no request
	const withoutConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
		includeUnmanaged: false,
	});
	assert.equal(withoutConsent.requests.length, 0);
	assert.deepEqual(withoutConsent.skippedGroupKeys, ["orphan-skill"]);

	// With consent: request produced, not skipped
	const withConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
		includeUnmanaged: true,
	});
	assert.equal(withConsent.requests.length, 1);
	assert.equal(withConsent.requests[0].agent, "cursor");
	assert.deepEqual(withConsent.requests[0].agents, ["cursor"]);
	assert.deepEqual(withConsent.skippedGroupKeys, []);
});

test("bulk delete: mixed selection filters skill group but leaves mcp group unchanged", () => {
	const groups = [
		{
			key: "skill-group",
			resourceType: "skill" as const,
			items: [
				{
					name: "skill-group",
					agent: "claude",
					source: "global",
					source_path: "/path/claude",
				},
				{
					name: "skill-group",
					agent: "cursor",
					source: "global",
					source_path: "/path/cursor",
				},
			],
		},
		{
			key: "mcp-group",
			resourceType: "mcp" as const,
			items: [
				{
					name: "mcp-server",
					agent: "claude",
					source: "global",
				},
				{
					name: "mcp-server",
					agent: "cursor",
					source: "global",
				},
			],
		},
	];
	const managed: BackendHolders = {
		managed: ["claude"],
		unmanaged: ["cursor"],
		still_read_by_unmanaged: ["cursor"],
	};

	const { requests, skippedGroupKeys } = buildBulkDeleteRequests({
		groups,
		resourceType: "mixed",
		backendHolders: managed,
		includeUnmanaged: false,
	});

	assert.deepEqual(skippedGroupKeys, []);

	const skillReqs = requests.filter((r) => r.resourceType === "skill");
	const mcpReqs = requests.filter((r) => r.resourceType === "mcp");

	// Skill request leaves out the disabled agent cursor
	assert.equal(skillReqs.length, 1);
	assert.equal(skillReqs[0].agent, "claude");
	assert.deepEqual(skillReqs[0].agents, ["claude"]);

	// MCP request is unchanged: all its agents named
	assert.equal(mcpReqs.length, 2);
	assert.deepEqual(mcpReqs[0].agents, ["claude", "cursor"]);
	assert.deepEqual(mcpReqs[1].agents, ["claude", "cursor"]);
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
	const managed: BackendHolders = {
		managed: ["claude"],
		unmanaged: ["cursor", "opencode"],
		still_read_by_unmanaged: ["cursor", "opencode"],
	};
	const unmanaged = collectUnmanagedDeleteTargets(groups, managed, "skill");
	assert.deepEqual(
		unmanaged.map((item) => item.agent),
		["cursor", "opencode"],
	);
});
