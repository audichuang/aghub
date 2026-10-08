import assert from "node:assert/strict";
import { test } from "node:test";
import { HTTPError } from "ky";
import {
	deleteSkill,
	formatDeleteMessage,
	getBulkSkillHolders,
	getSkillHolders,
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

	// Kept git tracked
	assert.equal(
		formatDeleteMessage("kept", {
			name: "my-skill",
			path: "/home/user/.agents/skills/my-skill",
			isGit: true,
			t,
		}),
		"deleteSkillKeptGit|my-skill|/home/user/.agents/skills/my-skill",
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
		managedSurvivors: [],
		t,
	});
	assert.equal(refusal1.isRefusal, true);
	assert.equal(refusal1.code, "UNSUPPORTED_OPERATION");
	assert.equal(refusal1.retryWithUnmanaged, true);
	assert.equal(refusal1.message, "deleteSkillRetryWithUnmanaged");

	// (a) Managed survivor + unmanaged preview -> no hint, reason shown
	const managedSurvivorRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "skill reconcile preflight failed; nothing was written",
		rejected_targets: [
			{
				agent: "claude",
				reason: "location shared: /some/path",
				kind: "shared",
			},
		],
	});

	const refusalManaged = interpretRefusal(managedSurvivorRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["claude"],
			includeUnmanaged: false,
		},
		unmanagedAgents: ["cursor"],
		managedSurvivors: ["claude"],
		t,
	});
	assert.equal(refusalManaged.isRefusal, true);
	assert.equal(refusalManaged.retryWithUnmanaged, false);
	assert.equal(refusalManaged.message, "location shared: /some/path");

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
	assert.equal(result.scopeResults?.length, 2);
	assert.equal(result.scopeResults[0].scope, "global");
	assert.equal(result.scopeResults[0].success, true);
	assert.equal(result.scopeResults[0].verdict, "removed");
	assert.equal(result.scopeResults[1].scope, "project");
	assert.equal(result.scopeResults[1].success, false);
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
	assert.equal(result.scopeResults?.[0]?.verdict, "absent");
	assert.equal(result.scopeResults?.[1]?.verdict, "refused");
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
	assert.equal(result.scopeResults?.[0]?.verdict, "partial");
	assert.ok(
		result.scopeResults?.[0]?.error?.includes("cursor: kept"),
		"scope error must record that cursor was kept",
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

test("getSkillHolders retrieves authoritative classification from backend dry-run preview and overrides isDisabled", async () => {
	let deleteCalledWith: any = null;
	const api = {
		skills: {
			delete: async (
				agent: string,
				skillName: string,
				scope: string,
				projectRoot: string | undefined,
				allAgents: boolean,
				agents: string[],
				confirm: boolean,
			) => {
				deleteCalledWith = {
					agent,
					skillName,
					scope,
					projectRoot,
					allAgents,
					agents,
					confirm,
				};
				return {
					outcome: "kept",
					still_read_by_managed: [],
					still_read_by_unmanaged: ["cursor", "opencode"],
				};
			},
		},
	} as any;

	const holders = await getSkillHolders({
		api,
		skillName: "my-skill",
		agent: "claude",
		scope: "global",
	});

	assert.equal(
		deleteCalledWith.confirm,
		false,
		"dry-run preview must pass confirm: false",
	);
	assert.equal(deleteCalledWith.allAgents, true);
	assert.deepEqual(holders.still_read_by_managed, []);
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

test("getBulkSkillHolders queries holders for each skill group and aggregates unmanaged agents", async () => {
	const queries: string[] = [];
	const api = {
		skills: {
			delete: async (agent: string, skillName: string) => {
				queries.push(`${agent}:${skillName}`);
				if (skillName === "skill-1") {
					return {
						outcome: "kept",
						still_read_by_managed: [],
						still_read_by_unmanaged: ["cursor"],
					};
				}
				return {
					outcome: "kept",
					still_read_by_managed: [],
					still_read_by_unmanaged: ["opencode"],
				};
			},
		},
	} as any;

	const groups = [
		{
			key: "skill-1",
			items: [
				{ agent: "claude", source: "global" as const, name: "skill-1" },
			],
		},
		{
			key: "skill-2",
			items: [
				{ agent: "claude", source: "global" as const, name: "skill-2" },
			],
		},
	];

	const res = await getBulkSkillHolders(api, groups, "skill");

	assert.deepEqual(queries, ["claude:skill-1", "claude:skill-2"]);
	assert.deepEqual(
		[...(res.still_read_by_unmanaged ?? [])].sort(),
		["cursor", "opencode"].sort(),
	);
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
