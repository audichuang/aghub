import assert from "node:assert/strict";
import { test } from "node:test";
import { HTTPError } from "ky";
import {
	buildBulkDeleteRequests,
	deleteSkill,
	formatDeleteMessage,
	getUnmanagedAgents,
	interpretRefusal,
	interpretRemovalVerdict,
	splitDeleteTargets,
	type BackendHolders,
	type DeleteSkillIntent,
	type DeleteSkillVerdict,
} from "./delete-skill.ts";

const t = (
	key: string,
	options?: Record<string, string | number | boolean | null | undefined>,
) => {
	const parts = [key];
	if (typeof options?.name === "string") parts.push(options.name);
	if (typeof options?.path === "string") parts.push(options.path);
	if (typeof options?.writtenScope === "string")
		parts.push(options.writtenScope);
	if (typeof options?.failedScope === "string")
		parts.push(options.failedScope);
	if (typeof options?.reason === "string") parts.push(options.reason);
	else if (typeof options?.error === "string") parts.push(options.error);
	return parts.join("|");
};

function createMockHttpError(
	status: number,
	data: {
		code?: string;
		error?: string;
		rejected_targets?: Array<{
			agent: string;
			reason: string;
			kind?: string;
			path?: string;
		}>;
	},
): HTTPError {
	const err = new HTTPError(
		new Response(null, { status }),
		new Request("http://api.test/skills/delete"),
		{} as any,
	);
	(err as any).data = data;
	if (data.error) {
		err.message = data.error;
	}
	return err;
}

// =========================================================================
// 1. Verdict 5 states interpretation & localized formatting
// =========================================================================

test("interpretRemovalVerdict covers all five Verdict states", () => {
	const table: Array<{
		outcome: string | undefined;
		intentKind: DeleteSkillIntent["kind"];
		expected: DeleteSkillVerdict;
	}> = [
		// 1. Removed
		{ outcome: "removed", intentKind: "from-agents", expected: "removed" },
		{ outcome: "removed", intentKind: "all-agents", expected: "removed" },
		{ outcome: "removed", intentKind: "clean-lock", expected: "removed" },

		// 2. Absent
		{ outcome: "absent", intentKind: "from-agents", expected: "absent" },
		{ outcome: "absent", intentKind: "all-agents", expected: "absent" },

		// 3. Kept (shared master or unmanaged reader kept)
		{ outcome: "kept", intentKind: "from-agents", expected: "kept" },
		{ outcome: "kept", intentKind: "all-agents", expected: "kept" },
		{ outcome: "kept", intentKind: "clean-lock", expected: "kept" },

		// 4. Partial
		{ outcome: "partial", intentKind: "from-agents", expected: "partial" },
		{ outcome: "partial", intentKind: "all-agents", expected: "partial" },
		{ outcome: "partial", intentKind: "clean-lock", expected: "partial" },

		// 5. Lock-Only (Sources page intentional distinction: disk is absent, but lock survived)
		{ outcome: "absent", intentKind: "clean-lock", expected: "lock-only" },
	];

	for (const { outcome, intentKind, expected } of table) {
		const actual = interpretRemovalVerdict(outcome as any, intentKind);
		assert.equal(
			actual,
			expected,
			`outcome ${outcome} with intent ${intentKind} must yield verdict ${expected}`,
		);
	}
});

test("formatDeleteMessage provides appropriate localized messages for verdicts", () => {
	// Kept shared master
	assert.equal(
		formatDeleteMessage("kept", { name: "my-skill", t }),
		"deleteSkillKeptSharedMaster|my-skill",
	);

	// Partial
	assert.equal(
		formatDeleteMessage("partial", { name: "my-skill", t }),
		"deleteSkillPartial|my-skill",
	);

	// Lock-only
	assert.equal(
		formatDeleteMessage("lock-only", { name: "my-skill", t }),
		"sourceRemovedCleanLockOnly|my-skill",
	);

	// Refused fallback
	assert.equal(
		formatDeleteMessage("refused", { name: "my-skill", t }),
		"failedToDeleteSkill",
	);
});

test("formatDeleteMessage handles bulk context properly", () => {
	const bulkT = (key: string) => `i18n:${key}`;
	assert.equal(
		formatDeleteMessage("kept", {
			name: "my-skill",
			context: "bulk",
			t: bulkT,
		}),
		"i18n:bulkDeleteKept",
	);
	assert.equal(
		formatDeleteMessage("partial", {
			name: "my-skill",
			context: "bulk",
			t: bulkT,
		}),
		"i18n:bulkDeletePartial",
	);
});

// =========================================================================
// 2. Whole-batch refusal by code and structured fields (NOT error-text)
// =========================================================================

test("whole-batch refusal is identified by code and structured rejected_targets", () => {
	// (b) & (c) 422 UNSUPPORTED_OPERATION with only unmanaged survivors -> retryWithUnmanaged
	// Structured check only: reason text does NOT mention "disabled" or "agent", and hint still fires
	const unmanagedRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "skill reconcile preflight failed; nothing was written",
		rejected_targets: [
			{
				agent: "opencode",
				reason: "location shared: /some/path",
				kind: "shared",
			},
		],
	});

	const refusal1 = interpretRefusal(unmanagedRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["opencode"],
			includeUnmanaged: false,
		},
		unmanagedAgents: ["cursor"],
		t,
	});
	assert.equal(refusal1.isRefusal, true);
	assert.equal(refusal1.code, "UNSUPPORTED_OPERATION");
	assert.equal(refusal1.retryWithUnmanaged, true);
	assert.equal(refusal1.message, "deleteSkillRetryWithUnmanaged");

	// Real two-enabled-holder refusal: claude+codex enabled & named, cursor disabled unticked,
	// rejected_targets all kind "shared" => hint fires
	const twoEnabledRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "skill reconcile preflight failed; nothing was written",
		rejected_targets: [
			{
				agent: "claude",
				reason: "location shared: /path/shared",
				kind: "shared",
			},
			{
				agent: "codex",
				reason: "location shared: /path/shared",
				kind: "shared",
			},
		],
	});
	const refusalTwoEnabled = interpretRefusal(twoEnabledRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["claude", "codex"],
			includeUnmanaged: false,
		},
		unmanagedAgents: ["cursor"],
		t,
	});
	assert.equal(refusalTwoEnabled.isRefusal, true);
	assert.equal(refusalTwoEnabled.code, "UNSUPPORTED_OPERATION");
	assert.equal(refusalTwoEnabled.retryWithUnmanaged, true);
	assert.equal(refusalTwoEnabled.message, "deleteSkillRetryWithUnmanaged");

	// Refusal with non-"shared" kind (or mixed kinds) => no hint, shows reason
	const mixedKindRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "skill reconcile preflight failed",
		rejected_targets: [
			{
				agent: "claude",
				reason: "location shared: /path/shared",
				kind: "shared",
			},
			{
				agent: "zed",
				reason: "unsupported target",
				kind: "unsupported",
			},
		],
	});
	const refusalMixed = interpretRefusal(mixedKindRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["claude"],
			includeUnmanaged: false,
		},
		unmanagedAgents: ["cursor"],
		t,
	});
	assert.equal(refusalMixed.isRefusal, true);
	assert.equal(refusalMixed.retryWithUnmanaged, false);
	assert.equal(
		refusalMixed.message,
		"location shared: /path/shared; unsupported target",
	);

	// includeUnmanaged=true => no hint
	const refusalWithIncludeUnmanaged = interpretRefusal(twoEnabledRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["claude", "codex", "cursor"],
			includeUnmanaged: true,
		},
		unmanagedAgents: ["cursor"],
		t,
	});
	assert.equal(refusalWithIncludeUnmanaged.isRefusal, true);
	assert.equal(refusalWithIncludeUnmanaged.retryWithUnmanaged, false);
	assert.equal(
		refusalWithIncludeUnmanaged.message,
		"location shared: /path/shared; location shared: /path/shared",
	);

	// 422 UNSUPPORTED_OPERATION with git kind -> NOT retryWithUnmanaged, displays git message
	const gitRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "git tracked refusal",
		rejected_targets: [
			{
				agent: "claude",
				reason: "/path is tracked by git",
				kind: "git",
				path: "/path/to/skill",
			},
		],
	});

	const refusal2 = interpretRefusal(gitRefusal, {
		intent: { kind: "all-agents", includeUnmanaged: false },
		unmanagedAgents: ["cursor"],
		skillName: "my-skill",
		t,
	});
	assert.equal(refusal2.isRefusal, true);
	assert.equal(refusal2.retryWithUnmanaged, false);
	assert.equal(
		refusal2.message,
		"deleteSkillKeptGit|my-skill|/path/to/skill",
	);

	// 400 INVALID_CONFIG with duplicate masters -> NOT retryWithUnmanaged, displays real reason
	const dupMasterRefusal = createMockHttpError(400, {
		code: "INVALID_CONFIG",
		error: "skill reconcile preflight failed; nothing was written: duplicate masters",
		rejected_targets: [
			{
				agent: "claude",
				reason: "two Masters found in .aghub and .agents",
			},
		],
	});

	const refusal3 = interpretRefusal(dupMasterRefusal, {
		intent: { kind: "all-agents", includeUnmanaged: false },
		unmanagedAgents: ["cursor"],
		t,
	});
	assert.equal(refusal3.isRefusal, true);
	assert.equal(refusal3.retryWithUnmanaged, false);
	assert.ok(
		refusal3.message?.includes("two Masters found"),
		"must display the real reason from rejected_targets, not retryWithUnmanaged",
	);

	// 422 UNSUPPORTED_OPERATION for agent without project config (zed) -> displays real reason
	const zedRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "skill reconcile preflight failed; nothing was written: no project skill config for zed",
		rejected_targets: [
			{
				agent: "zed",
				reason: "Zed agent has no project skill config",
			},
		],
	});

	const refusal4 = interpretRefusal(zedRefusal, {
		intent: { kind: "all-agents", includeUnmanaged: false },
		unmanagedAgents: ["cursor"],
		t,
	});
	assert.equal(refusal4.isRefusal, true);
	assert.equal(refusal4.retryWithUnmanaged, false);
	assert.ok(
		refusal4.message?.includes("Zed agent has no project skill config"),
		"must display the real reason for unsupported scope",
	);
});

test("bulk delete refusal with includeUnmanaged=true does not produce retry hint", async () => {
	const sharedRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "skill reconcile preflight failed; nothing was written",
		rejected_targets: [
			{
				agent: "claude",
				reason: "location shared: /path/shared",
				kind: "shared",
			},
		],
	});

	const api = {
		skills: {
			delete: async () => {
				throw sharedRefusal;
			},
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "my-skill",
		agent: "claude",
		scope: "global",
		context: "bulk",
		intent: {
			kind: "from-agents",
			agents: ["claude", "cursor"],
			includeUnmanaged: true,
		},
		unmanagedAgents: ["cursor"],
		t,
	});

	assert.equal(result.success, false);
	assert.equal(result.verdict, "refused");
	assert.equal(result.retryWithUnmanaged, false);
	assert.notEqual(result.message, "deleteSkillRetryWithUnmanaged");
	assert.equal(result.message, "location shared: /path/shared");
});

// =========================================================================
// 3. Cross-scope deletion & multi-row aggregation
// =========================================================================

test("cross-scope deleteSkill reports per-scope results and never claims nothing was written when earlier scope succeeded", async () => {
	let globalCalled = false;
	let projectCalled = false;

	const api = {
		skills: {
			reconcile: async (req: any) => {
				if (req.source.scope === "global") {
					globalCalled = true;
					return {
						success_count: 1,
						failed_count: 0,
						results: [
							{
								agent: "claude",
								scope: "global",
								project_root: null,
								action: "delete",
								success: true,
								ok: true,
								already_present: false,
								error: null,
								outcome: "removed",
							},
						],
					};
				}
				if (req.source.scope === "project") {
					projectCalled = true;
					// Project scope preflight refusal!
					throw createMockHttpError(422, {
						code: "UNSUPPORTED_OPERATION",
						error: "skill reconcile preflight failed; nothing was written: delete zed (project): no project config",
						rejected_targets: [
							{
								agent: "zed",
								reason: "Zed agent has no project skill config",
							},
						],
					});
				}
				throw new Error("unexpected scope");
			},
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "cross-skill",
		scopes: [
			{ scope: "global", agents: ["claude"] },
			{ scope: "project", projectRoot: "/proj", agents: ["zed"] },
		],
		intent: { kind: "all-agents" },
		t,
	});

	assert.equal(globalCalled, true, "global scope must have been called");
	assert.equal(projectCalled, true, "project scope must have been called");
	assert.equal(
		result.success,
		false,
		"overall operation failed due to project refusal",
	);
	assert.equal(result.verdict, "partial");

	// Critical check: Message MUST NOT say "nothing was written" or imply nothing happened
	assert.ok(
		!result.message?.includes("nothing was written"),
		"cross-scope failure must not imply nothing was written when global wrote",
	);
	assert.equal(
		result.message,
		"deleteSkillCrossScopePartial|scopeGlobal|scopeProject|Zed agent has no project skill config",
	);
});

test("cross-scope deleteSkill correctly handles project written and global failed", async () => {
	const api = {
		skills: {
			reconcile: async (req: any) => {
				if (req.source.scope === "project") {
					return {
						success_count: 1,
						failed_count: 0,
						results: [
							{
								agent: "claude",
								scope: "project",
								project_root: "/proj",
								action: "delete",
								success: true,
								ok: true,
								already_present: false,
								error: null,
								outcome: "removed",
							},
						],
					};
				}
				if (req.source.scope === "global") {
					throw createMockHttpError(422, {
						code: "UNSUPPORTED_OPERATION",
						error: "global refusal",
						rejected_targets: [
							{
								agent: "zed",
								reason: "global failed for zed",
							},
						],
					});
				}
				throw new Error("unexpected scope");
			},
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "cross-skill",
		scopes: [
			{ scope: "project", projectRoot: "/proj", agents: ["claude"] },
			{ scope: "global", agents: ["zed"] },
		],
		intent: { kind: "all-agents" },
		t,
	});

	assert.equal(result.success, false);
	assert.equal(result.verdict, "partial");
	assert.equal(
		result.message,
		"deleteSkillCrossScopePartial|scopeProject|scopeGlobal|global failed for zed",
	);
});

test("cross-scope deleteSkill when first scope fails reports failure without claiming any scope was written", async () => {
	const api = {
		skills: {
			reconcile: async () => {
				throw createMockHttpError(422, {
					code: "UNSUPPORTED_OPERATION",
					error: "global refusal",
					rejected_targets: [
						{
							agent: "claude",
							reason: "cannot delete for claude",
						},
					],
				});
			},
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "cross-skill",
		scopes: [
			{ scope: "global", agents: ["claude"] },
			{ scope: "project", projectRoot: "/proj", agents: ["claude"] },
		],
		intent: { kind: "all-agents" },
		t,
	});

	assert.equal(result.success, false);
	assert.equal(result.verdict, "refused");
	assert.equal(result.message, "cannot delete for claude");
});

test("cross-scope deleteSkill when scope 1 is absent and scope 2 is refused reports scope 2 failure without claiming scope 1 was written", async () => {
	const api = {
		skills: {
			reconcile: async (req: any) => {
				if (req.source.scope === "global") {
					return {
						success_count: 1,
						failed_count: 0,
						results: [
							{
								agent: "claude",
								scope: "global",
								project_root: null,
								action: "delete",
								success: true,
								ok: true,
								already_present: false,
								error: null,
								outcome: "absent",
							},
						],
					};
				}
				if (req.source.scope === "project") {
					throw createMockHttpError(422, {
						code: "UNSUPPORTED_OPERATION",
						error: "project refusal",
						rejected_targets: [
							{
								agent: "zed",
								reason: "Zed agent has no project skill config",
							},
						],
					});
				}
				throw new Error("unexpected scope");
			},
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "absent-then-fail",
		scopes: [
			{ scope: "global", agents: ["claude"] },
			{ scope: "project", projectRoot: "/proj", agents: ["zed"] },
		],
		intent: { kind: "all-agents" },
		t,
	});

	assert.equal(result.success, false);
	assert.equal(
		result.verdict,
		"refused",
		"overall verdict must be refused, not partial, because nothing was written",
	);
	assert.equal(
		result.message,
		"Zed agent has no project skill config",
		"message must not claim global was written",
	);
});

test("cross-scope deleteSkill where all scopes return absent yields verdict 'absent'", async () => {
	const api = {
		skills: {
			reconcile: async () => ({
				success_count: 1,
				failed_count: 0,
				results: [
					{
						agent: "claude",
						scope: "global",
						action: "delete",
						success: true,
						outcome: "absent",
					},
				],
			}),
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "both-absent",
		scopes: [
			{ scope: "global", agents: ["claude"] },
			{ scope: "project", projectRoot: "/proj", agents: ["claude"] },
		],
		intent: { kind: "all-agents" },
		t,
	});

	assert.equal(result.success, true);
	assert.equal(result.verdict, "absent");
});

test("cross-scope delete aggregates all delete rows and detects partial when one agent is removed and another is kept", async () => {
	const api = {
		skills: {
			reconcile: async () => ({
				success_count: 2,
				failed_count: 0,
				results: [
					{
						agent: "claude",
						scope: "global",
						action: "delete",
						success: true,
						outcome: "removed",
					},
					{
						agent: "cursor",
						scope: "global",
						action: "delete",
						success: true,
						outcome: "kept",
					},
				],
			}),
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "my-skill",
		scopes: [{ scope: "global", agents: ["claude", "cursor"] }],
		intent: { kind: "from-agents", agents: ["claude", "cursor"] },
		t,
	});

	assert.equal(
		result.success,
		false,
		"overall must fail if one agent is kept",
	);
	assert.equal(
		result.verdict,
		"partial",
		"mixed removed and kept must yield partial",
	);
});

// =========================================================================
// 4. Backend fields for still_read_by & dry-run preview (Findings 1, 2, 8)
// =========================================================================

test("deleteSkill returns unmanagedKept directly from backend still_read_by_unmanaged field", async () => {
	const api = {
		skills: {
			deleteByPath: async () => ({
				success: true,
				dry_run: false,
				executed: true,
				needs_confirm: false,
				paths: ["/path"],
				skipped: [],
				deleted_path: "/path",
				outcome: "removed",
				still_read_by: ["cursor"],
				still_read_by_managed: [],
				still_read_by_unmanaged: ["cursor"],
			}),
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "my-skill",
		intent: { kind: "by-path", sourcePath: "/path", agents: ["claude"] },
		t,
	});

	assert.equal(result.success, true);
	assert.equal(result.verdict, "removed");
	assert.equal(result.unmanagedKept, true);
	assert.deepEqual(result.stillReadByUnmanaged, ["cursor"]);
});

test("splitDeleteTargets classifies managed based on backend fields, not frontend rule", () => {
	const items = [
		{ agent: "claude", source: "global" },
		{ agent: "cursor", source: "global" },
		{ agent: "opencode", source: "global" },
	];

	// Backend classification has still_read_by_managed empty (targeted agent credited away):
	const backend: BackendHolders = {
		still_read_by_managed: [],
		still_read_by_unmanaged: ["cursor", "opencode"],
	};

	const { named, managed, unmanaged } = splitDeleteTargets(
		items,
		backend,
		false,
	);
	assert.deepEqual(
		managed.map((i) => i.agent),
		["claude"],
	);
	assert.deepEqual(
		unmanaged.map((i) => i.agent),
		["cursor", "opencode"],
	);
	assert.deepEqual(
		named.map((i) => i.agent),
		["claude"],
	);
});

test("getUnmanagedAgents fetches disabled agents from backend without dry-run delete", async () => {
	const api = {
		agents: {
			disabled: async () => ({
				configured: true,
				agents: ["cursor", "opencode"],
			}),
		},
		skills: {
			delete: async () => {
				throw new Error(
					"skills.delete must not be called by getUnmanagedAgents",
				);
			},
		},
	} as any;

	const holders = await getUnmanagedAgents(api);

	assert.deepEqual(holders.still_read_by_unmanaged, ["cursor", "opencode"]);

	const items = [
		{ agent: "claude", source: "global" },
		{ agent: "cursor", source: "global" },
		{ agent: "opencode", source: "global" },
	];
	const { managed, unmanaged } = splitDeleteTargets(items, holders, false);
	assert.deepEqual(
		managed.map((i) => i.agent),
		["claude"],
	);
	assert.deepEqual(
		unmanaged.map((i) => i.agent),
		["cursor", "opencode"],
	);
});

test("first item's agent is disabled => not in named/request until includeUnmanaged is ticked", () => {
	const backendHolders: BackendHolders = {
		still_read_by_unmanaged: ["cursor"],
	};

	// cursor is the FIRST item in the group
	const items = [
		{ name: "my-skill", agent: "cursor", source: "global" as const },
		{ name: "my-skill", agent: "claude", source: "global" as const },
	];

	// Unticked: cursor is not in named
	const unticked = splitDeleteTargets(items, backendHolders, false);
	assert.deepEqual(
		unticked.managed.map((i) => i.agent),
		["claude"],
	);
	assert.deepEqual(
		unticked.unmanaged.map((i) => i.agent),
		["cursor"],
	);
	assert.deepEqual(
		unticked.named.map((i) => i.agent),
		["claude"],
	);

	// In bulk request building, cursor is not in requests without consent
	const untickedBulk = buildBulkDeleteRequests({
		groups: [{ key: "my-skill", items }],
		resourceType: "skill",
		backendHolders,
		includeUnmanaged: false,
	});
	assert.equal(untickedBulk.requests.length, 1);
	assert.equal(untickedBulk.requests[0].agent, "claude");
	assert.deepEqual(untickedBulk.requests[0].agents, ["claude"]);

	// Ticked: cursor is included in named and request
	const ticked = splitDeleteTargets(items, backendHolders, true);
	assert.deepEqual(
		ticked.named.map((i) => i.agent),
		["claude", "cursor"],
	);
	const tickedBulk = buildBulkDeleteRequests({
		groups: [{ key: "my-skill", items }],
		resourceType: "skill",
		backendHolders,
		includeUnmanaged: true,
	});
	assert.ok(tickedBulk.requests.some((r) => r.agent === "cursor"));
	assert.ok(tickedBulk.requests[0].agents.includes("cursor"));
});

test("mixed-scope group: disabled agent is not named in either scope's request", () => {
	const backendHolders: BackendHolders = {
		still_read_by_unmanaged: ["cursor"],
	};

	const group = {
		key: "my-skill",
		items: [
			{
				name: "my-skill",
				agent: "claude",
				source: "global" as const,
			},
			{
				name: "my-skill",
				agent: "cursor",
				source: "project" as const,
				source_path: "/project/.agents/skills/my-skill",
			},
		],
	};

	const { requests, skippedGroupKeys } = buildBulkDeleteRequests({
		groups: [group],
		resourceType: "skill",
		backendHolders,
		includeUnmanaged: false,
		projectPath: "/project",
	});

	// Only global scope for claude should be requested; project scope held only by disabled cursor must not be requested
	assert.equal(requests.length, 1);
	assert.equal(requests[0].scope, "global");
	assert.equal(requests[0].agent, "claude");
	assert.deepEqual(requests[0].agents, ["claude"]);
	assert.ok(!requests.some((r) => r.agent === "cursor"));
	assert.ok(!requests.some((r) => r.agents.includes("cursor")));
	assert.ok(!requests.some((r) => r.scope === "project"));
	assert.deepEqual(skippedGroupKeys, []);
});

test("deleteSkill returns verdict 'absent' when backend returns outcome 'absent', not 'removed'", async () => {
	const api = {
		skills: {
			delete: async () => ({
				outcome: "absent",
				still_read_by_managed: [],
				still_read_by_unmanaged: [],
			}),
		},
	} as any;

	const res = await deleteSkill({
		api,
		skillName: "withheld-skill",
		agent: "claude",
		scope: "global",
		intent: { kind: "all-agents" },
		t,
	});

	assert.equal(res.verdict, "absent");
	assert.notEqual(res.verdict, "removed");
});

test("deleteSkill by-path with project location passes scope='project' and project_root to api.skills.deleteByPath", async () => {
	let deleteByPathPayload: any = null;
	const api = {
		skills: {
			deleteByPath: async (payload: any) => {
				deleteByPathPayload = payload;
				return {
					success: true,
					dry_run: false,
					executed: true,
					needs_confirm: false,
					paths: ["/my/project/.agents/skills/test-skill"],
					skipped: [],
					deleted_path: "/my/project/.agents/skills/test-skill",
					outcome: "removed",
					still_read_by: [],
					still_read_by_managed: [],
					still_read_by_unmanaged: [],
				};
			},
		},
	} as any;

	const result = await deleteSkill({
		api,
		skillName: "test-skill",
		intent: {
			kind: "by-path",
			sourcePath: "/my/project/.agents/skills/test-skill",
			scope: "project",
			projectRoot: "/my/project",
			agents: ["claude"],
		},
		t,
	});

	assert.equal(result.success, true);
	assert.equal(result.verdict, "removed");
	assert.ok(deleteByPathPayload, "deleteByPath must be called");
	assert.equal(
		deleteByPathPayload.source_path,
		"/my/project/.agents/skills/test-skill",
	);
	assert.equal(deleteByPathPayload.scope, "project");
	assert.equal(deleteByPathPayload.project_root, "/my/project");
	assert.deepEqual(deleteByPathPayload.agents, ["claude"]);
	assert.equal(deleteByPathPayload.confirm, true);
});

test("deleteSkill by-name throws if agent is not provided", async () => {
	const api = { skills: { delete: async () => {} } } as any;
	await assert.rejects(
		deleteSkill({
			api,
			skillName: "my-skill",
			intent: { kind: "all-agents" },
			t,
		}),
		/agent is required for by-name skill deletion/,
	);
});
