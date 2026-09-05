import {
	ArrowPathIcon,
	CheckCircleIcon,
	GlobeAltIcon,
	PlusIcon,
	RectangleStackIcon,
} from "@heroicons/react/24/solid";
import {
	Button,
	Dropdown,
	Spinner,
	ToggleButton,
	ToggleButtonGroup,
	Tooltip,
	toast,
} from "@heroui/react";
import {
	useMutation,
	useQueries,
	useQuery,
	useQueryClient,
} from "@tanstack/react-query";
import { useQueryState } from "nuqs";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { SkillLayoutMigrationBanner } from "../../components/skill-layout-migration-banner";
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
import {
	PROJECT_KEY_PREFIX,
	ScopeControl,
} from "../../components/scope-control";
import { SkillAgentFilterRow } from "../../components/skill-agent-filter-row";
import { SkillDetail } from "../../components/skill-detail";
import { SkillList } from "../../components/skill-list";
import type { SourceGroup } from "../../components/skill-list";
import { SourceDetail } from "../../components/source-detail";
import type { SourceRow } from "../../components/source-detail";
import type {
	SkillResponse,
	SkillUpdateResponse,
	SourcesListResponse,
} from "../../generated/dto";
import { useApi } from "../../hooks/use-api";
import { useApplyAllSkillUpdates } from "../../hooks/use-apply-all-skill-updates";
import { useCredentialSpeedHint } from "../../hooks/use-credential-speed-hint";
import { useGitForwarding } from "../../hooks/use-git-forwarding";
import { useProjects } from "../../hooks/use-projects";
import { filterGroupsByAgent } from "../../lib/skill-agent-filter";
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

// ─── Types ───────────────────────────────────────────────────────────────────

type ViewMode = "agent" | "source";

const TRAILING_SLASH_RE = /\/+$/;

function sourceDisplayName(row: SourceRow): string {
	if (row.sourceType !== "local") return row.source;
	const trimmed = row.source.replace(TRAILING_SLASH_RE, "");
	return trimmed.split("/").filter(Boolean).pop() ?? row.source;
}

function sourceRowKey(r: SourceRow) {
	// Keyed on `sourceUrl`, not `source`: rows are grouped by repository origin,
	// so two forges serving one `owner/repo` are two rows that SHARE the
	// host-blind `source`. `sourceUrl` is unique per origin by construction.
	// Falls back for entries with no recorded URL (local sources).
	return `${r.rowScope}:${r.projectRoot ?? ""}:${r.sourceUrl || r.source}`;
}

// ─── Source list panel (view=source) ─────────────────────────────────────────

interface SourceListPanelProps {
	scope: "global" | "project";
	projectPath: string | null;
	selectedKey: string | null;
	onSelectKey: (key: string) => void;
	searchQuery: string;
}

function SourceListPanel({
	scope,
	projectPath,
	selectedKey,
	onSelectKey,
	searchQuery,
}: SourceListPanelProps) {
	const { t } = useTranslation();
	const api = useApi();
	const { data: projects = [] } = useProjects();

	// Guard: do not query project sources until a project is actually selected.
	const projectIsReady = scope !== "project" || projectPath !== null;

	const sourceQueries = useQueries({
		queries:
			scope === "global"
				? [sourcesListQueryOptions({ api, scope: "global" })]
				: projects
						.filter((p) => !projectPath || p.path === projectPath)
						.map((p) =>
							sourcesListQueryOptions({
								api,
								scope: "project",
								projectRoot: p.path,
								enabled: projectIsReady,
							}),
						),
	});

	const isLoading = sourceQueries.some((q) => q.isLoading);

	const rows = useMemo<SourceRow[]>(() => {
		if (scope === "global") {
			const data = sourceQueries[0]?.data as
				| SourcesListResponse
				| undefined;
			return (data?.sources ?? []).map((s) => ({
				...s,
				rowScope: "global" as const,
			}));
		}
		const filtered = projectPath
			? projects.filter((p) => p.path === projectPath)
			: projects;
		return filtered.flatMap((project, index) => {
			const data = sourceQueries[index]?.data as
				| SourcesListResponse
				| undefined;
			return (data?.sources ?? []).map((s) => ({
				...s,
				rowScope: "project" as const,
				projectRoot: project.path,
				projectName: project.name,
			}));
		});
	}, [sourceQueries, projects, scope, projectPath]);

	const filteredRows = useMemo(() => {
		const q = searchQuery.trim().toLowerCase();
		if (!q) return rows;
		return rows.filter((r) => r.source.toLowerCase().includes(q));
	}, [rows, searchQuery]);

	// Show "select a project" prompt before firing any project-scoped query.
	if (scope === "project" && projectPath === null) {
		return (
			<p className="px-4 py-8 text-center text-sm text-muted">
				{t("selectProject")}
			</p>
		);
	}

	if (isLoading) {
		return (
			<div className="flex flex-1 items-center justify-center">
				<Spinner />
			</div>
		);
	}

	if (filteredRows.length === 0) {
		return (
			<p className="px-4 py-8 text-center text-sm text-muted">
				{t("sourcesEmpty")}
			</p>
		);
	}

	return (
		<div className="min-h-0 flex-1 overflow-y-auto [transform:translateZ(0)]">
			<ul className="space-y-1 p-2">
				{filteredRows.map((row) => {
					const key = sourceRowKey(row);
					const isActive = key === selectedKey;
					return (
						<li key={key}>
							<button
								type="button"
								onClick={() => onSelectKey(key)}
								aria-current={isActive ? "page" : undefined}
								className={cn(
									"group flex h-12 w-full items-center gap-3 rounded-xl px-3 text-left outline-none transition-colors hover:bg-surface-secondary focus-visible:ring-2 focus-visible:ring-accent/35",
									isActive && "bg-surface",
								)}
							>
								<GlobeAltIcon
									className={cn(
										"size-5 shrink-0 text-muted",
										isActive && "text-foreground",
									)}
								/>
								<span
									className="min-w-0 flex-1 truncate text-base font-semibold leading-6 text-foreground"
									title={row.source}
								>
									{sourceDisplayName(row)}
								</span>
								<span className="ml-auto shrink-0 rounded-full bg-surface-tertiary px-2 font-mono text-sm leading-7 text-muted tabular-nums">
									{row.skillCount}
								</span>
							</button>
						</li>
					);
				})}
			</ul>
		</div>
	);
}

// ─── Main page ───────────────────────────────────────────────────────────────

export default function SkillsPage() {
	const { t, i18n } = useTranslation();
	const api = useApi();
	const { forBoundSources: forwardForBoundSources } = useGitForwarding();
	const queryClient = useQueryClient();

	// ── URL state (nuqs) ──
	const [view, setView] = useQueryState<ViewMode>("view", {
		defaultValue: "agent",
		parse: (v): ViewMode => (v === "source" ? "source" : "agent"),
		serialize: (v) => v,
	});
	const [selectedSkillName, setSelectedSkillName] = useQueryState("skill");
	const [selectedSourceKey, setSelectedSourceKey] = useQueryState("source");

	const handleSetView = (newView: ViewMode) => {
		// Clear the other view's param when switching
		if (newView === "source") {
			void setSelectedSkillName(null);
		} else {
			void setSelectedSourceKey(null);
		}
		void setView(newView);
	};

	// ── Scope / project state — derived from URL source key on initial load ──
	// Parse scope and projectRoot from a composite source key "scope:projectRoot:source".
	const parseScopeFromKey = (key: string | null): "global" | "project" =>
		key?.startsWith("project:") ? "project" : "global";
	const parseProjectFromKey = (key: string | null): string | null => {
		if (!key?.startsWith("project:")) return null;
		// key format: "project:<projectRoot>:<source>" where projectRoot may be empty
		const afterPrefix = key.slice("project:".length);
		const colonIdx = afterPrefix.indexOf(":");
		return colonIdx === -1 ? null : afterPrefix.slice(0, colonIdx) || null;
	};

	const [scope, setScope] = useState<"global" | "project">(() =>
		parseScopeFromKey(selectedSourceKey),
	);
	const [selectedProjectPath, setSelectedProjectPath] = useState<
		string | null
	>(() => parseProjectFromKey(selectedSourceKey));

	const handleScopeSelect = (
		newScope: "global" | "project",
		newProjectPath: string | null,
	) => {
		setScope(newScope);
		setSelectedProjectPath(newProjectPath);
		// Clear a selected source that no longer belongs to the new scope/project.
		if (selectedSourceKey !== null) {
			if (newScope === "global") {
				if (selectedSourceKey.startsWith(PROJECT_KEY_PREFIX)) {
					void setSelectedSourceKey(null);
				}
			} else if (
				parseProjectFromKey(selectedSourceKey) !== newProjectPath
			) {
				void setSelectedSourceKey(null);
			}
		}
	};

	// ── Skills data ──
	// Guard: do not fire project-scoped skill query until a project is selected.
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
			toast.danger(t("sourceUpdateUnconfirmed"), {
				description: outcome.failureDescription,
			});
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
		// The source view's list + per-source diff carry their own staleTime, so
		// without this a refresh in that view keeps serving cached rows. Not
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

	// ── Agent-view state ──
	const [searchQuery, setSearchQuery] = useState("");
	const [tagFilter, setTagFilter] = useState<Set<string>>(() => new Set());
	// Which agent's skills to show. `null` = every agent. Answers "what does
	// Claude Code actually have installed", which the flat list cannot.
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
		if (selectedSkillName) {
			return (
				groupedSkills.find((g) => g.name === selectedSkillName) ?? null
			);
		}
		// Fall back within the VISIBLE set, or filtering to one agent leaves
		// the detail panel parked on a skill that agent does not have.
		return visibleGroups[0] ?? null;
	}, [selectedSkillName, groupedSkills, visibleGroups]);

	const selectedGroups = useMemo(
		() => groupedSkills.filter((g) => selectedKeys.has(g.name)),
		[selectedKeys, groupedSkills],
	);

	const effectiveSelectedKeys = useMemo(() => {
		if (selectedKeys.size > 0) return selectedKeys;
		if (activeGroup && !isMultiSelectMode) {
			return new Set([activeGroup.name]);
		}
		return new Set<string>();
	}, [selectedKeys, activeGroup, isMultiSelectMode]);

	const handleSelectionChange = (keys: Set<string>, clickedKey?: string) => {
		setSelectedKeys(keys);
		if (clickedKey && !isMultiSelectMode) {
			void setSelectedSkillName(clickedKey);
		}
		if (keys.size > 1 && !isMultiSelectMode) {
			setIsMultiSelectMode(true);
		}
		if (keys.size === 0 && isMultiSelectMode) {
			setIsMultiSelectMode(false);
		}
		setPanelMode(null);
	};

	// ── Source-view state ──
	const [sourceImporting, setSourceImporting] = useState(false);

	// When selecting a source, also sync the scope/project segments from the key.
	// Key format: "scope:projectRoot:source"
	const handleSourceSelect = (key: string) => {
		void setSelectedSourceKey(key);
		setSourceImporting(false);
		const keyScope = parseScopeFromKey(key);
		const keyProject = parseProjectFromKey(key);
		setScope(keyScope);
		setSelectedProjectPath(keyProject);
	};
	const { data: projects = [] } = useProjects();

	// Load all sources so we can resolve a full SourceRow from the URL key.
	const allSourcesQuery = useQueries({
		queries: [
			sourcesListQueryOptions({ api, scope: "global" }),
			...projects.map((p) =>
				sourcesListQueryOptions({
					api,
					scope: "project",
					projectRoot: p.path,
				}),
			),
		],
	});

	const allSourceRows = useMemo<SourceRow[]>(() => {
		const globalData = allSourcesQuery[0]?.data as
			| SourcesListResponse
			| undefined;
		const globalRows: SourceRow[] = (globalData?.sources ?? []).map(
			(s) => ({ ...s, rowScope: "global" as const }),
		);
		const projectRows: SourceRow[] = projects.flatMap((p, i) => {
			const data = allSourcesQuery[i + 1]?.data as
				| SourcesListResponse
				| undefined;
			return (data?.sources ?? []).map((s) => ({
				...s,
				rowScope: "project" as const,
				projectRoot: p.path,
				projectName: p.name,
			}));
		});
		return [...globalRows, ...projectRows];
	}, [allSourcesQuery, projects]);

	const resolvedSourceRow = useMemo<SourceRow | null>(() => {
		if (!selectedSourceKey) return null;
		return (
			allSourceRows.find((r) => sourceRowKey(r) === selectedSourceKey) ??
			null
		);
	}, [selectedSourceKey, allSourceRows]);

	// Update-check states ONLY — a plain skill-list refetch (isFetching) must
	// not trigger the credential speed hint.
	useCredentialSpeedHint({
		checking: isAutoChecking || checkUpdatesMutation.isPending,
		sources: allSourceRows,
	});

	// Refresh control, shared by the agent + source toolbars. The last-checked
	// time lives in its tooltip so it never competes for room in the narrow
	// header row.
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
			{/* Above the panels, not inside the list: the layout is a property
			    of the whole scope, not of whichever skill happens to be
			    selected. Renders nothing when there is nothing to migrate. */}
			<div className="px-3 pt-3 empty:hidden">
				<SkillLayoutMigrationBanner scope="global" />
			</div>
			<div className="flex min-h-0 flex-1">
				{/* Left panel: list */}
				<div className="relative flex w-80 shrink-0 flex-col border-r border-border">
					{/* Row A — view + scope (replaces the old full-width
					    header + view-toggle + segmented scope rows) */}
					<div className="flex items-center justify-between gap-2 px-3 pt-3">
						<ToggleButtonGroup
							selectedKeys={[view]}
							onSelectionChange={(keys) =>
								handleSetView([...keys][0] as ViewMode)
							}
							selectionMode="single"
							disallowEmptySelection
							size="sm"
						>
							<ToggleButton id="agent">
								{t("viewByAgent")}
							</ToggleButton>
							<ToggleButtonGroup.Separator />
							<ToggleButton id="source">
								{t("viewBySource")}
							</ToggleButton>
						</ToggleButtonGroup>
						<ScopeControl
							scope={scope}
							selectedProjectPath={selectedProjectPath}
							onChange={handleScopeSelect}
						/>
					</div>

					{backgroundNews !== null && (
						<button
							type="button"
							className="mx-3 mb-2 rounded-md border border-separator bg-surface-secondary px-3 py-2 text-left text-xs text-foreground transition-colors hover:bg-surface"
							onClick={() => {
								void handleRefreshSkills();
							}}
						>
							{t("backgroundCheckFoundUpdates", {
								count: backgroundNews,
							})}
						</button>
					)}

					{view === "agent" ? (
						<>
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
											onClick={() => {
												setIsMultiSelectMode(
													(prev) => !prev,
												);
												if (isMultiSelectMode) {
													handleSelectionChange(
														new Set(),
													);
												}
											}}
											onKeyDown={(event) => {
												if (
													event.key !== "Enter" &&
													event.key !== " "
												) {
													return;
												}
												event.preventDefault();
												setIsMultiSelectMode(
													(prev) => !prev,
												);
												if (isMultiSelectMode) {
													handleSelectionChange(
														new Set(),
													);
												}
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
												if (key === "create") {
													setSelectedKeys(new Set());
													void setSelectedSkillName(
														null,
													);
													setPanelMode("create");
												} else if (key === "import") {
													setSelectedKeys(new Set());
													void setSelectedSkillName(
														null,
													);
													setPanelMode("import");
												} else if (
													key === "import-github"
												) {
													setSelectedKeys(new Set());
													void setSelectedSkillName(
														null,
													);
													setPanelMode(
														"import-github",
													);
												}
											}}
										>
											<Dropdown.Item
												id="create"
												textValue={t(
													"createCustomSkill",
												)}
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
												textValue={t(
													"importRemoteSource",
												)}
											>
												{t("importRemoteSource")}
											</Dropdown.Item>
										</Dropdown.Menu>
									</Dropdown.Popover>
								</Dropdown>
								{pendingUpdateCount > 0 && (
									<Tooltip delay={0}>
										<Button
											variant="ghost"
											size="sm"
											className="shrink-0"
											isDisabled={
												isApplying || isRefreshingSkills
											}
											onPress={() => {
												void handleUpdateAll();
											}}
										>
											{isApplying ? (
												<Spinner size="sm" />
											) : (
												<ArrowPathIcon className="size-4 text-warning" />
											)}
											{t("updateAllSkills", {
												count: pendingUpdateCount,
											})}
										</Button>
										<Tooltip.Content>
											{t("updateAllSkills", {
												count: pendingUpdateCount,
											})}
										</Tooltip.Content>
									</Tooltip>
								)}
								{refreshButton}
							</ListSearchHeader>

							<SkillAgentFilterRow
								skills={skills}
								selected={agentFilter}
								onChange={setAgentFilter}
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
								onResolveAuth={(skillName: string) => {
									void setSelectedSkillName(skillName);
									setPendingAuthSkill(skillName);
								}}
								onManageGroupAgents={setBulkAgentsGroup}
								onOpenSourceView={(source) => {
									// Reuse the real SourceRow's key so we never
									// re-encode the composite-key format (which is
									// not colon-safe) by hand.
									const row = allSourceRows.find(
										(r) =>
											r.rowScope === scope &&
											r.source === source &&
											(scope === "global" ||
												(r.projectRoot ?? null) ===
													(selectedProjectPath ??
														null)),
									);
									setSearchQuery("");
									if (row)
										handleSourceSelect(sourceRowKey(row));
									handleSetView("source");
								}}
							/>

							{isMultiSelectMode && selectedKeys.size > 0 && (
								<MultiSelectFloatingBar
									selectedCount={selectedKeys.size}
									totalCount={groupedSkills.length}
									onManageTags={() =>
										setTagDialogNames([...selectedKeys])
									}
									onDelete={() =>
										setIsBulkDeleteDialogOpen(true)
									}
								/>
							)}
						</>
					) : (
						<>
							<ListSearchHeader
								searchValue={searchQuery}
								onSearchChange={setSearchQuery}
								placeholder={t("searchSources")}
								ariaLabel={t("searchSources")}
							>
								{refreshButton}
							</ListSearchHeader>
							<SourceListPanel
								scope={scope}
								projectPath={selectedProjectPath}
								selectedKey={selectedSourceKey}
								onSelectKey={handleSourceSelect}
								searchQuery={searchQuery}
							/>
						</>
					)}
				</div>

				{/* Right panel: detail */}
				<div className="relative flex-1 overflow-hidden">
					{view === "agent" && !projectIsReady ? (
						<div className="flex h-full flex-col items-center justify-center gap-4">
							<p className="text-sm text-muted">
								{t("selectProject")}
							</p>
						</div>
					) : view === "agent" ? (
						panelMode === "create" ? (
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
						)
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
								key={sourceRowKey(resolvedSourceRow)}
								row={resolvedSourceRow}
								onImport={() => setSourceImporting(true)}
							/>
						)
					) : (
						<div className="flex h-full flex-col items-center justify-center gap-4">
							<p className="text-sm text-muted">
								{t("selectSource")}
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
