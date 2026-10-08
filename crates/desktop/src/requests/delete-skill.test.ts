import assert from "node:assert/strict";
import { test } from "node:test";
import { HTTPError } from "ky";
import {
	buildBulkDeleteRequests,
	deleteSkill,
	fetchBulkHolders,
	fetchSkillHoldersForGroup,
	formatDeleteMessage,
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
			readers?: Array<{
				agent: string;
				managed: boolean;
			}>;
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
				readers: [
					{
						agent: "cursor",
						managed: false,
					},
				],
			},
		],
	});

	const refusal1 = interpretRefusal(unmanagedRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["opencode"],
			includeUnmanaged: false,
		},
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
				readers: [
					{
						agent: "cursor",
						managed: false,
					},
				],
			},
			{
				agent: "codex",
				reason: "location shared: /path/shared",
				kind: "shared",
				readers: [
					{
						agent: "cursor",
						managed: false,
					},
				],
			},
		],
	});
	const refusalTwoEnabled = interpretRefusal(twoEnabledRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["claude", "codex"],
			includeUnmanaged: false,
		},
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
				readers: [
					{
						agent: "cursor",
						managed: false,
					},
				],
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
		skillName: "my-skill",
		t,
	});
	assert.equal(refusal2.isRefusal, true);
	assert.equal(refusal2.retryWithUnmanaged, false);
	assert.equal(
		refusal2.message,
		"deleteSkillKeptGit|my-skill|/path/to/skill",
	);

	// 400 INVALID_CONFIG is NOT classified as a whole-batch refusal (fails through)
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
		t,
	});
	assert.equal(refusal3.isRefusal, false);
	assert.equal(refusal3.retryWithUnmanaged, false);
	assert.ok(
		refusal3.message?.includes("duplicate masters"),
		"must pass through the error message, not whole-batch refusal",
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
		t,
	});

	assert.equal(result.success, false);
	assert.equal(result.verdict, "refused");
	assert.equal(result.retryWithUnmanaged, false);
	assert.notEqual(result.message, "deleteSkillRetryWithUnmanaged");
	assert.equal(result.message, "location shared: /path/shared");
});

test("shared UNSUPPORTED_OPERATION refusal caused by a managed reader does not produce retry hint", () => {
	const sharedRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "skill reconcile preflight failed; nothing was written",
		rejected_targets: [
			{
				agent: "claude",
				reason: "location shared: /path/shared",
				kind: "shared",
				readers: [{ agent: "opencode", managed: true }],
			},
		],
	});

	const refusal = interpretRefusal(sharedRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["claude"],
			includeUnmanaged: false,
		},
		t,
	});

	assert.equal(refusal.isRefusal, true);
	assert.equal(refusal.code, "UNSUPPORTED_OPERATION");
	assert.equal(refusal.retryWithUnmanaged, false);
	assert.notEqual(refusal.message, "deleteSkillRetryWithUnmanaged");
	assert.equal(refusal.message, "location shared: /path/shared");
});

test("shared UNSUPPORTED_OPERATION refusal where all readers are unmanaged produces retry hint", () => {
	const sharedRefusal = createMockHttpError(422, {
		code: "UNSUPPORTED_OPERATION",
		error: "skill reconcile preflight failed; nothing was written",
		rejected_targets: [
			{
				agent: "claude",
				reason: "location shared: /path/shared",
				kind: "shared",
				readers: [{ agent: "cursor", managed: false }],
			},
		],
	});

	const refusal = interpretRefusal(sharedRefusal, {
		intent: {
			kind: "from-agents",
			agents: ["claude"],
			includeUnmanaged: false,
		},
		t,
	});

	assert.equal(refusal.isRefusal, true);
	assert.equal(refusal.code, "UNSUPPORTED_OPERATION");
	assert.equal(refusal.retryWithUnmanaged, true);
	assert.equal(refusal.message, "deleteSkillRetryWithUnmanaged");
});

// =========================================================================
// 3. Cross-scope deletion & multi-row aggregation
// =========================================================================

test("cross-scope deleteSkill handles scope outcomes according to table cases", async () => {
	const cases = [
		{
			name: "written -> refused => partial verdict",
			scopes: [
				{ scope: "global" as const, agents: ["claude"] },
				{
					scope: "project" as const,
					projectRoot: "/proj",
					agents: ["zed"],
				},
			],
			mockResponses: {
				global: () => ({
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
				}),
				project: () => {
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
				},
			},
			expectedSuccess: false,
			expectedVerdict: "partial",
			verify(result: any, calls: { global: number; project: number }) {
				assert.equal(
					calls.global,
					1,
					"global scope must have been called",
				);
				assert.equal(
					calls.project,
					1,
					"project scope must have been called",
				);
				assert.ok(
					!result.message?.includes("nothing was written"),
					"cross-scope failure must not imply nothing was written when global wrote",
				);
				assert.equal(
					result.message,
					"deleteSkillCrossScopePartial|scopeGlobal|scopeProject|Zed agent has no project skill config",
				);
			},
		},
		{
			name: "absent -> refused => refused verdict (not partial)",
			scopes: [
				{ scope: "global" as const, agents: ["claude"] },
				{
					scope: "project" as const,
					projectRoot: "/proj",
					agents: ["zed"],
				},
			],
			mockResponses: {
				global: () => ({
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
				}),
				project: () => {
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
				},
			},
			expectedSuccess: false,
			expectedVerdict: "refused",
			verify(result: any, calls: { global: number; project: number }) {
				assert.equal(calls.global, 1);
				assert.equal(calls.project, 1);
				assert.equal(
					result.message,
					"Zed agent has no project skill config",
					"message must not claim global was written",
				);
			},
		},
		{
			name: "refused-first => refused verdict (no second call)",
			scopes: [
				{ scope: "global" as const, agents: ["claude"] },
				{
					scope: "project" as const,
					projectRoot: "/proj",
					agents: ["claude"],
				},
			],
			mockResponses: {
				global: () => {
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
				project: () => {
					throw new Error(
						"project scope must not be called when first scope fails",
					);
				},
			},
			expectedSuccess: false,
			expectedVerdict: "refused",
			verify(result: any, calls: { global: number; project: number }) {
				assert.equal(calls.global, 1);
				assert.equal(
					calls.project,
					0,
					"second scope must not be called",
				);
				assert.equal(result.message, "cannot delete for claude");
			},
		},
	];

	for (const tc of cases) {
		const calls = { global: 0, project: 0 };
		const api = {
			skills: {
				reconcile: async (req: any) => {
					if (req.source.scope === "global") {
						calls.global++;
						return tc.mockResponses.global();
					}
					if (req.source.scope === "project") {
						calls.project++;
						return tc.mockResponses.project();
					}
					throw new Error(`unexpected scope: ${req.source.scope}`);
				},
			},
		} as any;

		const result = await deleteSkill({
			api,
			skillName: "cross-skill",
			scopes: tc.scopes,
			intent: { kind: "all-agents" },
			t,
		});

		assert.equal(
			result.success,
			tc.expectedSuccess,
			`${tc.name}: success mismatch`,
		);
		assert.equal(
			result.verdict,
			tc.expectedVerdict,
			`${tc.name}: verdict mismatch`,
		);
		tc.verify(result, calls);
	}
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

test("splitDeleteTargets: disabled agent missing from both managed and unmanaged lists is not named without consent (fail-closed)", () => {
	const items = [
		{ name: "my-skill", agent: "claude", source: "global" as const },
		{ name: "my-skill", agent: "cursor", source: "global" as const },
		{
			name: "my-skill",
			agent: "unknown-disabled",
			source: "global" as const,
		},
	];

	// Backend holders only knows about claude in managed.
	// Any agent not in managed is classified as unmanaged (fail-closed).
	const backend: BackendHolders = {
		managed: ["claude"],
	};

	// Unticked consent: unknown-disabled must NOT be classified as managed, so it must NOT be named!
	const unticked = splitDeleteTargets(items, backend, false);
	assert.deepEqual(
		unticked.managed.map((i) => i.agent),
		["claude"],
	);
	assert.deepEqual(
		unticked.unmanaged.map((i) => i.agent),
		["cursor", "unknown-disabled"],
	);
	assert.deepEqual(
		unticked.named.map((i) => i.agent),
		["claude"],
		"agent missing from both lists must not be named without consent",
	);

	// Ticked consent: unknown-disabled is now included in named
	const ticked = splitDeleteTargets(items, backend, true);
	assert.deepEqual(
		ticked.named.map((i) => i.agent),
		["claude", "cursor", "unknown-disabled"],
	);
});

test("getSkillHolders fetches holders and splits managed vs unmanaged from response", async () => {
	const api = {
		skills: {
			holders: async (name: string, scope: string) => {
				assert.equal(name, "my-skill");
				assert.equal(scope, "global");
				return {
					managed: ["claude"],
					unmanaged: ["cursor", "opencode"],
					all: ["claude", "cursor", "opencode"],
				};
			},
		},
	} as any;

	const holders = await getSkillHolders(api, "my-skill", "global");

	assert.deepEqual(holders.managed, ["claude"]);

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

test("mixed-scope group: disabled agent is not named in either scope's request", () => {
	const backendHolders: BackendHolders = {
		managed: ["claude"],
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

test("fetchSkillHoldersForGroup queries scopes from items and merges managed holders", async () => {
	const calls: Array<{
		name: string;
		scope: string;
		projectRoot?: string | null;
	}> = [];
	const api = {
		skills: {
			holders: async (
				name: string,
				scope: string,
				projectRoot?: string | null,
			) => {
				calls.push({ name, scope, projectRoot });
				if (scope === "global") {
					return {
						managed: ["claude"],
						unmanaged: ["cursor"],
						all: ["claude", "cursor"],
					};
				}
				return {
					managed: ["codex"],
					unmanaged: ["cursor"],
					all: ["codex", "cursor"],
				};
			},
		},
	} as any;

	const items = [
		{ name: "my-skill", agent: "claude", source: "global" },
		{ name: "my-skill", agent: "codex", source: "project" },
	];

	const result = await fetchSkillHoldersForGroup(
		api,
		"my-skill",
		items,
		"/project",
	);

	assert.deepEqual(result.managed?.slice().sort(), ["claude", "codex"]);
	assert.equal(calls.length, 2);
	assert.ok(calls.some((c) => c.scope === "global"));
	assert.ok(
		calls.some(
			(c) => c.scope === "project" && c.projectRoot === "/project",
		),
	);
});

test("fetchBulkHolders queries holders across groups and builds managed and byGroup", async () => {
	const api = {
		skills: {
			holders: async (name: string, _scope: string) => {
				if (name === "skill-1") {
					return {
						managed: ["claude"],
						unmanaged: ["cursor"],
						all: ["claude", "cursor"],
					};
				}
				return { managed: ["codex"], unmanaged: [], all: ["codex"] };
			},
		},
	} as any;

	const groups = [
		{
			key: "skill-1",
			items: [{ name: "skill-1", agent: "claude", source: "global" }],
			resourceType: "skill" as const,
		},
		{
			key: "skill-2",
			items: [{ name: "skill-2", agent: "codex", source: "global" }],
			resourceType: "skill" as const,
		},
		{
			key: "mcp-server",
			items: [{ name: "mcp-server", agent: "claude", source: "global" }],
			resourceType: "mcp" as const,
		},
	];

	const result = await fetchBulkHolders({
		api,
		groups,
		resourceType: "mixed",
	});

	assert.deepEqual(result.managed?.slice().sort(), ["claude", "codex"]);
	assert.deepEqual(result.byGroup?.["skill-1"]?.managed, ["claude"]);
	assert.deepEqual(result.byGroup?.["skill-2"]?.managed, ["codex"]);
	assert.equal(result.byGroup?.["mcp-server"], undefined);
});

test("fetchBulkHolders returns empty managed and byGroup when no skill groups present", async () => {
	const api = {
		skills: {
			holders: async () => {
				throw new Error("should not be called");
			},
		},
	} as any;

	const groups = [
		{
			key: "mcp-server",
			items: [{ name: "mcp-server", agent: "claude", source: "global" }],
			resourceType: "mcp" as const,
		},
	];

	const result = await fetchBulkHolders({
		api,
		groups,
		resourceType: "mixed",
	});

	assert.deepEqual(result.managed, []);
	assert.deepEqual(result.byGroup, {});
});
