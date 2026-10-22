import type { QueryClient } from "@tanstack/react-query";
import { isHTTPError, isTimeoutError } from "ky";
import type {
	DeleteSkillByNameResponse,
	DeleteSkillByPathResponse,
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
	name: string;
	agent: string;
	scope: "global" | "project";
	projectRoot?: string;
	agents: string[];
}

/** One by-name delete intent per skill, covering every scope it is named in. */
export interface BulkSkillDelete {
	name: string;
	scopes: ScopeTarget[];
	agents: string[];
}

// Anything not marked project is global: one rule for every delete path.
const scopeOf = (item: DeleteTargetItem): "global" | "project" =>
	item.source === "project" ? "project" : "global";

/**
 * A skill's named items as by-name scope targets, global first then project:
 * core stops at the first scope that does not finish, so the order decides
 * which scopes may already be written. One spelling for every delete dialog.
 */
export function scopeTargetsFor(
	items: readonly DeleteTargetItem[],
	projectPath?: string | null,
): ScopeTarget[] {
	const scopes: ScopeTarget[] = [];
	for (const scope of ["global", "project"] as const) {
		const agents = [
			...new Set(
				items.flatMap((i) =>
					i.agent && scopeOf(i) === scope ? [i.agent] : [],
				),
			),
		];
		if (agents.length > 0)
			scopes.push({
				scope,
				projectRoot: scope === "project" ? (projectPath ?? null) : null,
				agents,
			});
	}
	return scopes;
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
	skillDeletes: BulkSkillDelete[];
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
	const skillDeletes: BulkSkillDelete[] = [];
	const skippedGroupKeys: string[] = [];
	const seen = new Set<string>();

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

		if (isSkill) {
			const scopes = scopeTargetsFor(validCandidateItems, projectPath);
			skillDeletes.push({
				name: group.key,
				scopes,
				agents: [...new Set(scopes.flatMap((s) => s.agents ?? []))],
			});
			continue;
		}

		for (const item of validCandidateItems) {
			const agent = item.agent as string;
			const scope = scopeOf(item);
			const projectRoot = scope === "project" ? projectPath : undefined;

			const scopeAgents = candidateItems
				.filter((other) => scopeOf(other) === scope)
				.flatMap((other) => (other.agent ? [other.agent] : []));

			const dedupKey = `${agent}:${item.name}:${scope}`;

			if (seen.has(dedupKey)) continue;
			seen.add(dedupKey);

			requests.push({
				name: item.name,
				agent,
				scope,
				projectRoot,
				agents: [...new Set(scopeAgents)],
			});
		}
	}

	return { requests, skillDeletes, skippedGroupKeys };
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
		scopes.add(scopeOf(item));
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
	if (outcome === "absent")
		return intentKind === "clean-lock" ? "lock-only" : "absent";
	if (outcome === "removed") return "removed";
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
 * A timed-out delete proves nothing about what the server did. This proves
 * only that no targeted reader still serves the skill and no Master is left,
 * and it fails closed: any doubt (non-404 error, unreadable request) returns
 * false and the caller keeps its normal error path.
 *
 * Explicit agents: each one's own read must 404 (the all-agents list fails
 * open — it silently drops agents whose config failed to load). No agent
 * list: the list AND the withheld Masters (which no agent reads, so the list
 * never shows them) must both lack the name.
 */
async function goneAfterTimeout(
	error: unknown,
	api: ApiClient,
	name: string,
	targets: readonly ScopeTarget[],
): Promise<boolean> {
	if (!isTimeoutError(error)) return false;
	try {
		for (const { scope, projectRoot, agents } of targets) {
			const root =
				scope === "project" ? (projectRoot ?? undefined) : undefined;
			if (agents?.length) {
				for (const agent of agents) {
					try {
						await api.skills.get(agent, name, scope, root);
						return false;
					} catch (e) {
						if (!isGoneSkillPath(e)) return false;
					}
				}
			} else {
				const [list, withheld] = await Promise.all([
					api.skills.listAll(scope, root),
					api.skills.withheld(scope, root),
				]);
				if ([...list, ...withheld].some((s) => s.name === name))
					return false;
			}
		}
		return true;
	} catch {
		return false;
	}
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

			if (
				await goneAfterTimeout(error, api, name, [
					{ scope, projectRoot, agents },
				])
			)
				return { verdict: "removed", success: true };

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

	// 2. Cross-scope or multi-scope deletion: one core-owned call; this block
	// only projects the response into dialog verdicts.
	if (options.scopes && options.scopes.length > 0) {
		const label = (scope: string) =>
			t
				? t(scope === "project" ? "scopeProject" : "scopeGlobal")
				: scope === "project"
					? "Project"
					: "Global";

		let res: DeleteSkillByNameResponse;
		try {
			res = await api.skills.deleteByName({
				name,
				scopes: options.scopes.map((s) => ({
					scope: s.scope,
					project_root:
						s.scope === "project" ? (s.projectRoot ?? null) : null,
					agents: [...(s.agents ?? [])],
				})),
				confirm: true,
			});
		} catch (error) {
			await invalidate();
			if (await goneAfterTimeout(error, api, name, options.scopes))
				return { verdict: "removed", success: true };
			const refusal = interpretRefusal(error, {
				intent,
				skillName: name,
				t,
			});
			return {
				verdict: "refused",
				success: false,
				message: refusal.message,
				retryWithUnmanaged: refusal.retryWithUnmanaged,
			};
		}

		await invalidate();

		const stillReadByUnmanaged = res.still_read_by_unmanaged;
		const unmanagedKept = stillReadByUnmanaged.length > 0;

		if (res.outcome === "removed" || res.outcome === "absent") {
			return {
				verdict: res.outcome,
				success: true,
				unmanagedKept,
				stillReadByUnmanaged,
			};
		}

		// The last row is the scope the core stopped at.
		const failed = res.scopes[res.scopes.length - 1];
		const reason =
			failed?.error ??
			(failed?.outcome === "kept"
				? t
					? formatDeleteMessage("kept", {
							name,
							t,
							context: options.context,
						})
					: "kept"
				: failed?.outcome === "partial"
					? t
						? formatDeleteMessage("partial", {
								name,
								t,
								context: options.context,
							})
						: "partial"
					: t
						? t("failedToDeleteSkill")
						: "Failed to delete skill");
		// Every row before the stopping row was already run by core.
		const written = res.scopes.slice(0, -1);
		const message =
			written.length > 0
				? t
					? t("deleteSkillCrossScopePartial", {
							writtenScope: written
								.map((s) => label(s.scope))
								.join(", "),
							failedScope: label(failed?.scope ?? "global"),
							reason,
						})
					: `${written.map((s) => label(s.scope)).join(", ")} scope deleted, but ${label(failed?.scope ?? "global")} scope failed: ${reason}`
				: reason;
		return {
			verdict: res.outcome === "partial" ? "partial" : "refused",
			success: false,
			message,
			unmanagedKept,
			stillReadByUnmanaged,
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

		if (
			intent.kind !== "clean-lock" &&
			(await goneAfterTimeout(error, api, name, [
				{
					scope,
					projectRoot,
					agents: intent.kind === "from-agents" ? agents : [],
				},
			]))
		)
			return { verdict: "removed", success: true };

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
