import {
	ArrowPathIcon,
	CheckCircleIcon,
	PlusIcon,
	RectangleStackIcon,
} from "@heroicons/react/24/solid";
import { Button, Dropdown, Spinner, Tooltip, toast } from "@heroui/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useQueryState } from "nuqs";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BulkDeleteDialog } from "../../components/bulk-delete-dialog";
import { BulkManageGroupAgentsDialog } from "../../components/bulk-manage-group-agents-dialog";
import { CreateSkillPanel } from "../../components/create-skill-panel";
import { ImportGithubSkillPanel } from "../../components/import-github-skill-panel";
import { ImportSkillPanel } from "../../components/import-skill-panel";
import { ListSearchHeader } from "../../components/list-search-header";
import { EditSkillTagsDialog } from "../../components/edit-skill-tags-dialog";
import {
	backgroundUpdateNews,
	useLastSkillCheck,
} from "../../hooks/use-last-skill-check";
import { useSkillTags } from "../../hooks/use-skill-tags";
import { allTags, UNTAGGED } from "../../lib/skill-tags";
import { MultiSelectFloatingBar } from "../../components/multi-select-floating-bar";
import { ScopeControl } from "../../components/scope-control";
import {
	SkillAgentCostLine,
	SkillAgentSelect,
	useSkillAgentFilterData,
} from "../../components/skill-agent-filter-row";
import { SkillDetail } from "../../components/skill-detail";
import { SkillList } from "../../components/skill-list";
import type { SourceGroup } from "../../components/skill-list";
import { SkillStatusStrip } from "../../components/skill-status-strip";
import { SourceDetail } from "../../components/source-detail";
import type { SourceRow } from "../../components/source-detail";
import type { SkillResponse, SkillUpdateResponse } from "../../generated/dto";
import { useApi } from "../../hooks/use-api";
import { useApplyAllSkillUpdates } from "../../hooks/use-apply-all-skill-updates";
import { useCredentialSpeedHint } from "../../hooks/use-credential-speed-hint";
import { useGitForwarding } from "../../hooks/use-git-forwarding";
import { useProjects } from "../../hooks/use-projects";
import { filterGroupsByAgent } from "../../lib/skill-agent-filter";
import {
	clearSelectionParams,
	parseScopeParam,
	resolveSourceRow,
	selectSkillParams,
	selectSourceParams,
	serializeScopeParam,
} from "../../lib/skills-page-url";
import {
	batchedSkillCount,
	groupUpdatesBySource,
} from "../../lib/skill-update-batches";
import { cn } from "../../lib/utils";
import {
	checkSkillUpdatesMutationOptions,
	checkSkillUpdatesQueryOptions,
	globalSkillLockQueryOptions,
	projectSkillLockQueryOptions,
	skillListQueryOptions,
} from "../../requests/skills";
import { sourcesListQueryOptions } from "../../requests/sources";
import { queryKeys } from "../../requests/keys";

// ─── Main page ───────────────────────────────────────────────────────────────

export default function SkillsPage() {
	const { t, i18n } = useTranslation();
	const api = useApi();
	const { forBoundSources: forwardForBoundSources } = useGitForwarding();
	const queryClient = useQueryClient();

	// ── Scope: the page's single data root (see lib/skills-page-url.ts) ──
	// `clearOnDefault` is what keeps a global scope out of the URL — this
	// component never special-cases "no param" by hand.
	const [scopeRaw, setScopeRaw] = useQueryState("scope", {
		defaultValue: "global",
		clearOnDefault: true,
	});
	const [selectedSkillName, setSelectedSkillName] = useQueryState("skill");
	const [selectedSourceParam, setSelectedSourceParam] =
		useQueryState("source");

	const { data: projects = [] } = useProjects();
	const knownProjectPaths = useMemo(
		() => new Set(projects.map((p) => p.path)),
		[projects],
	);
	const pageScope = useMemo(
		() => parseScopeParam(scopeRaw, knownProjectPaths),
		[scopeRaw, knownProjectPaths],
	);
	const scope = pageScope.scope;
	const selectedProjectPath =
		pageScope.scope === "project" ? pageScope.projectPath : null;

	// ── Skills data ──
	// Guard: do not fire project-scoped API calls without a project selected —
	// this also covers a `project:<path>` scope whose path is not registered
	// (parseScopeParam already reduced that to `projectPath: null`).
	const projectIsReady = scope !== "project" || selectedProjectPath !== null;

	const skillQueryProjectRoot = useMemo(
		() =>
			scope === "project"
				? (selectedProjectPath ?? undefined)
				: undefined,
		[scope, selectedProjectPath],
	);

	const {
		data: skills = [],
		refetch,
		isFetching,
	} = useQuery({
		...skillListQueryOptions({
			api,
			scope,
			projectRoot: skillQueryProjectRoot,
			enabled: projectIsReady,
		}),
	});

	const updateCheckParams = useMemo(
		() => ({
			scope,
			projectRoot: skillQueryProjectRoot,
		}),
		[scope, skillQueryProjectRoot],
	);

	// Auto-check: fires on mount if data is stale (>10 min), online,
	// and a project is actually selected when in project scope.
	const {
		data: cachedUpdateChecks,
		isFetching: isAutoChecking,
		dataUpdatedAt: checksUpdatedAt,
	} = useQuery(
		checkSkillUpdatesQueryOptions({
			api,
			params: updateCheckParams,
			enabled: navigator.onLine && projectIsReady,
			forwardForBoundSources,
		}),
	);

	// Derive lastCheckedDate from dataUpdatedAt (ms since epoch, 0 = never).
	const lastCheckedDate = useMemo(
		() => (checksUpdatedAt > 0 ? new Date(checksUpdatedAt) : null),
		[checksUpdatedAt],
	);

	// What the OS-scheduled `aghub-cli check` found while the app was closed.
	const { data: lastBackgroundCheck } = useLastSkillCheck();
	const backgroundNews = backgroundUpdateNews(
		lastBackgroundCheck,
		lastCheckedDate,
	);

	// Manual refresh mutation — keeps explicit isPending for the button spinner.
	const checkUpdatesMutation = useMutation(
		checkSkillUpdatesMutationOptions({
			api,
			queryClient,
			forwardForBoundSources,
			onSuccess: (data) => {
				const updateCount = data.filter(
					(s) =>
						s.status === "updateAvailable" ||
						s.status === "renamed",
				).length;
				const uncheckableCount = data.filter(
					(s) => s.status === "uncheckable",
				).length;
				if (updateCount > 0) {
					toast.info(
						t("skillCheckCompleteWithUpdates", {
							count: updateCount,
						}),
					);
				} else if (uncheckableCount > 0) {
					toast.warning(
						t("skillCheckCompleteSomeUncheckable", {
							count: uncheckableCount,
						}),
					);
				} else {
					toast.success(t("skillCheckCompleteAllGood"));
				}
			},
			onError: () => toast.danger(t("skillUpdateCheckError")),
		}),
	);

	const updateStatuses = useMemo(
		() =>
			new Map<string, SkillUpdateResponse>(
				(cachedUpdateChecks ?? []).map((s) => [s.name, s]),
			),
		[cachedUpdateChecks],
	);

	// The lock is the only place a skill's SOURCE is recorded, and
	// `apply-updates` fetches one source per request — so "update all" needs
	// it to know how many requests to send and to which origins. Same query
	// options `SkillList` already uses, so react-query serves one fetch.
	const { data: globalLock } = useQuery({
		...globalSkillLockQueryOptions({ api, enabled: scope === "global" }),
	});
	const { data: projectLock } = useQuery({
		...projectSkillLockQueryOptions({
			api,
			projectPath: selectedProjectPath ?? undefined,
			enabled: scope === "project" && Boolean(selectedProjectPath),
		}),
	});

	const pendingUpdates = useMemo(
		() =>
			groupUpdatesBySource(
				cachedUpdateChecks ?? [],
				(scope === "global" ? globalLock : projectLock)?.skills ?? [],
				scope,
				scope === "project" ? selectedProjectPath : null,
			),
		[
			cachedUpdateChecks,
			globalLock,
			projectLock,
			scope,
			selectedProjectPath,
		],
	);
	const pendingUpdateCount = batchedSkillCount(pendingUpdates.batches);

	const { applyAll, isApplying } = useApplyAllSkillUpdates();

	const handleUpdateAll = async () => {
		const outcome = await applyAll(pendingUpdates.batches);
		if (!outcome) return;
		if (outcome.unconfirmed) {
			// Rows an earlier source already returned are confirmed outcomes;
			// only what came after the failure is unknown. Reporting the whole
			// run as unconfirmed would understate what actually happened.
			toast.danger(
				outcome.updated > 0
					? t("sourceUpdatePartialUnconfirmed", {
							count: outcome.updated,
						})
					: t("sourceUpdateUnconfirmed"),
				{ description: outcome.failureDescription },
			);
			return;
		}
		const failureCount =
			outcome.failures.length + outcome.definiteFailureCount;
		if (failureCount > 0) {
			// Per-row reasons are the only actionable part — a repointed
			// source needs a different response from a network failure.
			toast.danger(
				failureCount === 1
					? t("sourceUpdateSomeFailedOne", { count: 1 })
					: t("sourceUpdateSomeFailedMany", { count: failureCount }),
				{
					description:
						outcome.failures[0]?.error ??
						outcome.failureDescription ??
						undefined,
				},
			);
		} else {
			toast.success(
				t("sourceUpdatesApplied", { count: outcome.updated }),
			);
		}
		// Both are reported AFTER the batch result: they are things this run
		// deliberately did not touch, not failures of what it did.
		if (pendingUpdates.renamed.length > 0) {
			toast.info(
				t("skillUpdateRenamedExcluded", {
					count: pendingUpdates.renamed.length,
				}),
			);
		}
		if (pendingUpdates.unresolved.length > 0) {
			toast.warning(
				t("skillUpdateUnresolvedSource", {
					count: pendingUpdates.unresolved.length,
				}),
			);
		}
	};

	const isRefreshingSkills =
		isFetching || checkUpdatesMutation.isPending || isAutoChecking;

	const handleRefreshSkills = async () => {
		// Guard: do not fire project-scoped API calls without a project selected.
		if (isRefreshingSkills || !projectIsReady) return;
		await refetch();
		// The source panel's per-source diff carries its own staleTime, so
		// without this a refresh keeps serving cached source rows. Not
		// awaited: a stuck/offline source must not hold the button spinning.
		void queryClient.invalidateQueries({
			queryKey: queryKeys.skills.sources.all(),
		});
		checkUpdatesMutation.mutate(updateCheckParams);
	};

	/** Returns a human-readable relative string using the current UI locale. */
	const formatRelativeTime = (date: Date): string => {
		const diffMs = Date.now() - date.getTime();
		const diffSec = Math.round(diffMs / 1_000);
		const diffMin = Math.round(diffMs / 60_000);
		const diffHr = Math.round(diffMs / 3_600_000);
		const diffDay = Math.round(diffMs / 86_400_000);
		const rtf = new Intl.RelativeTimeFormat(i18n.language, {
			numeric: "auto",
		});
		if (diffSec < 60) return rtf.format(-diffSec, "second");
		if (diffMin < 60) return rtf.format(-diffMin, "minute");
		if (diffHr < 24) return rtf.format(-diffHr, "hour");
		return rtf.format(-diffDay, "day");
	};

	// ── List state ──
	const [searchQuery, setSearchQuery] = useState("");
	const [tagFilter, setTagFilter] = useState<Set<string>>(() => new Set());
	// Which agent's skills to show. `null` = every agent. Answers "what does
	// Claude Code actually have installed", which the flat list cannot.
	// Local state, not a URL param: kept across a scope switch (see
	// `handleScopeSelect`), but never worth deep-linking.
	const [agentFilter, setAgentFilter] = useState<string | null>(null);
	// Non-null while the tag dialog is open; holds the names it edits.
	const [tagDialogNames, setTagDialogNames] = useState<string[] | null>(null);
	const [selectedKeys, setSelectedKeys] = useState<Set<string>>(
		() => new Set(),
	);
	const [isBulkDeleteDialogOpen, setIsBulkDeleteDialogOpen] = useState(false);
	const [bulkAgentsGroup, setBulkAgentsGroup] = useState<SourceGroup | null>(
		null,
	);
	const [isMultiSelectMode, setIsMultiSelectMode] = useState(false);
	// pendingAuthSkill: when set, the SkillDetail for this skill will open the
	// credential dialog as soon as it renders.
	const [pendingAuthSkill, setPendingAuthSkill] = useState<string | null>(
		null,
	);
	const [panelMode, setPanelMode] = useState<
		"create" | "import" | "import-github" | null
	>(null);
	// True while the source panel's own "import this" sub-flow is active.
	const [sourceImporting, setSourceImporting] = useState(false);

	const handleScopeSelect = (
		newScope: "global" | "project",
		newProjectPath: string | null,
	) => {
		void setScopeRaw(serializeScopeParam(newScope, newProjectPath));
		const cleared = clearSelectionParams();
		void setSelectedSkillName(cleared.skill);
		void setSelectedSourceParam(cleared.source);
		// A checked-row name from the OLD scope can collide with a
		// same-named skill in the new one (e.g. a skill installed both
		// globally and in a project) and silently look selected there too —
		// multi-select state does not carry meaning across a data-root
		// switch, so it does not survive one either.
		setSelectedKeys(new Set());
		setIsMultiSelectMode(false);
	};

	const groupedSkills = useMemo(() => {
		const map = new Map<string, SkillResponse[]>();
		for (const skill of skills) {
			const existing = map.get(skill.name) ?? [];
			map.set(skill.name, [...existing, skill]);
		}
		return Array.from(map.entries()).map(([name, items]) => ({
			name,
			items,
			description: items.find((s) => s.description)?.description ?? "",
		}));
	}, [skills]);

	// Which groups the agent filter leaves visible. Groups keep ALL their
	// members either way — `items` is what tells the detail panel and the
	// manage-agents dialog which agents already hold the skill.
	const visibleGroups = useMemo(
		() => filterGroupsByAgent(groupedSkills, agentFilter),
		[groupedSkills, agentFilter],
	);

	const activeGroup = useMemo(() => {
		// The right panel shows the source view (found, loading, or "not in
		// this scope") whenever a source is selected — no skill row should be
		// highlighted underneath it, so this must not fall back to group 0.
		if (selectedSourceParam) return null;
		// Resolved within the VISIBLE set, including the explicit selection:
		// filtering to an agent that does not have the selected skill would
		// otherwise leave the detail panel showing a skill the list no longer
		// contains, with no row highlighted anywhere.
		if (selectedSkillName) {
			const selected = visibleGroups.find(
				(g) => g.name === selectedSkillName,
			);
			if (selected) return selected;
		}
		return visibleGroups[0] ?? null;
	}, [selectedSourceParam, selectedSkillName, visibleGroups]);

	const selectedGroups = useMemo(
		() => groupedSkills.filter((g) => selectedKeys.has(g.name)),
		[selectedKeys, groupedSkills],
	);

	const effectiveSelectedKeys = useMemo(() => {
		// Outside multi-select the highlight has exactly ONE owner: whatever
		// the right panel is showing (`activeGroup`, already null while a
		// source is open). `selectedKeys` only records row CLICKS, so reading
		// it here splits the two whenever something else moves the selection —
		// the group header's credential button, or a browser back/forward —
		// leaving the panel on one skill and the highlight on another.
		// Multi-select's checkboxes ARE that mode's selection, so they own it
		// there.
		if (!isMultiSelectMode) {
			return activeGroup
				? new Set([activeGroup.name])
				: new Set<string>();
		}
		return selectedKeys;
	}, [isMultiSelectMode, selectedKeys, activeGroup]);

	/**
	 * Everything that opens a skill in the right panel goes through here: a row
	 * click, the group header's credential button. Anything that only sets the
	 * `skill` param leaves whatever else owns that panel (a create/import form,
	 * the source import sub-flow) in place, and the click reads as dead.
	 */
	const showSkillDetail = (name: string) => {
		const next = selectSkillParams(name);
		void setSelectedSkillName(next.skill);
		void setSelectedSourceParam(next.source);
		setPanelMode(null);
		setSourceImporting(false);
	};

	/**
	 * Multi-select starts from what is on screen, never from the set the last
	 * single-select left behind: outside multi-select `selectedKeys` is not
	 * what the list draws (see `effectiveSelectedKeys`), so restoring it would
	 * silently check a row the user last touched several navigations ago — and
	 * hand the bulk delete/tag bar that row.
	 */
	const toggleMultiSelect = () => {
		setPanelMode(null);
		if (isMultiSelectMode) {
			setIsMultiSelectMode(false);
			setSelectedKeys(new Set());
			return;
		}
		setIsMultiSelectMode(true);
		setSelectedKeys(
			activeGroup && !selectedSourceParam
				? new Set([activeGroup.name])
				: new Set(),
		);
	};

	const handleSelectionChange = (keys: Set<string>, clickedKey?: string) => {
		setSelectedKeys(keys);
		if (clickedKey && !isMultiSelectMode) {
			showSkillDetail(clickedKey);
		}
		if (keys.size > 1 && !isMultiSelectMode) {
			setIsMultiSelectMode(true);
		}
		if (keys.size === 0 && isMultiSelectMode) {
			setIsMultiSelectMode(false);
		}
		setPanelMode(null);
	};

	// ── Sources (current scope only) ──
	const currentProjectName = useMemo(
		() => projects.find((p) => p.path === selectedProjectPath)?.name,
		[projects, selectedProjectPath],
	);

	const {
		data: sourceRowsData,
		isLoading: isSourceRowsLoading,
		isError: isSourceRowsError,
	} = useQuery(
		sourcesListQueryOptions({
			api,
			scope,
			projectRoot:
				scope === "project"
					? (selectedProjectPath ?? undefined)
					: undefined,
			// Guard: an unregistered/not-yet-selected project must not fire a
			// request with `projectRoot: undefined` (that would silently ask
			// for a DIFFERENT scope's sources).
			enabled: scope === "global" || Boolean(selectedProjectPath),
		}),
	);

	const sourceRows = useMemo<SourceRow[]>(
		() =>
			(sourceRowsData?.sources ?? []).map((s) => ({
				...s,
				rowScope: scope,
				...(scope === "project"
					? {
							projectRoot: selectedProjectPath ?? undefined,
							projectName: currentProjectName,
						}
					: {}),
			})),
		[sourceRowsData, scope, selectedProjectPath, currentProjectName],
	);

	const resolvedSourceRow = useMemo<SourceRow | null>(
		() => resolveSourceRow(sourceRows, selectedSourceParam),
		[sourceRows, selectedSourceParam],
	);

	// Update-check states ONLY — a plain skill-list refetch (isFetching) must
	// not trigger the credential speed hint.
	useCredentialSpeedHint({
		checking: isAutoChecking || checkUpdatesMutation.isPending,
		sources: sourceRows,
	});

	const handleOpenSourceView = (source: string) => {
		// `source` is the lock's source id (what group headers carry, never a
		// clone URL). Prefer the resolved row's clone URL so a deep link
		// built from this URL later matches by `sourceUrl` too — but still
		// set SOMETHING when the row can't be found in this scope's list, so
		// the right panel can say so instead of silently doing nothing.
		const row = resolveSourceRow(sourceRows, source);
		const nextValue = row ? row.sourceUrl || row.source : source;
		const next = selectSourceParams(nextValue);
		void setSelectedSkillName(next.skill);
		void setSelectedSourceParam(next.source);
		setSourceImporting(false);
		setPanelMode(null);
	};

	// Agent-select + token-cost data, computed once and shared by Row A's
	// Select and the cost line below it.
	const agentFilterData = useSkillAgentFilterData(skills, agentFilter);

	// Refresh control, shared by the search header and the status strip's
	// tooltip. The last-checked time lives in its tooltip so it never
	// competes for room in the narrow header row.
	const refreshButton = (
		<Tooltip delay={0}>
			<Button
				isIconOnly
				variant="ghost"
				size="sm"
				className="shrink-0"
				aria-label={t("refreshSkills")}
				isDisabled={isRefreshingSkills || !projectIsReady}
				onPress={() => {
					void handleRefreshSkills();
				}}
			>
				<ArrowPathIcon
					className={cn(
						"size-4",
						isRefreshingSkills && "animate-spin",
					)}
				/>
			</Button>
			<Tooltip.Content>
				<div className="flex flex-col gap-0.5">
					<span>{t("refreshSkills")}</span>
					<span className="opacity-70">
						{lastCheckedDate
							? t("lastCheckedAgo", {
									time: formatRelativeTime(lastCheckedDate),
								})
							: t("lastCheckedNever")}
					</span>
				</div>
			</Tooltip.Content>
		</Tooltip>
	);

	return (
		<div className="flex h-full flex-col">
			<div className="flex min-h-0 flex-1">
				{/* Left panel: list */}
				<div className="relative flex w-80 shrink-0 flex-col border-r border-border">
					{/* Row A — ScopeControl + agent filter, side by side. The
					    agent Select drops out entirely (and ScopeControl takes
					    the full row) when there are fewer than two agents with
					    skills and no filter is active. */}
					<div className="px-3 pt-3">
						<div
							className={cn(
								"grid gap-1.5",
								agentFilterData.showSelect
									? "grid-cols-[42fr_58fr]"
									: "grid-cols-1",
							)}
						>
							<ScopeControl
								scope={scope}
								selectedProjectPath={selectedProjectPath}
								onChange={handleScopeSelect}
								// `max-w-none` cancels the default
								// `max-w-[48%]` (coverage.tsx's constraint) —
								// the grid track (either half the row or the
								// whole row) is what should size this here,
								// not a percentage of it.
								className="min-w-0 max-w-none"
							/>
							{agentFilterData.showSelect && (
								<SkillAgentSelect
									data={agentFilterData}
									selected={agentFilter}
									onChange={setAgentFilter}
								/>
							)}
						</div>
						{agentFilterData.showCost && (
							<div className="pt-1.5">
								<SkillAgentCostLine data={agentFilterData} />
							</div>
						)}
					</div>

					<ListSearchHeader
						searchValue={searchQuery}
						onSearchChange={setSearchQuery}
						placeholder={t("searchSkills")}
						ariaLabel={t("searchSkills")}
					>
						<Tooltip delay={0}>
							<Tooltip.Trigger>
								<div
									role="button"
									tabIndex={0}
									className={cn(
										"flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted transition-colors hover:bg-default hover:text-foreground focus:outline-none focus:ring-2 focus:ring-accent/40",
										isMultiSelectMode &&
											"bg-accent/10 text-accent",
									)}
									aria-label={
										isMultiSelectMode
											? t("doneSelecting")
											: t("multiSelect")
									}
									onClick={toggleMultiSelect}
									onKeyDown={(event) => {
										if (
											event.key !== "Enter" &&
											event.key !== " "
										) {
											return;
										}
										event.preventDefault();
										toggleMultiSelect();
									}}
								>
									{isMultiSelectMode ? (
										<CheckCircleIcon className="size-4" />
									) : (
										<RectangleStackIcon className="size-4" />
									)}
								</div>
							</Tooltip.Trigger>
							<Tooltip.Content>
								{isMultiSelectMode
									? t("doneSelecting")
									: t("multiSelect")}
							</Tooltip.Content>
						</Tooltip>
						<Dropdown>
							<Button
								isIconOnly
								variant="ghost"
								size="sm"
								className="shrink-0"
								aria-label={t("addSkill")}
							>
								<PlusIcon className="size-4" />
							</Button>
							<Dropdown.Popover placement="bottom end">
								<Dropdown.Menu
									onAction={(key) => {
										// A source panel (or a stale one) must
										// not keep showing over the panel the
										// user just asked for.
										const cleared = clearSelectionParams();
										setSelectedKeys(new Set());
										void setSelectedSkillName(
											cleared.skill,
										);
										void setSelectedSourceParam(
											cleared.source,
										);
										if (key === "create") {
											setPanelMode("create");
										} else if (key === "import") {
											setPanelMode("import");
										} else if (key === "import-github") {
											setPanelMode("import-github");
										}
									}}
								>
									<Dropdown.Item
										id="create"
										textValue={t("createCustomSkill")}
									>
										{t("createCustomSkill")}
									</Dropdown.Item>
									<Dropdown.Item
										id="import"
										textValue={t("importFromFile")}
									>
										{t("importFromFile")}
									</Dropdown.Item>
									<Dropdown.Item
										id="import-github"
										textValue={t("importRemoteSource")}
									>
										{t("importRemoteSource")}
									</Dropdown.Item>
								</Dropdown.Menu>
							</Dropdown.Popover>
						</Dropdown>
						{refreshButton}
					</ListSearchHeader>

					<SkillStatusStrip
						scope={scope}
						projectPath={selectedProjectPath ?? undefined}
						pendingUpdateCount={pendingUpdateCount}
						onUpdateAll={() => {
							void handleUpdateAll();
						}}
						isApplyingUpdates={isApplying}
						backgroundNews={backgroundNews}
						onRefresh={() => {
							void handleRefreshSkills();
						}}
						isRefreshing={isRefreshingSkills}
					/>

					<SkillTagFilterRow
						selected={tagFilter}
						onChange={setTagFilter}
					/>

					<SkillList
						skills={skills}
						agentFilter={agentFilter}
						selectedKeys={effectiveSelectedKeys}
						searchQuery={searchQuery}
						tagFilter={tagFilter}
						onSelectionChange={handleSelectionChange}
						selectionMode="multiple"
						isMultiSelectMode={isMultiSelectMode}
						groupBySource={true}
						projectPath={
							scope === "project"
								? (selectedProjectPath ?? undefined)
								: undefined
						}
						updateStatuses={updateStatuses}
						selectedSource={resolvedSourceRow?.source ?? null}
						onResolveAuth={(skillName: string) => {
							showSkillDetail(skillName);
							setPendingAuthSkill(skillName);
						}}
						onManageGroupAgents={setBulkAgentsGroup}
						onOpenSourceView={handleOpenSourceView}
					/>

					{isMultiSelectMode && selectedKeys.size > 0 && (
						<MultiSelectFloatingBar
							selectedCount={selectedKeys.size}
							totalCount={groupedSkills.length}
							onManageTags={() =>
								setTagDialogNames([...selectedKeys])
							}
							onDelete={() => setIsBulkDeleteDialogOpen(true)}
						/>
					)}
				</div>

				{/* Right panel: detail */}
				<div className="relative flex-1 overflow-hidden">
					{!projectIsReady ? (
						<div className="flex h-full flex-col items-center justify-center gap-4">
							<p className="text-sm text-muted">
								{t("selectProject")}
							</p>
						</div>
					) : selectedSourceParam ? (
						isSourceRowsLoading ? (
							<div className="flex h-full items-center justify-center">
								<Spinner />
							</div>
						) : isSourceRowsError ? (
							// A failed query leaves `sources` empty, which is
							// NOT the same claim as "this scope has no such
							// source" (src/AGENTS.md: `!isLoading` is not "the
							// data is trustworthy").
							<div className="flex h-full flex-col items-center justify-center gap-4">
								<p
									role="alert"
									aria-live="polite"
									className="text-sm text-muted"
								>
									{t("sourceListLoadFailed")}
								</p>
							</div>
						) : resolvedSourceRow ? (
							sourceImporting ? (
								<ImportGithubSkillPanel
									initialUrl={resolvedSourceRow.sourceUrl}
									projectPath={
										resolvedSourceRow.rowScope === "project"
											? resolvedSourceRow.projectRoot
											: undefined
									}
									onDone={() => {
										setSourceImporting(false);
										void queryClient.invalidateQueries({
											queryKey:
												queryKeys.skills.sources.all(),
										});
									}}
								/>
							) : (
								<SourceDetail
									// Keyed so per-source state (a running "Update
									// all") cannot leak into another source's panel
									// and disable its buttons.
									key={
										resolvedSourceRow.sourceUrl ||
										resolvedSourceRow.source
									}
									row={resolvedSourceRow}
									onImport={() => setSourceImporting(true)}
								/>
							)
						) : (
							<div className="flex h-full flex-col items-center justify-center gap-4">
								<p className="text-sm text-muted">
									{t("sourceNotInCurrentScope")}
								</p>
							</div>
						)
					) : panelMode === "create" ? (
						<CreateSkillPanel
							onDone={() => setPanelMode(null)}
							projectPath={selectedProjectPath ?? undefined}
						/>
					) : panelMode === "import" ? (
						<ImportSkillPanel
							onDone={() => setPanelMode(null)}
							projectPath={selectedProjectPath ?? undefined}
						/>
					) : panelMode === "import-github" ? (
						<ImportGithubSkillPanel
							onDone={() => setPanelMode(null)}
							projectPath={selectedProjectPath ?? undefined}
						/>
					) : activeGroup ? (
						<SkillDetail
							key={
								pendingAuthSkill === activeGroup.name
									? `${activeGroup.name}-cred`
									: activeGroup.name
							}
							group={activeGroup}
							projectPath={selectedProjectPath ?? undefined}
							// Same route: `setLocation` would push a URL nuqs
							// never re-reads, so the panel would not change.
							onOpenSource={handleOpenSourceView}
							openCredDialog={
								pendingAuthSkill === activeGroup.name
							}
							onCredDialogClose={() => {
								setPendingAuthSkill(null);
							}}
						/>
					) : (
						<div className="flex h-full flex-col items-center justify-center gap-4">
							<p className="text-sm text-muted">
								{t("selectSkill")}
							</p>
						</div>
					)}

					<EditSkillTagsDialog
						isOpen={tagDialogNames !== null}
						names={tagDialogNames ?? []}
						onClose={() => setTagDialogNames(null)}
					/>

					<BulkDeleteDialog
						isOpen={isBulkDeleteDialogOpen}
						onClose={() => setIsBulkDeleteDialogOpen(false)}
						groups={selectedGroups.map((g) => ({
							key: g.name,
							items: g.items,
						}))}
						onSuccess={() => {
							handleSelectionChange(new Set());
							void refetch();
						}}
						resourceType="skill"
					/>

					{bulkAgentsGroup && (
						<BulkManageGroupAgentsDialog
							kind="skill"
							isOpen={!!bulkAgentsGroup}
							source={bulkAgentsGroup.source}
							resources={bulkAgentsGroup.skills.map((sg) => ({
								name: sg.name,
								items: sg.items.flatMap((it) =>
									it.agent
										? [
												{
													agent: it.agent,
													source:
														it.source ?? "global",
												},
											]
										: [],
								),
							}))}
							scope={scope}
							projectPath={
								scope === "project"
									? (selectedProjectPath ?? undefined)
									: undefined
							}
							onClose={() => setBulkAgentsGroup(null)}
						/>
					)}
				</div>
			</div>
		</div>
	);
}

/** Tag filter chips. Renders nothing until at least one tag exists, so a user
 * who never tags anything sees the list exactly as before. */
function SkillTagFilterRow({
	selected,
	onChange,
}: {
	selected: Set<string>;
	onChange: (next: Set<string>) => void;
}) {
	const { t } = useTranslation();
	const { tags } = useSkillTags();
	// A tag stays in the row while it is SELECTED even after its last skill
	// loses it — otherwise the chip vanishes with the filter still active and
	// the list is empty with nothing left to click.
	const available = [
		...new Set([
			...allTags(tags),
			...[...selected].filter((tag) => tag !== UNTAGGED),
		]),
	].sort((a, b) => a.localeCompare(b));
	if (available.length === 0 && !selected.has(UNTAGGED)) return null;

	const toggle = (tag: string) => {
		const next = new Set(selected);
		if (next.has(tag)) next.delete(tag);
		else next.add(tag);
		onChange(next);
	};

	const chip = (tag: string, label: string) => (
		<button
			key={tag}
			type="button"
			onClick={() => toggle(tag)}
			aria-pressed={selected.has(tag)}
			className={cn(
				"rounded-full border border-separator px-2 py-0.5 text-xs transition-colors",
				selected.has(tag)
					? "bg-accent/10 text-accent"
					: "text-muted hover:bg-surface-secondary",
			)}
		>
			{label}
		</button>
	);

	return (
		<div className="flex flex-wrap items-center gap-1.5 px-3 pb-2">
			{available.map((tag) => chip(tag, tag))}
			{chip(UNTAGGED, t("untagged"))}
		</div>
	);
}
