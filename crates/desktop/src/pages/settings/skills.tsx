import {
	ArrowPathIcon,
	CheckCircleIcon,
	PlusIcon,
	RectangleStackIcon,
} from "@heroicons/react/24/solid";
import { Button, Dropdown, Tooltip, toast } from "@heroui/react";
import {
	useMutation,
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
import type { SkillResponse, SkillUpdateResponse } from "../../generated/dto";
import { useApi } from "../../hooks/use-api";
import { cn } from "../../lib/utils";
import {
	checkSkillUpdatesMutationOptions,
	checkSkillUpdatesQueryOptions,
	skillListQueryOptions,
} from "../../requests/skills";

export default function SkillsPage() {
	const { t, i18n } = useTranslation();
	const api = useApi();
	const queryClient = useQueryClient();
	const updateCheckParams = useMemo(() => ({ scope: "global" as const }), []);
	const {
		data: skills,
		refetch,
		isFetching,
	} = useSuspenseQuery({
		...skillListQueryOptions({ api, scope: "global" }),
	});

	// Auto-check: fires on mount if data is stale (>10 min) and online.
	// navigator.onLine suppresses the check when offline to avoid turning
	// every skill into uncheckable(network). Toast is silent for auto-check.
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

	// Derive lastCheckedDate from TanStack Query v5's dataUpdatedAt (ms since
	// epoch, 0 when never fetched). No useState needed — pure derivation.
	const lastCheckedDate = useMemo(
		() => (checksUpdatedAt > 0 ? new Date(checksUpdatedAt) : null),
		[checksUpdatedAt],
	);

	// Manual refresh mutation — keeps explicit isPending for the button spinner.
	// Toast fires here (manual only); auto-check is silent per spec §4.3.
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
					// Some skills could not be checked (auth/network/local) —
					// do NOT claim "all good" when we cannot verify them all.
					toast.warning(
						t("skillCheckCompleteSomeUncheckable", {
							count: uncheckableCount,
						}),
					);
				} else {
					// Every skill was reachable and is up to date.
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
	const [searchQuery, setSearchQuery] = useState("");
	const [selectedName, setSelectedName] = useQueryState("skill");
	const [selectedKeys, setSelectedKeys] = useState<Set<string>>(
		() => new Set(),
	);
	const [isBulkDeleteDialogOpen, setIsBulkDeleteDialogOpen] = useState(false);
	const [isMultiSelectMode, setIsMultiSelectMode] = useState(false);
	// pendingAuthSkill: when set, the SkillDetail for this skill will open the
	// credential dialog as soon as it renders. Cleared by SkillDetail itself.
	const [pendingAuthSkill, setPendingAuthSkill] = useState<string | null>(
		null,
	);

	const [panelMode, setPanelMode] = useState<
		"create" | "import" | "import-github" | null
	>(null);
	const isRefreshingSkills =
		isFetching || checkUpdatesMutation.isPending || isAutoChecking;

	// Per Codex correction: do NOT invalidateQueries here — the mutation's
	// onSuccess writes the shared cache (checkSkillUpdatesMutationOptions line
	// ~361). Double-invalidating would fire two network checks.
	// Early-return guard: prevents re-entrant duplicate checks + toasts when
	// the user clicks "recheck" while a check (auto or manual) is already
	// running (Codex CR P1).
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
		if (selectedName) {
			return groupedSkills.find((g) => g.name === selectedName) ?? null;
		}
		return groupedSkills[0] ?? null;
	}, [selectedName, groupedSkills]);

	// 多选模式下被选中的所有 groups（用于批量删除）
	const selectedGroups = useMemo(() => {
		return groupedSkills.filter((g) => selectedKeys.has(g.name));
	}, [selectedKeys, groupedSkills]);

	// ListBox 高亮用的 keys
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
			setSelectedName(clickedKey);
		}

		if (keys.size > 1 && !isMultiSelectMode) {
			setIsMultiSelectMode(true);
		}
		if (keys.size === 0 && isMultiSelectMode) {
			setIsMultiSelectMode(false);
		}
		setPanelMode(null);
	};

	const handleCreateSkill = () => {
		setSelectedKeys(new Set());
		setSelectedName(null);
		setPanelMode("create");
	};

	const handleImportSkill = () => {
		setSelectedKeys(new Set());
		setSelectedName(null);
		setPanelMode("import");
	};

	return (
		<div className="flex h-full">
			{/* Skills List Panel */}
			<div className="relative flex w-80 shrink-0 flex-col border-r border-border">
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
									setIsMultiSelectMode((prev) => !prev);
									if (isMultiSelectMode) {
										handleSelectionChange(new Set());
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
									setIsMultiSelectMode((prev) => !prev);
									if (isMultiSelectMode) {
										handleSelectionChange(new Set());
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
										handleCreateSkill();
									} else if (key === "import") {
										handleImportSkill();
									} else if (key === "import-github") {
										setSelectedKeys(new Set());
										setSelectedName(null);
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
					<Tooltip delay={0}>
						<Button
							isIconOnly
							variant="ghost"
							size="sm"
							className="shrink-0"
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
				</ListSearchHeader>

				{/* Last-checked timestamp row */}
				<div className="flex items-center justify-between border-b border-separator px-3 py-1.5">
					<span className="text-xs text-muted">
						{lastCheckedDate
							? t("lastCheckedAgo", {
									time: formatRelativeTime(lastCheckedDate),
								})
							: t("lastCheckedNever")}
					</span>
					{!isRefreshingSkills && (
						<button
							type="button"
							className="text-xs text-accent hover:underline"
							onClick={() => {
								void handleRefreshSkills();
							}}
						>
							{t("recheck")}
						</button>
					)}
				</div>

				{/* Skills List */}
				<SkillList
					skills={skills}
					selectedKeys={effectiveSelectedKeys}
					searchQuery={searchQuery}
					onSelectionChange={handleSelectionChange}
					selectionMode="multiple"
					isMultiSelectMode={isMultiSelectMode}
					groupBySource={true}
					updateStatuses={updateStatuses}
					onResolveAuth={(skillName: string) => {
						// Select the skill to open the detail panel, and
						// signal it to open the credential dialog immediately.
						void setSelectedName(skillName);
						setPendingAuthSkill(skillName);
					}}
				/>

				{isMultiSelectMode && selectedKeys.size > 0 && (
					<MultiSelectFloatingBar
						selectedCount={selectedKeys.size}
						totalCount={groupedSkills.length}
						onDelete={() => setIsBulkDeleteDialogOpen(true)}
					/>
				)}
			</div>

			<div className="flex-1 overflow-hidden relative">
				{panelMode === "create" ? (
					<CreateSkillPanel onDone={() => setPanelMode(null)} />
				) : panelMode === "import" ? (
					<ImportSkillPanel onDone={() => setPanelMode(null)} />
				) : panelMode === "import-github" ? (
					<ImportGithubSkillPanel onDone={() => setPanelMode(null)} />
				) : activeGroup ? (
					<SkillDetail
						key={
							pendingAuthSkill === activeGroup.name
								? `${activeGroup.name}-cred`
								: activeGroup.name
						}
						group={activeGroup}
						openCredDialog={pendingAuthSkill === activeGroup.name}
						onCredDialogClose={() => {
							setPendingAuthSkill(null);
						}}
					/>
				) : (
					<div className="flex h-full flex-col items-center justify-center gap-4">
						<div className="text-center">
							<p className="mb-2 text-sm text-muted">
								{t("selectSkill")}
							</p>
						</div>
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
						refetch();
					}}
					resourceType="skill"
				/>
			</div>
		</div>
	);
}
