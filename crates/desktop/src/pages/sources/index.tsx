import {
	ArrowDownTrayIcon,
	ArrowPathIcon,
	ArrowUpCircleIcon,
	CheckCircleIcon,
	ClipboardDocumentIcon,
	ExclamationTriangleIcon,
	GlobeAltIcon,
	LockClosedIcon,
	PlusCircleIcon,
	QuestionMarkCircleIcon,
	TrashIcon,
} from "@heroicons/react/24/solid";
import { Alert, Button, Chip, Spinner, toast } from "@heroui/react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
	useMutation,
	useQueries,
	useQuery,
	useQueryClient,
} from "@tanstack/react-query";
import { useQueryState } from "nuqs";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ImportGithubSkillPanel } from "../../components/import-github-skill-panel";
import { ListSearchHeader } from "../../components/list-search-header";
import type {
	SourceSkillDiff,
	SourcesListResponse,
	SourceSummaryResponse,
} from "../../generated/dto";
import { useAgentAvailability } from "../../hooks/use-agent-availability";
import { useApi } from "../../hooks/use-api";
import { useProjects } from "../../hooks/use-projects";
import { supportsSkillMutation } from "../../lib/agent-capabilities";
import { cn } from "../../lib/utils";
import { queryKeys } from "../../requests/keys";
import { applySkillUpdateMutationOptions } from "../../requests/skills";
import {
	sourceDiffQueryOptions,
	sourcesListQueryOptions,
} from "../../requests/sources";

interface SourceRow extends SourceSummaryResponse {
	/** "global" or the project path for project-scope rows. */
	rowScope: "global" | "project";
	projectRoot?: string;
	projectName?: string;
}

const SKILL_FILE_SUFFIX_RE = /\/SKILL\.md$/;

export default function SourcesPage() {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();

	const { data: projects = [] } = useProjects();

	const [searchQuery, setSearchQuery] = useState("");
	const [selectedKey, setSelectedKey] = useQueryState("source");
	const [isImporting, setIsImporting] = useState(false);

	// Query the global scope plus each project scope.
	const sourceQueries = useQueries({
		queries: [
			sourcesListQueryOptions({ api, scope: "global" }),
			...projects.map((project) =>
				sourcesListQueryOptions({
					api,
					scope: "project",
					projectRoot: project.path,
				}),
			),
		],
	});

	const isLoadingSources = sourceQueries.some((q) => q.isLoading);

	const rows = useMemo<SourceRow[]>(() => {
		const globalResult = sourceQueries[0]?.data as
			| SourcesListResponse
			| undefined;
		const globalRows: SourceRow[] = (globalResult?.sources ?? []).map(
			(s) => ({ ...s, rowScope: "global" as const }),
		);
		const projectRows: SourceRow[] = projects.flatMap((project, index) => {
			const result = sourceQueries[index + 1]?.data as
				| SourcesListResponse
				| undefined;
			return (result?.sources ?? []).map((s) => ({
				...s,
				rowScope: "project" as const,
				projectRoot: project.path,
				projectName: project.name,
			}));
		});
		return [...globalRows, ...projectRows];
	}, [sourceQueries, projects]);

	const filteredRows = useMemo(() => {
		const q = searchQuery.trim().toLowerCase();
		if (!q) return rows;
		return rows.filter((r) => r.source.toLowerCase().includes(q));
	}, [rows, searchQuery]);

	const rowKey = (r: SourceRow) =>
		`${r.rowScope}:${r.projectRoot ?? ""}:${r.source}`;

	const activeRow = useMemo(() => {
		if (selectedKey) {
			return filteredRows.find((r) => rowKey(r) === selectedKey) ?? null;
		}
		return null;
	}, [selectedKey, filteredRows]);

	const handleSelectRow = (r: SourceRow) => {
		setSelectedKey(rowKey(r));
		setIsImporting(false);
	};

	const handleImportDone = () => {
		setIsImporting(false);
		void queryClient.invalidateQueries({
			queryKey: queryKeys.skills.sources.all(),
		});
	};

	return (
		<div className="flex h-full">
			{/* Sources List Panel */}
			<div className="relative flex w-80 shrink-0 flex-col border-r border-border">
				<ListSearchHeader
					searchValue={searchQuery}
					onSearchChange={setSearchQuery}
					placeholder={t("searchSources")}
					ariaLabel={t("searchSources")}
				/>

				<div className="min-h-0 flex-1 overflow-y-auto">
					{isLoadingSources ? (
						<div className="flex h-full items-center justify-center">
							<Spinner />
						</div>
					) : filteredRows.length === 0 ? (
						<p className="px-4 py-8 text-center text-sm text-muted">
							{t("sourcesEmpty")}
						</p>
					) : (
						<ul className="space-y-1 p-2">
							{filteredRows.map((row) => {
								const key = rowKey(row);
								const isActive = key === selectedKey;
								return (
									<li key={key}>
										<button
											type="button"
											onClick={() => handleSelectRow(row)}
											className={cn(
												"flex w-full items-start gap-2 rounded-lg p-2 text-left transition-colors hover:bg-surface-secondary",
												isActive &&
													"bg-accent/10 text-accent",
											)}
										>
											<GlobeAltIcon className="mt-0.5 size-4 shrink-0 text-muted" />
											<div className="min-w-0 flex-1">
												<div className="flex items-center gap-1.5">
													<span className="truncate font-medium text-foreground">
														{row.source}
													</span>
													{row.isPrivate && (
														<LockClosedIcon
															className="size-3 shrink-0 text-muted"
															aria-label={t(
																"privateRepo",
															)}
														/>
													)}
												</div>
												<div className="mt-1 flex flex-wrap items-center gap-1.5">
													<Chip
														size="sm"
														variant="secondary"
													>
														{row.rowScope ===
														"global"
															? t("scopeGlobal")
															: `${t(
																	"scopeProject",
																)} · ${row.projectName ?? ""}`}
													</Chip>
													<span className="text-xs text-muted">
														{row.skillCount}
													</span>
													{row.credentialStatus ===
														"missing" && (
														<Chip
															size="sm"
															color="default"
															variant="soft"
														>
															{t(
																"needsCredential",
															)}
														</Chip>
													)}
												</div>
											</div>
										</button>
									</li>
								);
							})}
						</ul>
					)}
				</div>
			</div>

			{/* Detail Panel */}
			<div className="relative flex-1 overflow-hidden">
				{activeRow ? (
					isImporting ? (
						<ImportGithubSkillPanel
							initialUrl={activeRow.sourceUrl}
							projectPath={
								activeRow.rowScope === "project"
									? activeRow.projectRoot
									: undefined
							}
							onDone={handleImportDone}
						/>
					) : (
						<SourceDetail
							row={activeRow}
							onImport={() => setIsImporting(true)}
						/>
					)
				) : (
					<div className="flex h-full flex-col items-center justify-center gap-4">
						<p className="text-sm text-muted">
							{t("selectSource")}
						</p>
					</div>
				)}
			</div>
		</div>
	);
}

interface SourceDetailProps {
	row: SourceRow;
	onImport: () => void;
}

function SourceDetail({ row, onImport }: SourceDetailProps) {
	const { t } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const { availableAgents } = useAgentAvailability();
	const [expandedSkillPath, setExpandedSkillPath] = useState<string | null>(
		null,
	);
	const [isApplyingAll, setIsApplyingAll] = useState(false);
	const [isInstallingAll, setIsInstallingAll] = useState(false);
	const [installingSkillPath, setInstallingSkillPath] = useState<
		string | null
	>(null);

	const { data, isLoading, isFetching } = useQuery(
		sourceDiffQueryOptions({
			api,
			source: row.source,
			scope: row.rowScope,
			projectRoot:
				row.rowScope === "project" ? row.projectRoot : undefined,
			enabled: true,
		}),
	);

	const grouped = useMemo(() => {
		const byState = new Map<string, SourceSkillDiff[]>();
		for (const skill of data?.skills ?? []) {
			const existing = byState.get(skill.state) ?? [];
			byState.set(skill.state, [...existing, skill]);
		}
		return byState;
	}, [data]);

	const notInstalled = grouped.get("notInstalled") ?? [];
	const outdated = grouped.get("installedOutdated") ?? [];
	const renamed = grouped.get("renamed") ?? [];
	const current = grouped.get("installedCurrent") ?? [];
	const uncheckable = grouped.get("uncheckable") ?? [];
	const hasVisibleSkills = (data?.skills.length ?? 0) > 0;
	const updateScope = row.rowScope;
	const updateProjectRoot =
		row.rowScope === "project" ? (row.projectRoot ?? null) : null;
	const shouldCheckOrphans =
		!isLoading &&
		!isFetching &&
		Boolean(data) &&
		!data?.needsCredential &&
		!hasVisibleSkills;
	const installAgentIds = useMemo(
		() =>
			availableAgents
				.filter(
					(agent) =>
						agent.isUsable &&
						supportsSkillMutation(agent, updateScope),
				)
				.map((agent) => agent.id),
		[availableAgents, updateScope],
	);

	const applyUpdateMutation = useMutation(
		applySkillUpdateMutationOptions({
			api,
			queryClient,
			onSuccess: async (data) => {
				if (!data.success) {
					toast.danger(data.error ?? t("skillUpdateApplyError"));
					return;
				}
				toast.success(t("skillSyncedSuccessfully"));
				await queryClient.invalidateQueries({
					queryKey: queryKeys.skills.sources.all(),
				});
			},
			onError: () => toast.danger(t("skillUpdateApplyError")),
		}),
	);

	const prunePreviewQuery = useQuery({
		queryKey: queryKeys.skills.pruneLock(updateScope, updateProjectRoot),
		queryFn: () =>
			api.skills.pruneLock({
				scope: updateScope,
				projectRoot: updateProjectRoot,
				confirm: false,
			}),
		enabled: shouldCheckOrphans,
	});

	const pruneLockMutation = useMutation({
		mutationFn: () =>
			api.skills.pruneLock({
				scope: updateScope,
				projectRoot: updateProjectRoot,
				confirm: true,
			}),
		onSuccess: async (result) => {
			if (result.error) {
				toast.danger(result.error);
				return;
			}

			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});

			if (result.pruned.length === 0) {
				toast.success(t("sourceOrphansCleanedZero"));
			} else {
				toast.success(
					t("sourceOrphansCleanedMany", {
						count: result.pruned.length,
					}),
				);
			}
		},
		onError: () => toast.danger(t("sourcePruneFailed")),
	});

	const deleteRenamedSkillMutation = useMutation({
		mutationFn: async (skill: SourceSkillDiff) => {
			const oldName = skill.previousName;
			if (!oldName) {
				throw new Error("Missing previous name for renamed skill.");
			}
			const results = await Promise.allSettled(
				installAgentIds.map((agentId) =>
					api.skills.delete(
						agentId,
						oldName,
						updateScope,
						updateProjectRoot ?? undefined,
					),
				),
			);
			const failures = results.filter(
				(result) => result.status === "rejected",
			);
			if (failures.length > 0) {
				throw new Error(
					(failures[0] as PromiseRejectedResult).reason instanceof
						Error
						? (failures[0] as PromiseRejectedResult).reason.message
						: t("sourceRenamedDeleteFailed", { oldName }),
				);
			}
		},
		onSuccess: async (_data, skill) => {
			if (!skill.previousName) return;
			toast.success(
				t("sourceRenamedDeleted", { oldName: skill.previousName }),
			);
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});
		},
		onError: (error, skill) => {
			if (skill.previousName) {
				toast.danger(
					error instanceof Error
						? error.message
						: t("sourceRenamedDeleteFailed", {
								oldName: skill.previousName,
							}),
				);
				return;
			}
			toast.danger(t("sourcePruneFailed"));
		},
	});

	const copyRenamedInstallMutation = useMutation({
		mutationFn: async (skill: SourceSkillDiff) => {
			await writeText(`aghub-cli install ${skill.name}`);
		},
		onSuccess: (_data, skill) => {
			toast.success(
				t("sourceRenamedInstallCommandCopied", {
					newName: skill.name,
				}),
			);
		},
		onError: () => toast.danger(t("sourceCopyCommandFailed")),
	});

	const updateRequestFor = (skill: SourceSkillDiff) => ({
		name: skill.name,
		scope: updateScope,
		projectRoot: updateProjectRoot,
		confirm: true,
	});

	const applyOneUpdate = (skill: SourceSkillDiff) => {
		applyUpdateMutation.mutate(updateRequestFor(skill));
	};

	const applyAllUpdates = async (skills: SourceSkillDiff[]) => {
		if (skills.length === 0 || isApplyingAll) return;

		setIsApplyingAll(true);
		let updated = 0;
		let failed = 0;
		try {
			for (const skill of skills) {
				try {
					const result = await api.skills.applyUpdate(
						updateRequestFor(skill),
					);
					if (result.success) {
						updated += 1;
					} else {
						failed += 1;
					}
				} catch {
					failed += 1;
				}
			}

			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.sources.all(),
			});

			if (failed > 0) {
				toast.danger(
					failed === 1
						? t("sourceUpdateSomeFailedOne", { count: failed })
						: t("sourceUpdateSomeFailedMany", { count: failed }),
				);
			} else {
				toast.success(t("sourceUpdatesApplied", { count: updated }));
			}
		} finally {
			setIsApplyingAll(false);
		}
	};

	const installPathFor = (skill: SourceSkillDiff) =>
		skill.skillPath === "SKILL.md"
			? "."
			: skill.skillPath.replace(SKILL_FILE_SUFFIX_RE, "");

	const installFromSource = async (skills: SourceSkillDiff[]) => {
		if (
			skills.length === 0 ||
			isInstallingAll ||
			installingSkillPath !== null
		) {
			return;
		}
		if (installAgentIds.length === 0) {
			toast.danger(t("sourceInstallNoAgents"));
			return;
		}

		const installAll = skills.length > 1;
		if (installAll) {
			setIsInstallingAll(true);
		} else {
			setInstallingSkillPath(skills[0]?.skillPath ?? null);
		}

		try {
			const scan = await api.skills.gitScan({
				url: row.sourceUrl,
				credential_id: null,
				branch: null,
				session_id: null,
			});
			const wantedPaths = new Set(skills.map(installPathFor));
			const scanPaths = new Set(scan.skills.map((skill) => skill.path));
			const skillPaths = Array.from(wantedPaths).filter((path) =>
				scanPaths.has(path),
			);

			if (skillPaths.length !== wantedPaths.size) {
				throw new Error(t("sourceInstallFailed"));
			}

			const result = await api.skills.gitInstall({
				session_id: scan.session_id,
				skill_paths: skillPaths,
				agents: installAgentIds,
				scope: updateScope,
				project_root: updateProjectRoot,
				universal: true,
			});
			const failed = result.results.filter((entry) => !entry.success);

			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.all(),
			});
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.sources.all(),
			});

			if (failed.length > 0) {
				toast.danger(
					failed.length === 1
						? t("sourceInstallSomeFailedOne", {
								count: failed.length,
							})
						: t("sourceInstallSomeFailedMany", {
								count: failed.length,
							}),
				);
			} else {
				toast.success(
					t("sourceInstalled", { count: skillPaths.length }),
				);
			}
		} catch (error) {
			toast.danger(
				error instanceof Error
					? error.message
					: t("sourceInstallFailed"),
			);
		} finally {
			setIsInstallingAll(false);
			setInstallingSkillPath(null);
		}
	};

	return (
		<div className="flex h-full flex-col overflow-hidden">
			{/* Header */}
			<div className="flex items-start justify-between gap-3 border-b border-border p-4">
				<div className="min-w-0">
					<div className="flex items-center gap-2">
						<GlobeAltIcon className="size-5 shrink-0 text-muted" />
						<h1 className="truncate text-lg font-semibold text-foreground">
							{row.source}
						</h1>
						<Chip size="sm" variant="secondary">
							{row.rowScope === "global"
								? t("scopeGlobal")
								: `${t("scopeProject")} · ${row.projectName ?? ""}`}
						</Chip>
					</div>
					<p className="mt-1 truncate font-mono text-xs text-muted">
						{row.sourceUrl}
					</p>
				</div>
				<Button className="shrink-0" onPress={onImport}>
					<ArrowDownTrayIcon className="size-4" />
					{t("importFromThisSource")}
				</Button>
			</div>

			{/* Body */}
			<div className="min-h-0 flex-1 overflow-y-auto p-4">
				{isLoading || isFetching ? (
					<div className="flex flex-col items-center gap-3 py-12">
						<Spinner size="lg" />
						<p className="text-sm text-muted">
							{t("checkingSource")}
						</p>
					</div>
				) : data?.needsCredential ? (
					<Alert status="warning">
						<Alert.Indicator />
						<Alert.Content>
							<Alert.Title>{t("needsCredential")}</Alert.Title>
							<Alert.Description>
								{t("needsCredentialHint")}
							</Alert.Description>
						</Alert.Content>
					</Alert>
				) : (
					<div className="space-y-6">
						{!hasVisibleSkills && (
							<SourceEmptyState
								prunedCount={
									prunePreviewQuery.data?.pruned.length ?? 0
								}
								isChecking={prunePreviewQuery.isFetching}
								isCleaning={pruneLockMutation.isPending}
								hasError={prunePreviewQuery.isError}
								onClean={() => pruneLockMutation.mutate()}
								onRetry={() => prunePreviewQuery.refetch()}
							/>
						)}
						<SkillSection
							title={t("sourceStateNotInstalled")}
							icon={
								<PlusCircleIcon className="size-4 text-accent" />
							}
							skills={notInstalled}
							expandedSkillPath={expandedSkillPath}
							onToggleSkill={setExpandedSkillPath}
							sectionAction={
								<Button
									size="sm"
									variant="ghost"
									className="h-7 px-2 text-xs"
									isDisabled={
										isInstallingAll ||
										installingSkillPath !== null
									}
									onPress={() =>
										installFromSource(notInstalled)
									}
								>
									<ArrowDownTrayIcon className="size-3.5" />
									{isInstallingAll
										? t("sourceInstalling")
										: t("sourceInstallAll")}
								</Button>
							}
							rowAction={(skill) => {
								const isInstalling =
									installingSkillPath === skill.skillPath;
								return (
									<Button
										size="sm"
										variant="secondary"
										className="h-7 px-2 text-xs"
										isDisabled={
											isInstallingAll ||
											installingSkillPath !== null
										}
										onPress={() =>
											installFromSource([skill])
										}
									>
										<ArrowDownTrayIcon className="size-3.5" />
										{isInstalling
											? t("sourceInstalling")
											: t("sourceInstallSkill")}
									</Button>
								);
							}}
						/>
						<SkillSection
							title={t("sourceStateOutdated")}
							icon={
								<ArrowUpCircleIcon className="size-4 text-warning" />
							}
							skills={outdated}
							expandedSkillPath={expandedSkillPath}
							onToggleSkill={setExpandedSkillPath}
							sectionAction={
								<Button
									size="sm"
									variant="ghost"
									className="h-7 px-2 text-xs"
									isDisabled={
										isApplyingAll ||
										applyUpdateMutation.isPending
									}
									onPress={() => applyAllUpdates(outdated)}
								>
									<ArrowPathIcon className="size-3.5" />
									{isApplyingAll
										? t("sourceUpdating")
										: t("sourceUpdateAll")}
								</Button>
							}
							rowAction={(skill) => {
								const isApplying =
									applyUpdateMutation.isPending &&
									applyUpdateMutation.variables?.name ===
										skill.name;
								return (
									<Button
										size="sm"
										variant="secondary"
										className="h-7 px-2 text-xs"
										isDisabled={isApplyingAll || isApplying}
										onPress={() => applyOneUpdate(skill)}
									>
										<ArrowPathIcon className="size-3.5" />
										{isApplying
											? t("sourceUpdating")
											: t("sourceUpdateSkill")}
									</Button>
								);
							}}
						/>
						<SkillSection
							title={t("sourceStateRenamed")}
							icon={
								<ExclamationTriangleIcon className="size-4 text-warning" />
							}
							skills={renamed}
							expandedSkillPath={expandedSkillPath}
							onToggleSkill={setExpandedSkillPath}
							rowAction={(skill) => {
								const isDeleting =
									deleteRenamedSkillMutation.isPending &&
									deleteRenamedSkillMutation.variables
										?.skillPath === skill.skillPath;
								const isCopying =
									copyRenamedInstallMutation.isPending &&
									copyRenamedInstallMutation.variables
										?.skillPath === skill.skillPath;
								const rowBusy = isDeleting || isCopying;
								return (
									<div className="flex items-center gap-1.5">
										<Button
											size="sm"
											variant="secondary"
											className="h-7 px-2 text-xs"
											isDisabled={
												!skill.previousName || rowBusy
											}
											onPress={() =>
												deleteRenamedSkillMutation.mutate(
													skill,
												)
											}
										>
											<TrashIcon className="size-3.5" />
											{isDeleting
												? t("sourceRenamedDeleting")
												: t("sourceRenamedDeleteOld")}
										</Button>
										<Button
											size="sm"
											variant="ghost"
											className="h-7 px-2 text-xs"
											isDisabled={rowBusy}
											onPress={() =>
												copyRenamedInstallMutation.mutate(
													skill,
												)
											}
										>
											<ClipboardDocumentIcon className="size-3.5" />
											{isCopying
												? t("sourceRenamedCopying")
												: t("sourceRenamedCopyInstall")}
										</Button>
									</div>
								);
							}}
						/>
						<SkillSection
							title={t("sourceStateCurrent")}
							icon={
								<CheckCircleIcon className="size-4 text-success" />
							}
							skills={current}
							expandedSkillPath={expandedSkillPath}
							onToggleSkill={setExpandedSkillPath}
						/>
						{uncheckable.length > 0 && (
							<SkillSection
								title={t("sourceStateUncheckable")}
								icon={
									<QuestionMarkCircleIcon className="size-4 text-muted" />
								}
								skills={uncheckable}
								expandedSkillPath={expandedSkillPath}
								onToggleSkill={setExpandedSkillPath}
								muted
								showReason
							/>
						)}
					</div>
				)}
			</div>
		</div>
	);
}

interface SourceEmptyStateProps {
	prunedCount: number;
	isChecking: boolean;
	isCleaning: boolean;
	hasError: boolean;
	onClean: () => void;
	onRetry: () => void;
}

function SourceEmptyState({
	prunedCount,
	isChecking,
	isCleaning,
	hasError,
	onClean,
	onRetry,
}: SourceEmptyStateProps) {
	const { t } = useTranslation();
	const hasOrphans = prunedCount > 0;

	if (hasError) {
		return (
			<Alert status="danger">
				<Alert.Indicator />
				<Alert.Content>
					<Alert.Title>
						{t("sourcePrunePreviewErrorTitle")}
					</Alert.Title>
					<Alert.Description>
						{t("sourcePrunePreviewErrorHint")}
					</Alert.Description>
					<div className="mt-3">
						<Button size="sm" variant="secondary" onPress={onRetry}>
							{t("retry")}
						</Button>
					</div>
				</Alert.Content>
			</Alert>
		);
	}

	return (
		<Alert status="warning">
			<Alert.Indicator />
			<Alert.Content>
				<Alert.Title>
					{hasOrphans
						? t("sourceOrphanTitle")
						: t("sourceEmptyDiffTitle")}
				</Alert.Title>
				<Alert.Description>
					{isChecking
						? t("sourceCheckingOrphans")
						: hasOrphans
							? prunedCount === 1
								? t("sourceOrphanHintOne", {
										count: prunedCount,
									})
								: t("sourceOrphanHintMany", {
										count: prunedCount,
									})
							: t("sourceEmptyDiffHint")}
				</Alert.Description>
				{hasOrphans && (
					<div className="mt-3">
						<Button
							size="sm"
							variant="secondary"
							isDisabled={isChecking || isCleaning}
							onPress={onClean}
						>
							<TrashIcon className="size-3.5" />
							{isCleaning
								? t("sourceCleaningOrphans")
								: t("sourceCleanOrphans")}
						</Button>
					</div>
				)}
			</Alert.Content>
		</Alert>
	);
}

interface SkillSectionProps {
	title: string;
	icon: React.ReactNode;
	skills: SourceSkillDiff[];
	expandedSkillPath: string | null;
	onToggleSkill: (skillPath: string | null) => void;
	sectionAction?: React.ReactNode;
	rowAction?: (skill: SourceSkillDiff) => React.ReactNode;
	muted?: boolean;
	showReason?: boolean;
}

function SkillSection({
	title,
	icon,
	skills,
	expandedSkillPath,
	onToggleSkill,
	sectionAction,
	rowAction,
	muted = false,
	showReason = false,
}: SkillSectionProps) {
	if (skills.length === 0) return null;

	return (
		<section>
			<div className="mb-2 flex items-center justify-between gap-3">
				<div className="flex min-w-0 items-center gap-2">
					{icon}
					<h2
						className={cn(
							"truncate text-sm font-semibold",
							muted ? "text-muted" : "text-foreground",
						)}
					>
						{title}
					</h2>
					<span className="shrink-0 text-xs text-muted">
						{skills.length}
					</span>
				</div>
				{sectionAction}
			</div>
			<ul className="overflow-hidden rounded-lg border border-border">
				{skills.map((skill) => (
					<SkillRow
						key={skill.skillPath}
						skill={skill}
						isExpanded={expandedSkillPath === skill.skillPath}
						onToggle={() =>
							onToggleSkill(
								expandedSkillPath === skill.skillPath
									? null
									: skill.skillPath,
							)
						}
						muted={muted}
						showReason={showReason}
						action={rowAction?.(skill)}
					/>
				))}
			</ul>
		</section>
	);
}

interface SkillRowProps {
	skill: SourceSkillDiff;
	isExpanded: boolean;
	onToggle: () => void;
	action?: React.ReactNode;
	muted?: boolean;
	showReason?: boolean;
}

function SkillRow({
	skill,
	isExpanded,
	onToggle,
	action,
	muted = false,
	showReason = false,
}: SkillRowProps) {
	const { t } = useTranslation();
	const detailText = skill.description || skill.skillPath;

	return (
		<li className="flex items-center gap-3 border-b border-border px-3 py-2.5 last:border-b-0 hover:bg-surface-secondary/70">
			<button
				type="button"
				className="min-w-0 flex-1 text-left"
				aria-expanded={isExpanded}
				onClick={onToggle}
			>
				<div className="flex min-w-0 items-center gap-2">
					<span
						className={cn(
							"truncate text-sm font-medium",
							muted ? "text-muted" : "text-foreground",
						)}
					>
						{skill.name}
					</span>
					{skill.version && (
						<Chip size="sm" variant="secondary">
							v{skill.version}
						</Chip>
					)}
					<span className="truncate font-mono text-[11px] text-muted/80">
						{skill.skillPath}
					</span>
				</div>
				{detailText && (
					<p
						className={cn(
							"mt-0.5 text-xs leading-5 text-muted",
							!isExpanded && "line-clamp-1",
						)}
					>
						{detailText}
					</p>
				)}
				{showReason && skill.reason && (
					<p className="mt-0.5 text-xs text-muted">{skill.reason}</p>
				)}
				{skill.state === "renamed" && skill.previousName && (
					<p className="mt-0.5 text-xs text-warning">
						{t("sourceRenamedHint", {
							oldName: skill.previousName,
							newName: skill.name,
						})}
					</p>
				)}
			</button>
			{action && <div className="shrink-0">{action}</div>}
		</li>
	);
}
