import {
	ArrowDownTrayIcon,
	ArrowUpCircleIcon,
	CheckCircleIcon,
	GlobeAltIcon,
	LockClosedIcon,
	PlusCircleIcon,
	QuestionMarkCircleIcon,
} from "@heroicons/react/24/solid";
import { Alert, Button, Chip, Spinner } from "@heroui/react";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
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
import { useApi } from "../../hooks/use-api";
import { useProjects } from "../../hooks/use-projects";
import { cn } from "../../lib/utils";
import { queryKeys } from "../../requests/keys";
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
	const current = grouped.get("installedCurrent") ?? [];
	const uncheckable = grouped.get("uncheckable") ?? [];

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
						<SkillSection
							title={t("sourceStateNotInstalled")}
							icon={
								<PlusCircleIcon className="size-4 text-accent" />
							}
							skills={notInstalled}
						/>
						<SkillSection
							title={t("sourceStateOutdated")}
							icon={
								<ArrowUpCircleIcon className="size-4 text-warning" />
							}
							skills={outdated}
						/>
						<SkillSection
							title={t("sourceStateCurrent")}
							icon={
								<CheckCircleIcon className="size-4 text-success" />
							}
							skills={current}
						/>
						{uncheckable.length > 0 && (
							<SkillSection
								title={t("sourceStateUncheckable")}
								icon={
									<QuestionMarkCircleIcon className="size-4 text-muted" />
								}
								skills={uncheckable}
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

interface SkillSectionProps {
	title: string;
	icon: React.ReactNode;
	skills: SourceSkillDiff[];
	muted?: boolean;
	showReason?: boolean;
}

function SkillSection({
	title,
	icon,
	skills,
	muted = false,
	showReason = false,
}: SkillSectionProps) {
	if (skills.length === 0) return null;

	return (
		<section>
			<div className="mb-2 flex items-center gap-2">
				{icon}
				<h2
					className={cn(
						"text-sm font-semibold",
						muted ? "text-muted" : "text-foreground",
					)}
				>
					{title}
				</h2>
				<span className="text-xs text-muted">{skills.length}</span>
			</div>
			<ul className="space-y-2">
				{skills.map((skill) => (
					<li
						key={skill.skillPath}
						className="rounded-lg border border-border p-3"
					>
						<div className="flex flex-wrap items-center gap-2">
							<span className="font-medium text-foreground">
								{skill.name}
							</span>
							{skill.version && (
								<Chip size="sm" variant="secondary">
									v{skill.version}
								</Chip>
							)}
						</div>
						{skill.description && (
							<p className="mt-1 text-sm text-muted">
								{skill.description}
							</p>
						)}
						{showReason && skill.reason && (
							<p className="mt-1 text-xs text-muted">
								{skill.reason}
							</p>
						)}
					</li>
				))}
			</ul>
		</section>
	);
}
