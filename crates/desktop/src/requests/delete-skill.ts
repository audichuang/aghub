import type { QueryClient } from "@tanstack/react-query";
import { isHTTPError } from "ky";
import type {
	DeleteSkillByPathResponse,
	OperationBatchResponse,
	RemovalOutcomeKind,
} from "../generated/dto";
import {
	getApiErrorBody,
	getApiErrorCode,
	type RejectedTarget,
} from "../lib/api.ts";
import type { ApiClient } from "./client.ts";
import { invalidateSkillQueries } from "./skills.ts";

export type DeleteSkillVerdict =
	| "removed"
	| "absent"
	| "kept"
	| "partial"
	| "lock-only"
	| "refused";

export type DeleteSkillIntent =
	| {
			kind: "from-agents";
			agents: readonly string[];
			includeUnmanaged?: boolean;
	  }
	| { kind: "all-agents"; includeUnmanaged?: boolean }
	| { kind: "clean-lock" }
	| {
			kind: "by-path";
			sourcePath: string;
			scope?: "global" | "project";
			projectRoot?: string | null;
			agents?: readonly string[];
			includeUnmanaged?: boolean;
	  };

export interface ScopeTarget {
	scope: "global" | "project";
	projectRoot?: string | null;
	agents?: readonly string[];
}

export interface BackendHolders {
	managed?: readonly string[];
	byGroup?: Record<string, { managed?: readonly string[] }>;
}

export interface DeleteTargetItem {
	agent?: string | null;
	source?: string | null;
	source_path?: string | null;
}

export interface DeleteTargets<T extends DeleteTargetItem> {
	managed: T[];
	unmanaged: T[];
	named: T[];
}

/**
 * Splits target items into managed and unmanaged groups using the backend's
 * classification, avoiding frontend heuristics.
 */
export function splitDeleteTargets<T extends DeleteTargetItem>(
	items: readonly T[],
	backendHolders: BackendHolders,
	includeUnmanaged: boolean,
): DeleteTargets<T> {
	const withAgent = items.filter((item) => !!item.agent);
	const managedSet = new Set(backendHolders.managed ?? []);

	const managed = withAgent.filter((item) =>
		managedSet.has(item.agent as string),
	);
	const unmanaged = withAgent.filter(
		(item) => !managedSet.has(item.agent as string),
	);
	return {
		managed,
		unmanaged,
		named: includeUnmanaged ? [...managed, ...unmanaged] : managed,
	};
}

export interface BulkDeleteItem extends DeleteTargetItem {
	name: string;
	source_path?: string | null;
}

export interface BulkDeleteGroup {
	key: string;
	items: BulkDeleteItem[];
	resourceType?: "mcp" | "skill";
}

export interface BulkDeleteRequest {
	resourceType: "mcp" | "skill";
	name: string;
	groupKey: string;
	agent: string;
	scope: "global" | "project";
	projectRoot?: string;
	agents: string[];
	sourcePath?: string | null;
}

export interface BuildBulkDeleteRequestsOptions {
	groups: readonly BulkDeleteGroup[];
	resourceType: "mcp" | "skill" | "mixed";
	backendHolders: BackendHolders;
	includeUnmanaged: boolean;
	projectPath?: string;
}

export interface BuildBulkDeleteRequestsResult {
	requests: BulkDeleteRequest[];
	skippedGroupKeys: string[];
}

export function buildBulkDeleteRequests({
	groups,
	resourceType,
	backendHolders,
	includeUnmanaged,
	projectPath,
}: BuildBulkDeleteRequestsOptions): BuildBulkDeleteRequestsResult {
	const requests: BulkDeleteRequest[] = [];
	const skippedGroupKeys: string[] = [];
	const seen = new Set<string>();

	const scopeOf = (item: DeleteTargetItem): "global" | "project" =>
		item.source === "project" ? "project" : "global";

	for (const group of groups) {
		const groupResourceType = group.resourceType ?? resourceType;
		const isSkill = groupResourceType === "skill";
		const groupHolders: BackendHolders =
			backendHolders.byGroup && backendHolders.byGroup[group.key]
				? {
						managed: backendHolders.byGroup[group.key].managed,
					}
				: backendHolders;
		const targets = isSkill
			? splitDeleteTargets(group.items, groupHolders, includeUnmanaged)
			: null;
		const candidateItems = targets ? targets.named : group.items;

		const validCandidateItems = candidateItems.filter(
			(item) => !!item.agent,
		);
		if (validCandidateItems.length === 0) {
			skippedGroupKeys.push(group.key);
			continue;
		}

		for (const item of validCandidateItems) {
			const agent = item.agent as string;
			const scope = scopeOf(item);
			const projectRoot = scope === "project" ? projectPath : undefined;

			const scopeAgents = candidateItems
				.filter((other) => scopeOf(other) === scope)
				.flatMap((other) => (other.agent ? [other.agent] : []));

			const dedupKey =
				groupResourceType === "skill" && item.source_path
					? `skill:${item.source_path}:${scope}`
					: groupResourceType === "skill"
						? `skill:${agent}:${group.key}:${scope}`
						: `${groupResourceType}:${agent}:${item.name}:${scope}`;

			if (seen.has(dedupKey)) continue;
			seen.add(dedupKey);

			requests.push({
				resourceType: groupResourceType === "mcp" ? "mcp" : "skill",
				name: item.name,
				groupKey: group.key,
				agent,
				scope,
				projectRoot,
				agents: [...new Set(scopeAgents)],
				sourcePath: item.source_path ?? null,
			});
		}
	}

	return { requests, skippedGroupKeys };
}

export function collectUnmanagedDeleteTargets(
	groups: readonly BulkDeleteGroup[],
	backendHolders: BackendHolders,
	resourceType: "mcp" | "skill" | "mixed",
): BulkDeleteItem[] {
	const unmanaged: BulkDeleteItem[] = [];
	for (const group of groups) {
		const groupResourceType = group.resourceType ?? resourceType;
		if (groupResourceType === "skill") {
			const groupHolders: BackendHolders =
				backendHolders.byGroup && backendHolders.byGroup[group.key]
					? {
							managed: backendHolders.byGroup[group.key].managed,
						}
					: backendHolders;
			const targets = splitDeleteTargets(
				group.items,
				groupHolders,
				false,
			);
			unmanaged.push(...targets.unmanaged);
		}
	}
	return unmanaged;
}

export function isGoneSkillPath(error: unknown): boolean {
	return isHTTPError(error) && error.response.status === 404;
}

/**
 * Retrieves skill holders from the backend.
 */
export async function getSkillHolders(
	api: ApiClient,
	name: string,
	scope: "global" | "project",
	projectRoot?: string | null,
): Promise<BackendHolders> {
	const res = await api.skills.holders(name, scope, projectRoot);
	return {
		managed: res.managed,
	};
}

/**
 * Fetches and merges backend holders across all scopes present in a skill group's items.
 */
export async function fetchSkillHoldersForGroup(
	api: ApiClient,
	skillName: string,
	items: readonly DeleteTargetItem[],
	projectPath?: string,
): Promise<BackendHolders> {
	const scopes = new Set<"global" | "project">();
	for (const item of items) {
		scopes.add(item.source === "project" ? "project" : "global");
	}
	if (scopes.size === 0) scopes.add("global");

	const results = await Promise.all(
		Array.from(scopes).map((scope) =>
			getSkillHolders(
				api,
				skillName,
				scope,
				scope === "project" ? projectPath : undefined,
			),
		),
	);

	const allManaged = new Set<string>();
	for (const r of results) {
		for (const m of r.managed ?? []) allManaged.add(m);
	}

	return {
		managed: Array.from(allManaged),
	};
}

export interface FetchBulkHoldersOptions {
	api: ApiClient;
	groups: readonly BulkDeleteGroup[];
	resourceType?: "mcp" | "skill" | "mixed";
	projectPath?: string;
}

/**
 * Fetches and aggregates holders across all skill groups and their respective scopes.
 */
export async function fetchBulkHolders({
	api,
	groups,
	resourceType = "mixed",
	projectPath,
}: FetchBulkHoldersOptions): Promise<BackendHolders> {
	const skillGroups = groups.filter(
		(g) => (g.resourceType ?? resourceType) === "skill",
	);

	if (skillGroups.length === 0) {
		return { managed: [], byGroup: {} };
	}

	const groupResults = await Promise.all(
		skillGroups.map(async (group) => {
			const skillName = group.items[0]?.name ?? group.key;
			const holders = await fetchSkillHoldersForGroup(
				api,
				skillName,
				group.items,
				projectPath,
			);
			return { groupKey: group.key, holders };
		}),
	);

	const byGroup: Record<string, { managed: string[] }> = {};
	const allManaged = new Set<string>();

	for (const { groupKey, holders } of groupResults) {
		if (!byGroup[groupKey]) {
			byGroup[groupKey] = { managed: [] };
		}
		for (const m of holders.managed ?? []) {
			if (!byGroup[groupKey].managed.includes(m)) {
				byGroup[groupKey].managed.push(m);
			}
			allManaged.add(m);
		}
	}

	return {
		managed: Array.from(allManaged),
		byGroup,
	};
}

export type TranslateFn = (
	key: string,
	options?: Record<string, string | number | boolean | null | undefined>,
) => string;

/**
 * Evaluates whether an error represents a whole-batch preflight refusal
 * based on the machine code and presence of rejected targets, rather than matching error text.
 */
export function isWholeBatchRefusal(error: unknown): boolean {
	const code = getApiErrorCode(error);
	const body = getApiErrorBody(error);
	const rejectedTargets = body?.rejected_targets ?? [];
	return code === "UNSUPPORTED_OPERATION" && rejectedTargets.length > 0;
}

/**
 * Interprets a backend outcome enum into one of the 5 canonical Verdict states.
 * For the Sources page `clean-lock` intent, `absent` maps to `lock-only`.
 */
export function interpretRemovalVerdict(
	outcome: RemovalOutcomeKind | undefined | null,
	intentKind: DeleteSkillIntent["kind"],
): DeleteSkillVerdict {
	if (intentKind === "clean-lock") {
		if (outcome === "removed") return "removed";
		if (outcome === "absent") return "lock-only";
		if (outcome === "partial") return "partial";
		if (outcome === "kept") return "kept";
		return "refused";
	}
	if (outcome === "removed") return "removed";
	if (outcome === "absent") return "absent";
	if (outcome === "kept") return "kept";
	if (outcome === "partial") return "partial";
	return "refused";
}

export interface RefusalInterpretation {
	isRefusal: boolean;
	retryWithUnmanaged: boolean;
	code?: string;
	rejectedTargets?: RejectedTarget[];
	message?: string;
}

export interface InterpretRefusalOptions {
	intent?: DeleteSkillIntent;
	skillName?: string;
	t?: TranslateFn;
}

export function interpretRefusal(
	error: unknown,
	options?: InterpretRefusalOptions,
): RefusalInterpretation {
	const code = getApiErrorCode(error);
	const body = getApiErrorBody(error);
	const isRefusal = isWholeBatchRefusal(error);

	if (!isRefusal) {
		return {
			isRefusal: false,
			retryWithUnmanaged: false,
			message: error instanceof Error ? error.message : String(error),
		};
	}

	const rejectedTargets = body?.rejected_targets ?? [];
	const intent = options?.intent;
	const includeUnmanaged =
		intent && "includeUnmanaged" in intent
			? Boolean(intent.includeUnmanaged)
			: false;
	const t = options?.t;
	const skillName = options?.skillName ?? "";

	// 1. Check git refusal
	const gitTarget = rejectedTargets.find((tgt) => tgt.kind === "git");
	if (gitTarget) {
		const msg = t
			? t("deleteSkillKeptGit", { name: skillName, path: gitTarget.path })
			: gitTarget.reason || "Git tracked refusal";
		return {
			isRefusal: true,
			retryWithUnmanaged: false,
			code,
			rejectedTargets,
			message: msg,
		};
	}

	// 2. Check if refusal was caused by unticked unmanaged (disabled) agents
	const allShared =
		rejectedTargets.length > 0 &&
		rejectedTargets.every((tgt) => tgt.kind === "shared");

	const everyReaderUnmanaged =
		allShared &&
		rejectedTargets.every(
			(tgt) =>
				tgt.readers &&
				tgt.readers.length > 0 &&
				tgt.readers.every((r) => r.managed === false),
		);

	const retryWithUnmanaged =
		code === "UNSUPPORTED_OPERATION" &&
		!includeUnmanaged &&
		!gitTarget &&
		everyReaderUnmanaged;

	let message: string | undefined;
	if (retryWithUnmanaged) {
		message = t
			? t("deleteSkillRetryWithUnmanaged")
			: "deleteSkillRetryWithUnmanaged";
	} else if (rejectedTargets.length > 0) {
		message = rejectedTargets.map((tgt) => tgt.reason).join("; ");
	} else if (body?.error) {
		message = body.error;
	} else if (error instanceof Error) {
		message = error.message;
	}

	return {
		isRefusal: true,
		retryWithUnmanaged,
		code,
		rejectedTargets,
		message,
	};
}

export interface FormatDeleteMessageOptions {
	name: string;
	path?: string;
	error?: unknown;
	t: TranslateFn;
	retryWithUnmanaged?: boolean;
	context?: "single" | "bulk";
}

export function formatDeleteMessage(
	verdict: DeleteSkillVerdict,
	options: FormatDeleteMessageOptions,
): string {
	const { name, t, retryWithUnmanaged, context } = options;

	if (retryWithUnmanaged) {
		return t("deleteSkillRetryWithUnmanaged");
	}

	if (context === "bulk") {
		if (verdict === "kept") return t("bulkDeleteKept");
		if (verdict === "partial") return t("bulkDeletePartial");
	}

	switch (verdict) {
		case "kept":
			return t("deleteSkillKeptSharedMaster", { name });
		case "partial":
			return t("deleteSkillPartial", { name });
		case "lock-only":
			return t("sourceRemovedCleanLockOnly", { name });
		case "refused":
			if (options.error instanceof Error) return options.error.message;
			if (typeof options.error === "string") return options.error;
			return t("failedToDeleteSkill");
		case "removed":
		case "absent":
			return "";
	}
}

export interface ScopeDeleteResult {
	scope: "global" | "project";
	verdict: DeleteSkillVerdict;
	success: boolean;
	error?: string;
}

export interface DeleteSkillResult {
	verdict: DeleteSkillVerdict;
	success: boolean;
	message?: string;
	error?: Error | null;
	unmanagedKept?: boolean;
	stillReadByUnmanaged?: string[];
	retryWithUnmanaged?: boolean;
}

export interface DeleteSkillOptions {
	api: ApiClient;
	queryClient?: QueryClient;
	skillName: string;
	agent?: string;
	scope?: "global" | "project";
	projectRoot?: string | null;
	scopes?: readonly ScopeTarget[];
	intent: DeleteSkillIntent;
	context?: "single" | "bulk";
	t?: TranslateFn;
}

/**
 * Unified request layer entry point for deleting skills.
 * Handles single-agent, all-agents, by-path, cross-scope, and clean-lock operations,
 * interprets backend Verdicts, and centralizes non-blocking cache invalidation.
 */
export async function deleteSkill(
	options: DeleteSkillOptions,
): Promise<DeleteSkillResult> {
	const { api, queryClient, intent, t, skillName } = options;
	const name = skillName;

	const invalidate = async () => {
		if (queryClient) {
			await invalidateSkillQueries(queryClient);
		}
	};

	// 1. By-path deletion
	if (intent.kind === "by-path") {
		const sourcePath = intent.sourcePath;
		if (!sourcePath) {
			throw new Error("sourcePath is required for by-path deletion");
		}
		const scope = intent.scope ?? "global";
		const projectRoot =
			scope === "project" ? (intent.projectRoot ?? null) : null;
		const agents = intent.agents ? [...intent.agents] : [];

		try {
			const res: DeleteSkillByPathResponse =
				await api.skills.deleteByPath({
					source_path: sourcePath,
					confirm: true,
					agents,
					scope,
					project_root: projectRoot,
				});

			const verdict = interpretRemovalVerdict(res.outcome, intent.kind);
			const unmanagedKept =
				(res.still_read_by_unmanaged?.length ?? 0) > 0;

			await invalidate();

			if (verdict === "kept") {
				const msg = t
					? formatDeleteMessage("kept", {
							name,
							path: res.skipped[0],
							t,
							context: options.context,
						})
					: undefined;
				return {
					verdict: "kept",
					success: false,
					message: msg,
					unmanagedKept,
					stillReadByUnmanaged: res.still_read_by_unmanaged,
				};
			}

			if (verdict === "partial") {
				const msg = t
					? formatDeleteMessage("partial", {
							name,
							t,
							context: options.context,
						})
					: undefined;
				return {
					verdict: "partial",
					success: false,
					message: msg,
					unmanagedKept,
					stillReadByUnmanaged: res.still_read_by_unmanaged,
				};
			}

			return {
				verdict,
				success: verdict === "removed" || verdict === "absent",
				unmanagedKept,
				stillReadByUnmanaged: res.still_read_by_unmanaged,
			};
		} catch (error) {
			await invalidate();

			const refusal = interpretRefusal(error, {
				intent,
				skillName: name,
				t,
			});

			return {
				verdict: "refused",
				success: false,
				message: refusal.message,
				error:
					error instanceof Error ? error : new Error(String(error)),
				retryWithUnmanaged: refusal.retryWithUnmanaged,
			};
		}
	}

	// 2. Cross-scope or multi-scope deletion
	if (options.scopes && options.scopes.length > 0) {
		const scopeResults: ScopeDeleteResult[] = [];
		const allStillReadByUnmanaged: string[] = [];

		for (let i = 0; i < options.scopes.length; i++) {
			const target = options.scopes[i];
			const isProject = target.scope === "project";
			const projectRoot = isProject ? (target.projectRoot ?? null) : null;
			const agents = target.agents ? [...target.agents] : [];
			const agent = agents[0] ?? options.agent;
			if (!agent) {
				throw new Error("agent is required for skill deletion");
			}

			try {
				const reconcileRes: OperationBatchResponse =
					await api.skills.reconcile({
						source: {
							agent,
							scope: target.scope,
							project_root: projectRoot,
							name,
						},
						added: null,
						removed: agents,
						confirm: true,
					});

				const deleteRows = reconcileRes.results.filter(
					(r) => r.action === "delete",
				);
				for (const r of deleteRows) {
					if (r.still_read_by_unmanaged) {
						allStillReadByUnmanaged.push(
							...r.still_read_by_unmanaged,
						);
					}
				}

				const rowVerdicts = deleteRows.map((r) =>
					interpretRemovalVerdict(r.outcome, intent.kind),
				);

				const allAbsent =
					rowVerdicts.length > 0 &&
					rowVerdicts.every((v) => v === "absent");
				const allRemovedOrAbsent =
					rowVerdicts.length > 0 &&
					rowVerdicts.every((v) => v === "removed" || v === "absent");
				const anyRemoved = rowVerdicts.some((v) => v === "removed");
				const hasKept = rowVerdicts.some((v) => v === "kept");
				const hasPartial = rowVerdicts.some((v) => v === "partial");
				const anyFailed =
					reconcileRes.failed_count > 0 ||
					deleteRows.some(
						(r) => !r.success || r.outcome === "failed",
					);

				let scopeVerdict: DeleteSkillVerdict;
				let scopeSuccess = false;
				let scopeError: string | undefined;

				if (allAbsent && reconcileRes.failed_count === 0) {
					scopeVerdict = "absent";
					scopeSuccess = true;
				} else if (
					allRemovedOrAbsent &&
					anyRemoved &&
					reconcileRes.failed_count === 0
				) {
					scopeVerdict = "removed";
					scopeSuccess = true;
				} else if (
					hasPartial ||
					(anyRemoved && (hasKept || anyFailed))
				) {
					scopeVerdict = "partial";
					scopeSuccess = false;
					const failedReasons = deleteRows
						.filter(
							(r) =>
								!r.success ||
								r.outcome === "partial" ||
								r.outcome === "kept" ||
								r.outcome === "failed",
						)
						.map((r) =>
							r.error
								? `${r.agent}: ${r.error}`
								: `${r.agent}: ${r.outcome}`,
						)
						.join("; ");
					scopeError =
						failedReasons ||
						(t ? t("deleteSkillPartial", { name }) : "partial");
				} else if (hasKept) {
					scopeVerdict = "kept";
					scopeSuccess = false;
					scopeError = t
						? t("deleteSkillKeptSharedMaster", { name })
						: "kept";
				} else {
					scopeVerdict = "refused";
					scopeSuccess = false;
					const fallbackError = t
						? t("failedToDeleteSkill")
						: "Failed to delete skill";
					scopeError =
						deleteRows
							.map((r) => r.error)
							.filter(Boolean)
							.join("; ") || fallbackError;
				}

				scopeResults.push({
					scope: target.scope,
					verdict: scopeVerdict,
					success: scopeSuccess,
					error: scopeError,
				});

				if (!scopeSuccess) {
					break;
				}
			} catch (error) {
				const refusal = interpretRefusal(error, {
					intent,
					skillName: name,
					t,
				});

				scopeResults.push({
					scope: target.scope,
					verdict: "refused",
					success: false,
					error: refusal.message,
				});

				break;
			}
		}

		await invalidate();

		const writtenScopes = scopeResults.filter(
			(s) => s.verdict === "removed" || s.verdict === "partial",
		);
		const anyWritten = writtenScopes.length > 0;
		const failedScope = scopeResults.find((s) => !s.success);

		if (failedScope) {
			const fallbackError = t
				? t("failedToDeleteSkill")
				: "Failed to delete skill";
			const projectError = failedScope.error ?? fallbackError;
			const formatScopeLabel = (scope: "global" | "project"): string => {
				if (t) {
					return scope === "project"
						? t("scopeProject")
						: t("scopeGlobal");
				}
				return scope === "project" ? "Project" : "Global";
			};
			const writtenScopeName = writtenScopes
				.map((s) => formatScopeLabel(s.scope))
				.join(", ");
			const failedScopeName = formatScopeLabel(failedScope.scope);
			const msg = anyWritten
				? t
					? t("deleteSkillCrossScopePartial", {
							writtenScope: writtenScopeName,
							failedScope: failedScopeName,
							reason: projectError,
						})
					: `${writtenScopeName} scope deleted, but ${failedScopeName} scope failed: ${projectError}`
				: projectError;

			return {
				verdict: anyWritten ? "partial" : "refused",
				success: false,
				message: msg,
				unmanagedKept: allStillReadByUnmanaged.length > 0,
				stillReadByUnmanaged: allStillReadByUnmanaged,
			};
		}

		const allScopesAbsent =
			scopeResults.length > 0 &&
			scopeResults.every((s) => s.verdict === "absent");

		return {
			verdict: allScopesAbsent ? "absent" : "removed",
			success: true,
			unmanagedKept: allStillReadByUnmanaged.length > 0,
			stillReadByUnmanaged: allStillReadByUnmanaged,
		};
	}

	// 3. Single-scope deletion (from-agents, all-agents, or clean-lock)
	const scope = options.scope ?? "global";
	const projectRoot =
		scope === "project" ? (options.projectRoot ?? undefined) : undefined;
	const agent = options.agent;
	if (!agent) {
		throw new Error("agent is required for by-name skill deletion");
	}
	const allAgents =
		intent.kind === "all-agents" || intent.kind === "clean-lock";
	const agents = intent.kind === "from-agents" ? [...intent.agents] : [];

	try {
		const res: DeleteSkillByPathResponse = await api.skills.delete(
			agent,
			name,
			scope,
			projectRoot,
			allAgents,
			agents,
		);

		const verdict = interpretRemovalVerdict(res.outcome, intent.kind);
		const unmanagedKept = (res.still_read_by_unmanaged?.length ?? 0) > 0;

		await invalidate();

		if (verdict === "lock-only") {
			const msg = t
				? formatDeleteMessage("lock-only", { name, t })
				: undefined;
			return {
				verdict: "lock-only",
				success: false,
				message: msg,
				unmanagedKept,
				stillReadByUnmanaged: res.still_read_by_unmanaged,
			};
		}

		if (verdict === "kept") {
			const msg = t
				? formatDeleteMessage("kept", {
						name,
						path: res.skipped[0],
						t,
						context: options.context,
					})
				: undefined;
			return {
				verdict: "kept",
				success: false,
				message: msg,
				unmanagedKept,
				stillReadByUnmanaged: res.still_read_by_unmanaged,
			};
		}

		if (verdict === "partial") {
			const msg = t
				? formatDeleteMessage("partial", {
						name,
						t,
						context: options.context,
					})
				: undefined;
			return {
				verdict: "partial",
				success: false,
				message: msg,
				unmanagedKept,
				stillReadByUnmanaged: res.still_read_by_unmanaged,
			};
		}

		return {
			verdict,
			success: verdict === "removed" || verdict === "absent",
			unmanagedKept,
			stillReadByUnmanaged: res.still_read_by_unmanaged,
		};
	} catch (error) {
		await invalidate();

		const refusal = interpretRefusal(error, {
			intent,
			skillName: name,
			t,
		});

		return {
			verdict: "refused",
			success: false,
			message: refusal.message,
			error: error instanceof Error ? error : new Error(String(error)),
			retryWithUnmanaged: refusal.retryWithUnmanaged,
		};
	}
}
