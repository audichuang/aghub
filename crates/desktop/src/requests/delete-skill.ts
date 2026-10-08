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
			agents?: readonly string[];
			includeUnmanaged?: boolean;
	  };

export interface ScopeTarget {
	scope: "global" | "project";
	projectRoot?: string | null;
	agents?: readonly string[];
}

export interface BackendHolders {
	still_read_by_managed?: readonly string[];
	still_read_by_unmanaged?: readonly string[];
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
 * classification (`still_read_by_managed`), avoiding frontend heuristics.
 */
export function splitDeleteTargets<T extends DeleteTargetItem>(
	items: readonly T[],
	backendHolders: BackendHolders,
	includeUnmanaged: boolean,
): DeleteTargets<T> {
	const withAgent = items.filter((item) => !!item.agent);
	const managedSet = new Set(backendHolders.still_read_by_managed ?? []);
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
		const targets = isSkill
			? splitDeleteTargets(group.items, backendHolders, includeUnmanaged)
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
			const targets = splitDeleteTargets(
				group.items,
				backendHolders,
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

export function failedReconcileRowsMessage(
	results: readonly {
		agent: string;
		success: boolean;
		error?: string | null;
	}[],
	agentName: (id: string) => string,
): string | null {
	const failed = results.filter((row) => !row.success);
	if (failed.length === 0) return null;
	return failed
		.map((row) => `${agentName(row.agent)}: ${row.error ?? "failed"}`)
		.join("\n");
}

export function isSharedMasterRefusal(error: unknown): boolean {
	return (
		isHTTPError(error) &&
		error.response.status === 422 &&
		getApiErrorCode(error) === "UNSUPPORTED_OPERATION"
	);
}

export type KeptDeleteTranslate = (
	key: "deleteSkillKeptSharedMaster" | "deleteSkillKeptGit",
	options: { name: string; path?: string },
) => string;

export interface KeptDeleteAnswer {
	error?: string | null;
	skipped: readonly string[];
}

export function keptDeleteMessage(
	answer: KeptDeleteAnswer,
	name: string,
	t: KeptDeleteTranslate,
): string {
	const path = answer.skipped[0];
	if (answer.error && path) {
		return t("deleteSkillKeptGit", { name, path });
	}
	return t("deleteSkillKeptSharedMaster", { name });
}

export function keptDeleteMessageFromError(
	error: unknown,
	name: string,
	t: KeptDeleteTranslate,
): string | null {
	if (!isSharedMasterRefusal(error)) {
		return null;
	}
	const body = getApiErrorBody(error);
	const target =
		body?.rejected_targets?.find((t) => t.kind === "git") ??
		body?.rejected_targets?.[0];
	if (target?.kind === "git") {
		return t("deleteSkillKeptGit", { name, path: target.path });
	}
	return t("deleteSkillKeptSharedMaster", { name });
}

export type TranslateFn = (
	key: string,
	options?: Record<string, string | number | boolean | null | undefined>,
) => string;

/**
 * Evaluates whether an error represents a whole-batch preflight refusal
 * based on the machine code, rather than matching error text.
 */
export function isWholeBatchRefusal(error: unknown): boolean {
	const code = getApiErrorCode(error);
	return code === "UNSUPPORTED_OPERATION" || code === "INVALID_CONFIG";
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
	unmanagedAgents?: readonly string[];
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
			: `deleteSkillKeptGit|${skillName}|${gitTarget.path ?? ""}`;
		return {
			isRefusal: true,
			retryWithUnmanaged: false,
			code,
			rejectedTargets,
			message: msg,
		};
	}

	// 2. Check if refusal was genuinely caused by unticked disabled agents
	let retryWithUnmanaged = false;
	if (
		code === "UNSUPPORTED_OPERATION" &&
		!includeUnmanaged &&
		rejectedTargets.length > 0 &&
		(options?.unmanagedAgents?.length ?? 0) > 0
	) {
		const unmanagedSet = new Set(options?.unmanagedAgents ?? []);
		const allUnmanaged = rejectedTargets.every(
			(tgt) => tgt.kind !== "git" && unmanagedSet.has(tgt.agent),
		);
		if (allUnmanaged) {
			retryWithUnmanaged = true;
		}
	}

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
	isGit?: boolean;
	error?: unknown;
	t?: TranslateFn;
	retryWithUnmanaged?: boolean;
	projectError?: string;
}

export function formatDeleteMessage(
	verdict: DeleteSkillVerdict,
	options: FormatDeleteMessageOptions,
): string {
	const { name, path, isGit, t, retryWithUnmanaged, projectError } = options;
	const translate: TranslateFn =
		t ??
		((key, opts) => {
			const nameStr = typeof opts?.name === "string" ? opts.name : "";
			const pathStr = typeof opts?.path === "string" ? opts.path : "";
			const errStr = typeof opts?.error === "string" ? opts.error : "";
			return `${key}|${nameStr}|${pathStr}|${errStr}`;
		});

	if (retryWithUnmanaged) {
		return translate("deleteSkillRetryWithUnmanaged");
	}

	switch (verdict) {
		case "kept":
			if (isGit && path) {
				return translate("deleteSkillKeptGit", { name, path });
			}
			return translate("deleteSkillKeptSharedMaster", { name });
		case "partial":
			if (projectError) {
				return translate("deleteSkillCrossScopePartial", {
					error: projectError,
				});
			}
			return translate("deleteSkillPartial", { name });
		case "lock-only":
			return translate("sourceRemovedCleanLockOnly", { name });
		case "refused":
			if (options.error instanceof Error) return options.error.message;
			if (typeof options.error === "string") return options.error;
			return "Failed to delete skill";
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
	stillReadByManaged?: string[];
	stillReadByUnmanaged?: string[];
	retryWithUnmanaged?: boolean;
	refusalCode?: string;
	rejectedTargets?: RejectedTarget[];
	scopeResults?: ScopeDeleteResult[];
}

export interface DeleteSkillOptions {
	api: ApiClient;
	queryClient?: QueryClient;
	skillName?: string;
	name?: string;
	agent?: string;
	scope?: "global" | "project";
	projectRoot?: string | null;
	sourcePath?: string | null;
	scopes?: readonly ScopeTarget[];
	intent: DeleteSkillIntent;
	unmanagedAgents?: readonly string[];
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
	const { api, queryClient, intent, t } = options;
	const name = options.skillName ?? options.name ?? "";

	const invalidate = async () => {
		if (queryClient) {
			await invalidateSkillQueries(queryClient);
		}
	};

	// 1. By-path deletion
	if (intent.kind === "by-path") {
		const sourcePath = options.sourcePath ?? intent.sourcePath;
		const scope = options.scope ?? "global";
		const projectRoot =
			scope === "project" ? (options.projectRoot ?? null) : null;
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
				const isGit = Boolean(res.error && res.skipped.length > 0);
				const msg = formatDeleteMessage("kept", {
					name,
					path: res.skipped[0],
					isGit,
					t,
				});
				return {
					verdict: "kept",
					success: false,
					message: msg,
					unmanagedKept,
					stillReadByManaged: res.still_read_by_managed,
					stillReadByUnmanaged: res.still_read_by_unmanaged,
				};
			}

			if (verdict === "partial") {
				const msg = formatDeleteMessage("partial", { name, t });
				return {
					verdict: "partial",
					success: false,
					message: msg,
					unmanagedKept,
					stillReadByManaged: res.still_read_by_managed,
					stillReadByUnmanaged: res.still_read_by_unmanaged,
				};
			}

			return {
				verdict,
				success: verdict === "removed" || verdict === "absent",
				unmanagedKept,
				stillReadByManaged: res.still_read_by_managed,
				stillReadByUnmanaged: res.still_read_by_unmanaged,
			};
		} catch (error) {
			await invalidate();

			const refusal = interpretRefusal(error, {
				intent,
				unmanagedAgents: options.unmanagedAgents,
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
				refusalCode: refusal.code,
				rejectedTargets: refusal.rejectedTargets,
			};
		}
	}

	// 2. Cross-scope or multi-scope deletion
	if (options.scopes && options.scopes.length > 0) {
		const scopeResults: ScopeDeleteResult[] = [];
		let anyWritten = false;
		const allStillReadByManaged: string[] = [];
		const allStillReadByUnmanaged: string[] = [];

		for (let i = 0; i < options.scopes.length; i++) {
			const target = options.scopes[i];
			const isProject = target.scope === "project";
			const projectRoot = isProject ? (target.projectRoot ?? null) : null;
			const agents = target.agents ? [...target.agents] : [];
			const agent = agents[0] ?? options.agent ?? "claude";

			try {
				let resOutcome: RemovalOutcomeKind | undefined;
				let managedHolders: string[] = [];
				let unmanagedHolders: string[] = [];

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

				const removeRow = reconcileRes.results.find(
					(r) => r.action === "delete",
				);
				resOutcome =
					removeRow?.outcome ??
					(reconcileRes.failed_count === 0 ? "removed" : "partial");
				managedHolders = removeRow?.still_read_by_managed ?? [];
				unmanagedHolders = removeRow?.still_read_by_unmanaged ?? [];

				allStillReadByManaged.push(...managedHolders);
				allStillReadByUnmanaged.push(...unmanagedHolders);

				const verdict = interpretRemovalVerdict(
					resOutcome,
					intent.kind,
				);
				const scopeSuccess =
					verdict === "removed" || verdict === "absent";

				if (scopeSuccess) {
					anyWritten = true;
				}

				scopeResults.push({
					scope: target.scope,
					verdict,
					success: scopeSuccess,
				});
			} catch (error) {
				const refusal = interpretRefusal(error, {
					intent,
					unmanagedAgents: options.unmanagedAgents,
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

		const failedScope = scopeResults.find((s) => !s.success);
		if (failedScope) {
			const projectError = failedScope.error ?? "failed";
			const msg = anyWritten
				? formatDeleteMessage("partial", {
						name,
						projectError,
						t,
					})
				: projectError;

			return {
				verdict: anyWritten ? "partial" : "refused",
				success: false,
				message: msg,
				scopeResults,
				unmanagedKept: allStillReadByUnmanaged.length > 0,
				stillReadByManaged: allStillReadByManaged,
				stillReadByUnmanaged: allStillReadByUnmanaged,
			};
		}

		return {
			verdict: "removed",
			success: true,
			scopeResults,
			unmanagedKept: allStillReadByUnmanaged.length > 0,
			stillReadByManaged: allStillReadByManaged,
			stillReadByUnmanaged: allStillReadByUnmanaged,
		};
	}

	// 3. Single-scope deletion (from-agents, all-agents, or clean-lock)
	const scope = options.scope ?? "global";
	const projectRoot =
		scope === "project" ? (options.projectRoot ?? undefined) : undefined;
	const agent = options.agent ?? "claude";
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
			const msg = formatDeleteMessage("lock-only", { name, t });
			return {
				verdict: "lock-only",
				success: false,
				message: msg,
				unmanagedKept,
				stillReadByManaged: res.still_read_by_managed,
				stillReadByUnmanaged: res.still_read_by_unmanaged,
			};
		}

		if (verdict === "kept") {
			const isGit = Boolean(res.error && res.skipped.length > 0);
			const msg = formatDeleteMessage("kept", {
				name,
				path: res.skipped[0],
				isGit,
				t,
			});
			return {
				verdict: "kept",
				success: false,
				message: msg,
				unmanagedKept,
				stillReadByManaged: res.still_read_by_managed,
				stillReadByUnmanaged: res.still_read_by_unmanaged,
			};
		}

		if (verdict === "partial") {
			const msg = formatDeleteMessage("partial", { name, t });
			return {
				verdict: "partial",
				success: false,
				message: msg,
				unmanagedKept,
				stillReadByManaged: res.still_read_by_managed,
				stillReadByUnmanaged: res.still_read_by_unmanaged,
			};
		}

		return {
			verdict,
			success: verdict === "removed" || verdict === "absent",
			unmanagedKept,
			stillReadByManaged: res.still_read_by_managed,
			stillReadByUnmanaged: res.still_read_by_unmanaged,
		};
	} catch (error) {
		await invalidate();

		const refusal = interpretRefusal(error, {
			intent,
			unmanagedAgents: options.unmanagedAgents,
			skillName: name,
			t,
		});

		return {
			verdict: "refused",
			success: false,
			message: refusal.message,
			error: error instanceof Error ? error : new Error(String(error)),
			retryWithUnmanaged: refusal.retryWithUnmanaged,
			refusalCode: refusal.code,
			rejectedTargets: refusal.rejectedTargets,
		};
	}
}
