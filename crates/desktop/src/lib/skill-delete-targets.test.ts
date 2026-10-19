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
		},
		true,
	);
	assert.equal(named.length, 3);
	assert.equal(m.length, 3);
	assert.equal(unmanaged.length, 0);
});

const globalClaude = (path = "/home/user/.claude/skills/my-skill") => ({
	name: "my-skill",
	agent: "claude",
	source: "global",
	source_path: path,
});
const globalCursor = (path = "/home/user/.cursor/skills/my-skill") => ({
	name: "my-skill",
	agent: "cursor",
	source: "global",
	source_path: path,
});

test("bulk delete: disabled agent is not named in any scope without consent", () => {
	const { requests, skillDeletes, skippedGroupKeys } =
		buildBulkDeleteRequests({
			groups: [
				{ key: "my-skill", items: [globalClaude(), globalCursor()] },
			],
			resourceType: "skill",
			backendHolders: { managed: ["claude"] },
			includeUnmanaged: false,
		});

	assert.deepEqual(requests, []);
	assert.deepEqual(skillDeletes, [
		{
			name: "my-skill",
			scopes: [
				{ scope: "global", projectRoot: null, agents: ["claude"] },
			],
			agents: ["claude"],
		},
	]);
	assert.ok(!JSON.stringify(skillDeletes).includes("cursor"));
	assert.deepEqual(skippedGroupKeys, []);
});

test("bulk delete: consent names the disabled agent in ONE delete, not one per source_path", () => {
	const { skillDeletes, skippedGroupKeys } = buildBulkDeleteRequests({
		groups: [{ key: "my-skill", items: [globalClaude(), globalCursor()] }],
		resourceType: "skill",
		backendHolders: { managed: ["claude"] },
		includeUnmanaged: true,
	});

	assert.deepEqual(skillDeletes, [
		{
			name: "my-skill",
			scopes: [
				{
					scope: "global",
					projectRoot: null,
					agents: ["claude", "cursor"],
				},
			],
			agents: ["claude", "cursor"],
		},
	]);
	assert.deepEqual(skippedGroupKeys, []);
});

test("bulk delete: shared source_path yields one delete carrying only the consented agents", () => {
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
	const managed: BackendHolders = { managed: ["claude"] };

	const withoutConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
		includeUnmanaged: false,
	});
	assert.equal(withoutConsent.skillDeletes.length, 1);
	assert.deepEqual(withoutConsent.skillDeletes[0].agents, ["claude"]);
	assert.deepEqual(withoutConsent.skippedGroupKeys, []);

	const withConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
		includeUnmanaged: true,
	});
	assert.equal(withConsent.skillDeletes.length, 1);
	assert.deepEqual(withConsent.skillDeletes[0].agents, ["claude", "cursor"]);
	assert.deepEqual(withConsent.skippedGroupKeys, []);
});

test("bulk delete: a group held only by a disabled agent is skipped without consent and sent with consent", () => {
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
	const managed: BackendHolders = { managed: [] };

	const withoutConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
		includeUnmanaged: false,
	});
	assert.equal(withoutConsent.skillDeletes.length, 0);
	assert.deepEqual(withoutConsent.skippedGroupKeys, ["orphan-skill"]);

	const withConsent = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: managed,
		includeUnmanaged: true,
	});
	assert.equal(withConsent.skillDeletes.length, 1);
	assert.deepEqual(withConsent.skillDeletes[0].agents, ["cursor"]);
	assert.deepEqual(withConsent.skippedGroupKeys, []);
});

test("bulk delete: mixed selection filters the skill group but leaves the mcp group unchanged", () => {
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
				{ name: "mcp-server", agent: "claude", source: "global" },
				{ name: "mcp-server", agent: "cursor", source: "global" },
			],
		},
	];

	const { requests, skillDeletes, skippedGroupKeys } =
		buildBulkDeleteRequests({
			groups,
			resourceType: "mixed",
			backendHolders: { managed: ["claude"] },
			includeUnmanaged: false,
		});

	assert.deepEqual(skippedGroupKeys, []);
	assert.equal(skillDeletes.length, 1);
	assert.deepEqual(skillDeletes[0].agents, ["claude"]);

	// MCP request is unchanged: all its agents named
	assert.equal(requests.length, 2);
	assert.deepEqual(requests[0].agents, ["claude", "cursor"]);
	assert.deepEqual(requests[1].agents, ["claude", "cursor"]);
});

test("bulk delete: one by-name delete per skill, global scope before project", () => {
	const item = (
		name: string,
		agent: string,
		source: string,
		source_path: string,
	) => ({ name, agent, source, source_path });
	const groups = [
		{
			key: "a",
			items: [
				// Project row listed first: the scope order must still be global first.
				item("a", "codex", "project", "/p/.codex/skills/a"),
				item("a", "claude", "global", "/h/.claude/skills/a"),
				item("a", "claude", "global", "/h/.agents/skills/a"),
				item("a", "cursor", "global", "/h/.cursor/skills/a"),
			],
		},
		{
			key: "b",
			items: [item("b", "claude", "global", "/h/.claude/skills/b")],
		},
	];

	const { requests, skillDeletes } = buildBulkDeleteRequests({
		groups,
		resourceType: "skill",
		backendHolders: { managed: ["claude", "codex"] },
		includeUnmanaged: false,
		projectPath: "/p",
	});

	assert.deepEqual(requests, []);
	assert.deepEqual(skillDeletes, [
		{
			name: "a",
			scopes: [
				{ scope: "global", projectRoot: null, agents: ["claude"] },
				{ scope: "project", projectRoot: "/p", agents: ["codex"] },
			],
			agents: ["claude", "codex"],
		},
		{
			name: "b",
			scopes: [
				{ scope: "global", projectRoot: null, agents: ["claude"] },
			],
			agents: ["claude"],
		},
	]);
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
	};
	const unmanaged = collectUnmanagedDeleteTargets(groups, managed, "skill");
	assert.deepEqual(
		unmanaged.map((item) => item.agent),
		["cursor", "opencode"],
	);
});
