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
	ListBox,
	Select,
	Spinner,
	Tooltip,
	toast,
} from "@heroui/react";
import {
	useMutation,
	useQueries,
	useQuery,
	useQueryClient,
	useSuspenseQuery,
} from "@tanstack/react-query";
import { useQueryState } from "nuqs";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BulkDeleteDialog } from "../../components/bulk-delete-dialog";
import { CreateSkillPanel } from "../../components/create-skill-panel";
import { ImportGithubSkillPanel } from "../../components/import-github-skill-panel";
import { ImportSkillPanel } from "../../components/import-skill-panel";
import { ListSearchHeader } from "../../components/list-search-header";
import { MultiSelectFloatingBar } from "../../components/multi-select-floating-bar";
import { SkillDetail } from "../../components/skill-detail";
import { SkillList } from "../../components/skill-list";
import { SourceDetail } from "../../components/source-detail";
import type { SourceRow } from "../../components/source-detail";
import type {
	SkillResponse,
	SkillUpdateResponse,
	SourcesListResponse,
} from "../../generated/dto";
import { useApi } from "../../hooks/use-api";
import { useProjects } from "../../hooks/use-projects";
import { cn } from "../../lib/utils";
import {
	checkSkillUpdatesMutationOptions,
	checkSkillUpdatesQueryOptions,
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
	return `${r.rowScope}:${r.projectRoot ?? ""}:${r.source}`;
}

// ─── Scope switch ─────────────────────────────────────────────────────────────

interface ScopeSwitchProps {
	scope: "global" | "project";
	onScopeChange: (scope: "global" | "project") => void;
	selectedProjectPath: string | null;
	onProjectChange: (path: string) => void;
}

function ScopeSwitch({
	scope,
	onScopeChange,
	selectedProjectPath,
	onProjectChange,
}: ScopeSwitchProps) {
	const { t } = useTranslation();
	const { data: projects = [] } = useProjects();

	return (
		<div className="flex items-center gap-2 border-b border-border px-3 py-2">
			<div className="flex rounded-lg bg-surface-secondary p-0.5">
				<button
					type="button"
					className={cn(
						"rounded-md px-3 py-1 text-xs font-medium transition-colors",
						scope === "global"
							? "bg-white text-foreground shadow-sm"
							: "text-muted hover:text-foreground",
					)}
					onClick={() => onScopeChange("global")}
				>
					{t("scopeSwitchGlobal")}
				</button>
				<button
					type="button"
					disabled={projects.length === 0}
					className={cn(
						"rounded-md px-3 py-1 text-xs font-medium transition-colors",
						scope === "project"
							? "bg-white text-foreground shadow-sm"
							: "text-muted hover:text-foreground",
						projects.length === 0 &&
							"cursor-not-allowed opacity-40",
					)}
					onClick={() => {
						if (projects.length > 0) onScopeChange("project");
					}}
				>
					{t("scopeSwitchProject")}
				</button>
			</div>
			{scope === "project" && projects.length > 0 && (
				<Select
					className="min-w-0 flex-1"
					variant="secondary"
					selectedKey={selectedProjectPath ?? undefined}
					onSelectionChange={(key) => {
						if (key) onProjectChange(String(key));
					}}
				>
					<Select.Trigger>
						<Select.Value />
						<Select.Indicator />
					</Select.Trigger>
					<Select.Popover>
						<ListBox>
							{projects.map((p) => (
								<ListBox.Item
									key={p.path}
									id={p.path}
									textValue={p.name}
								>
									{p.name}
									<ListBox.ItemIndicator />
								</ListBox.Item>
							))}
						</ListBox>
					</Select.Popover>
				</Select>
			)}
		</div>
	);
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
		<div className="min-h-0 flex-1 overflow-y-auto">
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

	// ── Scope state ──
	const [scope, setScope] = useState<"global" | "project">("global");
	const [selectedProjectPath, setSelectedProjectPath] = useState<
		string | null
	>(null);

	const handleScopeChange = (newScope: "global" | "project") => {
		setScope(newScope);
		if (newScope === "global") {
			setSelectedProjectPath(null);
		}
	};

	// ── Skills data ──
	const skillQueryProjectRoot = useMemo(
		() =>
			scope === "project"
				? (selectedProjectPath ?? undefined)
				: undefined,
		[scope, selectedProjectPath],
	);

	const {
		data: skills,
		refetch,
		isFetching,
	} = useSuspenseQuery({
		...skillListQueryOptions({
			api,
			scope,
			projectRoot: skillQueryProjectRoot,
		}),
	});

	const updateCheckParams = useMemo(
		() => ({
			scope,
			projectRoot: skillQueryProjectRoot,
		}),
		[scope, skillQueryProjectRoot],
	);

	// Auto-check: fires on mount if data is stale (>10 min) and online.
	// navigator.onLine suppresses the check when offline.
	const {
		data: cachedUpdateChecks,
		isFetching: isAutoChecking,
		dataUpdatedAt: checksUpdatedAt,
	} = useQuery(
		checkSkillUpdatesQueryOptions({
			api,
			params: updateCheckParams,
			enabled: navigator.onLine,
		}),
	);

	// Derive lastCheckedDate from dataUpdatedAt (ms since epoch, 0 = never).
	const lastCheckedDate = useMemo(
		() => (checksUpdatedAt > 0 ? new Date(checksUpdatedAt) : null),
		[checksUpdatedAt],
	);

	// Manual refresh mutation — keeps explicit isPending for the button spinner.
	const checkUpdatesMutation = useMutation(
		checkSkillUpdatesMutationOptions({
			api,
			queryClient,
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

	const isRefreshingSkills =
		isFetching || checkUpdatesMutation.isPending || isAutoChecking;

	const handleRefreshSkills = async () => {
		if (isRefreshingSkills) return;
		await refetch();
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
	const [selectedKeys, setSelectedKeys] = useState<Set<string>>(
		() => new Set(),
	);
	const [isBulkDeleteDialogOpen, setIsBulkDeleteDialogOpen] = useState(false);
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

	const activeGroup = useMemo(() => {
		if (selectedSkillName) {
			return (
				groupedSkills.find((g) => g.name === selectedSkillName) ?? null
			);
		}
		return groupedSkills[0] ?? null;
	}, [selectedSkillName, groupedSkills]);

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

	const resolvedSourceRow = useMemo<SourceRow | null>(() => {
		if (!selectedSourceKey) return null;
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
		const allRows = [...globalRows, ...projectRows];
		return (
			allRows.find((r) => sourceRowKey(r) === selectedSourceKey) ?? null
		);
	}, [selectedSourceKey, allSourcesQuery, projects]);

	return (
		<div className="flex h-full flex-col">
			{/* Page header with last-checked and recheck */}
			<div className="flex shrink-0 items-center justify-between gap-3 border-b border-border px-4 py-2.5">
				<h1 className="text-base font-semibold text-foreground">
					{t("skillCenter")}
				</h1>
				<div className="flex items-center gap-2">
					<span className="text-xs text-muted">
						{lastCheckedDate
							? t("lastCheckedAgo", {
									time: formatRelativeTime(lastCheckedDate),
								})
							: t("lastCheckedNever")}
					</span>
					<Tooltip delay={0}>
						<Button
							isIconOnly
							variant="ghost"
							size="sm"
							aria-label={t("refreshSkills")}
							isDisabled={isRefreshingSkills}
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
						<Tooltip.Content>{t("refreshSkills")}</Tooltip.Content>
					</Tooltip>
				</div>
			</div>

			{/* View-by toggle */}
			<div className="flex shrink-0 items-center gap-3 border-b border-border px-4 py-2">
				<div className="flex rounded-lg bg-surface-secondary p-0.5">
					<button
						type="button"
						className={cn(
							"rounded-md px-3 py-1 text-xs font-medium transition-colors",
							view === "agent"
								? "bg-white text-foreground shadow-sm"
								: "text-muted hover:text-foreground",
						)}
						onClick={() => handleSetView("agent")}
					>
						{t("viewByAgent")}
					</button>
					<button
						type="button"
						className={cn(
							"rounded-md px-3 py-1 text-xs font-medium transition-colors",
							view === "source"
								? "bg-white text-foreground shadow-sm"
								: "text-muted hover:text-foreground",
						)}
						onClick={() => handleSetView("source")}
					>
						{t("viewBySource")}
					</button>
				</div>
			</div>

			<div className="flex min-h-0 flex-1">
				{/* Left panel: list */}
				<div className="relative flex w-80 shrink-0 flex-col border-r border-border">
					{/* Scope switch */}
					<ScopeSwitch
						scope={scope}
						onScopeChange={handleScopeChange}
						selectedProjectPath={selectedProjectPath}
						onProjectChange={setSelectedProjectPath}
					/>

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
							</ListSearchHeader>

							<SkillList
								skills={skills}
								selectedKeys={effectiveSelectedKeys}
								searchQuery={searchQuery}
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
							/>

							{isMultiSelectMode && selectedKeys.size > 0 && (
								<MultiSelectFloatingBar
									selectedCount={selectedKeys.size}
									totalCount={groupedSkills.length}
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
							/>
							<SourceListPanel
								scope={scope}
								projectPath={selectedProjectPath}
								selectedKey={selectedSourceKey}
								onSelectKey={(key) => {
									void setSelectedSourceKey(key);
									setSourceImporting(false);
								}}
								searchQuery={searchQuery}
							/>
						</>
					)}
				</div>

				{/* Right panel: detail */}
				<div className="relative flex-1 overflow-hidden">
					{view === "agent" ? (
						panelMode === "create" ? (
							<CreateSkillPanel
								onDone={() => setPanelMode(null)}
							/>
						) : panelMode === "import" ? (
							<ImportSkillPanel
								onDone={() => setPanelMode(null)}
							/>
						) : panelMode === "import-github" ? (
							<ImportGithubSkillPanel
								onDone={() => setPanelMode(null)}
							/>
						) : activeGroup ? (
							<SkillDetail
								key={
									pendingAuthSkill === activeGroup.name
										? `${activeGroup.name}-cred`
										: activeGroup.name
								}
								group={activeGroup}
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
				</div>
			</div>
		</div>
	);
}
