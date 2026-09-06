import {
	ArrowTopRightOnSquareIcon,
	ChevronDownIcon,
	ChevronRightIcon,
	StarIcon as StarIconSolid,
	UsersIcon,
} from "@heroicons/react/24/solid";
import { Button, Chip, Label, ListBox, Spinner, Tooltip } from "@heroui/react";
import { useQuery } from "@tanstack/react-query";
import Fuse from "fuse.js";
import { useMemo, useState } from "react";
import { useMultiSelect } from "../hooks/use-multi-select";
import { useTranslation } from "react-i18next";
import type { SkillResponse, SkillUpdateResponse } from "../generated/dto";
import { SkillStatusBadge } from "./skill-update-badge";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { useApi } from "../hooks/use-api";
import { useFavorites } from "../hooks/use-favorites";
import { useSkillTags } from "../hooks/use-skill-tags";
import { filterGroupsByAgent } from "../lib/skill-agent-filter";
import {
	isGroupExpanded,
	toggleGroupExpansion,
} from "../lib/skill-group-expansion";
import {
	sharedUncheckableReason,
	uncheckableTooltipKey,
} from "../lib/skill-group-status";
import { matchesTagFilter } from "../lib/skill-tags";
import { cn, filterItemsByAgentIds } from "../lib/utils";
import {
	globalSkillLockQueryOptions,
	projectSkillLockQueryOptions,
} from "../requests/skills";

/** Sentinel key for the "no lock record" bucket — never a real lock source
 * id, so it can share the same collapse-state Set and never collides with
 * one. */
const UNRECORDED_GROUP_KEY = "__unrecorded__";

interface SkillGroup {
	name: string;
	items: SkillResponse[];
	description: string;
}

export interface SourceGroup {
	source: string;
	sourceType: string;
	skills: SkillGroup[];
}

interface SkillListProps {
	skills: SkillResponse[];
	selectedKeys: Set<string>;
	searchQuery: string;
	onSelectionChange: (keys: Set<string>, clickedKey?: string) => void;
	emptyMessage?: string;
	groupBySource?: boolean;
	projectPath?: string;
	selectionMode?: "none" | "single" | "multiple";
	isMultiSelectMode?: boolean;
	updateStatuses?: ReadonlyMap<string, SkillUpdateResponse>;
	/** Passed to SkillStatusBadge so auth-uncheckable skills are actionable
	 * in the list (not just in the detail panel). */
	onResolveAuth?: (skillName: string) => void;
	/** When set, source group headers show a button to bulk-manage the agents
	 * of every skill in that group. */
	onManageGroupAgents?: (group: SourceGroup) => void;
	/** When set, source group headers show a button that opens the Sources
	 * view for that source, where uninstalled skills can be installed. */
	onOpenSourceView?: (source: string) => void;
	/**
	 * The lock source id (`sg.source`, NOT a clone URL) of the group the page
	 * currently has open in the right panel. The caller resolves the URL's
	 * `source` param to a row first — comparing it against `sg.source`
	 * directly would silently never match, since a clone URL and a lock id
	 * are different strings for the same source.
	 */
	selectedSource?: string | null;
	/** Local tag filter (AND). Empty/absent shows everything. */
	tagFilter?: ReadonlySet<string>;
	/**
	 * Show only skills this agent reads. `null`/absent shows everything.
	 *
	 * Filters WHICH GROUPS are listed, never what a group CONTAINS: the
	 * members are what tells the detail panel and the manage-agents dialog
	 * which agents already hold the skill, and a filtered-down `items` would
	 * make a skill installed for 21 agents look like it belongs to one.
	 */
	agentFilter?: string | null;
}

export function SkillList({
	skills,
	selectedKeys,
	searchQuery,
	onSelectionChange,
	emptyMessage,
	groupBySource = false,
	projectPath,
	selectionMode = "single",
	isMultiSelectMode = false,
	updateStatuses,
	onResolveAuth,
	onManageGroupAgents,
	onOpenSourceView,
	selectedSource = null,
	tagFilter,
	agentFilter = null,
}: SkillListProps) {
	const { t } = useTranslation();
	const api = useApi();
	const { availableAgents } = useAgentAvailability();
	const effectiveScope = groupBySource
		? projectPath
			? "project"
			: "global"
		: null;
	const enabledAgentIds = useMemo(
		() =>
			new Set(
				availableAgents
					.filter((agent) => agent.isUsable)
					.map((agent) => agent.id),
			),
		[availableAgents],
	);
	const visibleSkills = useMemo(
		() => filterItemsByAgentIds(skills, enabledAgentIds),
		[skills, enabledAgentIds],
	);

	const { data: globalLock, isLoading: isLoadingGlobalLock } = useQuery({
		...globalSkillLockQueryOptions({
			api,
			enabled: effectiveScope === "global",
		}),
	});

	const { data: projectLock, isLoading: isLoadingProjectLock } = useQuery({
		...projectSkillLockQueryOptions({
			api,
			projectPath,
			enabled: effectiveScope === "project" && Boolean(projectPath),
		}),
	});

	const isGroupingLoading =
		groupBySource &&
		((effectiveScope === "global" && isLoadingGlobalLock) ||
			(effectiveScope === "project" && isLoadingProjectLock));

	const groupedByName = useMemo(() => {
		const map = new Map<string, SkillResponse[]>();
		for (const skill of visibleSkills) {
			const existing = map.get(skill.name) ?? [];
			map.set(skill.name, [...existing, skill]);
		}
		return Array.from(map.entries()).map(([name, items]) => ({
			name,
			items,
			description: items.find((s) => s.description)?.description ?? "",
		}));
	}, [visibleSkills]);

	const fuse = useMemo(
		() =>
			new Fuse(groupedByName, {
				keys: [
					{ name: "name", weight: 2 },
					{ name: "description", weight: 1 },
				],
				threshold: 0.4,
				includeScore: true,
			}),
		[groupedByName],
	);

	const { isSkillStarred } = useFavorites();
	const { tagsFor } = useSkillTags();

	const filteredByName = useMemo(() => {
		let items;
		if (!searchQuery) items = groupedByName;
		else items = fuse.search(searchQuery).map((result) => result.item);

		items = filterGroupsByAgent(items, agentFilter);

		if (tagFilter && tagFilter.size > 0) {
			items = items.filter((group) =>
				matchesTagFilter(tagsFor(group.name), tagFilter),
			);
		}

		return [...items].sort((a, b) => {
			const aStarred = isSkillStarred(a.name);
			const bStarred = isSkillStarred(b.name);
			if (aStarred && !bStarred) return -1;
			if (!aStarred && bStarred) return 1;
			return 0;
		});
	}, [
		fuse,
		groupedByName,
		searchQuery,
		isSkillStarred,
		tagFilter,
		tagsFor,
		agentFilter,
	]);

	// Every source is its own group — including a source with only one
	// skill. A skill the lock has no entry for at all (never installed
	// through aghub/npx) is the ONE case that still gets collected into a
	// single bucket, since there is no source identity to group it by.
	const { sourceGroups, unrecordedGroup } = useMemo(() => {
		const findSkillSource = (
			skillName: string,
		): { source: string; sourceType: string } | null => {
			const relevantEntries =
				effectiveScope === "project"
					? projectLock?.skills
					: globalLock?.skills;
			const entry = relevantEntries?.find((s) => s.name === skillName);
			if (entry) {
				return {
					source: entry.source,
					sourceType: entry.sourceType,
				};
			}
			return null;
		};

		if (!groupBySource) {
			return { sourceGroups: [] as SourceGroup[], unrecordedGroup: null };
		}

		const groups = new Map<string, SourceGroup>();
		const unrecorded: SkillGroup[] = [];

		for (const group of filteredByName) {
			const sourceInfo = findSkillSource(group.name);
			if (sourceInfo) {
				const existing = groups.get(sourceInfo.source);
				if (existing) {
					existing.skills.push(group);
				} else {
					groups.set(sourceInfo.source, {
						source: sourceInfo.source,
						sourceType: sourceInfo.sourceType,
						skills: [group],
					});
				}
			} else {
				unrecorded.push(group);
			}
		}

		const starThenName = (a: SkillGroup, b: SkillGroup) => {
			const aStarred = isSkillStarred(a.name);
			const bStarred = isSkillStarred(b.name);
			if (aStarred && !bStarred) return -1;
			if (!aStarred && bStarred) return 1;
			return a.name.localeCompare(b.name);
		};

		const sortedSourceGroups = [...groups.values()]
			.map((sg) => ({
				...sg,
				skills: [...sg.skills].sort(starThenName),
			}))
			.sort((a, b) => a.source.localeCompare(b.source));

		const sortedUnrecorded = [...unrecorded].sort(starThenName);

		return {
			sourceGroups: sortedSourceGroups,
			unrecordedGroup:
				sortedUnrecorded.length > 0 ? sortedUnrecorded : null,
		};
	}, [
		filteredByName,
		groupBySource,
		globalLock,
		effectiveScope,
		projectLock,
		isSkillStarred,
	]);

	// Only what the user explicitly collapsed or reopened; every other group
	// follows the "open" default. See `lib/skill-group-expansion.ts` for why
	// this is NOT seeded from `sourceGroups`.
	const [expandOverrides, setExpandOverrides] = useState<
		ReadonlyMap<string, boolean>
	>(() => new Map());

	const toggleSource = (key: string) => {
		setExpandOverrides((prev) => toggleGroupExpansion(prev, key));
	};

	const { createSelectionHandler, createGroupedSelectionHandler } =
		useMultiSelect({
			selectedKeys,
			onSelectionChange,
			isMultiSelectMode,
		});

	// Helper to render a skill item. `suppressStatusBadge` is set when the
	// group this row belongs to already said the shared reason once on its
	// header — repeating a badge on every row below it would be noise.
	const renderSkillItem = (
		skillGroup: SkillGroup,
		opts: { suppressStatusBadge: boolean },
	) => (
		<ListBox.Item
			key={skillGroup.name}
			id={skillGroup.name}
			textValue={skillGroup.name}
			className="data-selected:bg-surface"
		>
			<div className="flex w-full items-center gap-2">
				{/* Fixed-width slot regardless of starred state — an absent
				    icon must not shift the title flush left while a starred
				    row's title sits one icon+gap further right. */}
				<span className="flex size-3.5 shrink-0 items-center justify-center">
					{isSkillStarred(skillGroup.name) && (
						<StarIconSolid className="size-3.5 text-warning" />
					)}
				</span>
				<Label className="flex-1 truncate">{skillGroup.name}</Label>
				<SkillTagChips tags={tagsFor(skillGroup.name)} />
				{!opts.suppressStatusBadge && (
					<SkillStatusBadge
						status={updateStatuses?.get(skillGroup.name)}
						onResolveAuth={
							onResolveAuth
								? () => onResolveAuth(skillGroup.name)
								: undefined
						}
					/>
				)}
				{!agentFilter && (
					<span
						className="shrink-0 text-xs text-muted tabular-nums"
						title={t("skillInstalledAgentCount", {
							count: skillGroup.items.length,
						})}
					>
						{skillGroup.items.length}
					</span>
				)}
			</div>
		</ListBox.Item>
	);

	// One group block: a header (collapse chevron, name/count that opens the
	// source panel, shared-uncheckable rollup, ↗ and Users actions) plus its
	// skill list. Shared by every real source group AND the "no lock record"
	// bucket — `sourceIdForActions: null` is what turns off the source-only
	// affordances (opening a panel, bulk-managing agents) for the latter,
	// since there is no real source behind it.
	const renderGroupBlock = ({
		groupKey,
		title,
		sourceIdForActions,
		skills: groupSkills,
		sourceGroupForManage,
	}: {
		groupKey: string;
		title: string;
		sourceIdForActions: string | null;
		skills: SkillGroup[];
		sourceGroupForManage?: SourceGroup;
	}) => {
		const isOpen = isGroupExpanded(expandOverrides, groupKey);
		const groupKeySet = new Set(groupSkills.map((s) => s.name));
		// Only THIS group's keys. Handing a ListBox the global set
		// makes React Aria echo back the other groups' names on every
		// toggle, and the handler then reads one of THOSE as the row
		// that was clicked — so clicking a row in one group selects a
		// row in another. Same shape as `plugin-list.tsx`.
		const groupSelectedKeys = new Set(
			[...selectedKeys].filter((k) => groupKeySet.has(k)),
		);
		const isSelected =
			sourceIdForActions !== null &&
			sourceIdForActions === selectedSource;
		const names = groupSkills.map((s) => s.name);
		const shared = updateStatuses
			? sharedUncheckableReason(names, updateStatuses)
			: ({ kind: "none" } as const);
		const canOpen =
			Boolean(onOpenSourceView) && sourceIdForActions !== null;

		return (
			<div key={groupKey} className="border-y border-separator">
				<div className="flex w-full items-center gap-1 px-3 py-2 transition-colors hover:bg-surface-secondary">
					<button
						type="button"
						onClick={() => toggleSource(groupKey)}
						aria-expanded={isOpen}
						aria-label={
							isOpen
								? t("collapseSourceGroup")
								: t("expandSourceGroup")
						}
						className="flex size-6 shrink-0 items-center justify-center rounded-md text-muted hover:bg-surface hover:text-foreground"
					>
						{isOpen ? (
							<ChevronDownIcon className="size-4" />
						) : (
							<ChevronRightIcon className="size-4" />
						)}
					</button>
					{canOpen && sourceIdForActions !== null ? (
						<button
							type="button"
							onClick={() =>
								onOpenSourceView?.(sourceIdForActions)
							}
							className="flex min-w-0 flex-1 items-center gap-2 text-left"
						>
							<span
								className={cn(
									"truncate text-sm font-medium",
									isSelected
										? "text-accent"
										: "text-foreground",
								)}
							>
								{title}
							</span>
							<Chip size="sm" variant="secondary">
								{groupSkills.length}
							</Chip>
						</button>
					) : (
						// The "no lock record" bucket opens no panel. A
						// `disabled` button announces itself as a dimmed
						// control the user could have pressed; this is a
						// heading.
						<div className="flex min-w-0 flex-1 items-center gap-2">
							<span className="truncate text-sm font-medium text-foreground">
								{title}
							</span>
							<Chip size="sm" variant="secondary">
								{groupSkills.length}
							</Chip>
						</div>
					)}
					{shared.kind === "auth" && (
						<Button
							size="sm"
							variant="ghost"
							className="shrink-0"
							onPress={() => onResolveAuth?.(names[0])}
						>
							{t("credentialBind")}
						</Button>
					)}
					{shared.kind === "other" && (
						<Tooltip delay={0}>
							<Tooltip.Trigger>
								<span className="shrink-0 cursor-default text-xs text-muted">
									{t("skillGroupAllUncheckable")}
								</span>
							</Tooltip.Trigger>
							<Tooltip.Content>
								{t(uncheckableTooltipKey(shared.reason))}
							</Tooltip.Content>
						</Tooltip>
					)}
					{canOpen && sourceIdForActions !== null && (
						<Tooltip delay={500}>
							<Tooltip.Trigger>
								<Button
									isIconOnly
									size="sm"
									variant="ghost"
									className="shrink-0"
									aria-label={t("viewSkillSource")}
									onPress={() =>
										onOpenSourceView?.(sourceIdForActions)
									}
								>
									<ArrowTopRightOnSquareIcon className="size-4 text-muted" />
								</Button>
							</Tooltip.Trigger>
							<Tooltip.Content>
								{t("viewSkillSource")}
							</Tooltip.Content>
						</Tooltip>
					)}
					{onManageGroupAgents && sourceGroupForManage && (
						<Tooltip delay={500}>
							<Tooltip.Trigger>
								<Button
									isIconOnly
									size="sm"
									variant="ghost"
									className="shrink-0"
									aria-label={t("bulkManageGroupAgents")}
									onPress={() =>
										onManageGroupAgents(
											sourceGroupForManage,
										)
									}
								>
									<UsersIcon className="size-4 text-muted" />
								</Button>
							</Tooltip.Trigger>
							<Tooltip.Content>
								{t("bulkManageGroupAgents")}
							</Tooltip.Content>
						</Tooltip>
					)}
				</div>

				{isOpen && (
					<ListBox
						aria-label={t("skillsFromSource", { source: title })}
						selectionMode={selectionMode}
						selectionBehavior="toggle"
						selectedKeys={groupSelectedKeys}
						// Grouped: each source renders its own ListBox, so a
						// range-select here must keep the checks made in the
						// OTHER groups (plain `createSelectionHandler`
						// replaces the whole set and silently drops them).
						onSelectionChange={createGroupedSelectionHandler(names)}
						className="p-2 pl-6"
					>
						{groupSkills.map((s) =>
							renderSkillItem(s, {
								suppressStatusBadge: shared.kind !== "none",
							}),
						)}
					</ListBox>
				)}
			</div>
		);
	};

	if (groupBySource) {
		if (isGroupingLoading) {
			return (
				<div className="flex flex-1 items-center justify-center overflow-y-auto">
					<Spinner size="lg" />
				</div>
			);
		}

		const hasItems = sourceGroups.length > 0 || unrecordedGroup !== null;
		if (!hasItems) {
			return (
				<p className="px-3 py-6 text-center text-sm text-muted">
					{emptyMessage ?? t("noSkillsMatch")}
				</p>
			);
		}

		return (
			<div className="flex-1 overflow-y-auto [transform:translateZ(0)]">
				{sourceGroups.map((sg) =>
					renderGroupBlock({
						groupKey: sg.source,
						title: sg.source,
						sourceIdForActions: sg.source,
						skills: sg.skills,
						sourceGroupForManage: sg,
					}),
				)}
				{unrecordedGroup &&
					renderGroupBlock({
						groupKey: UNRECORDED_GROUP_KEY,
						title: t("skillsUnrecordedSource"),
						sourceIdForActions: null,
						skills: unrecordedGroup,
					})}
			</div>
		);
	}

	if (filteredByName.length === 0) {
		return (
			<p className="px-3 py-6 text-center text-sm text-muted">
				{emptyMessage ?? t("noSkillsMatch")}
			</p>
		);
	}

	return (
		<div className="flex-1 overflow-y-auto">
			<ListBox
				aria-label="Skills"
				selectionMode={selectionMode}
				selectionBehavior="toggle"
				selectedKeys={selectedKeys}
				onSelectionChange={createSelectionHandler(
					filteredByName.map((s) => s.name),
				)}
				className="p-2"
			>
				{filteredByName.map((s) =>
					renderSkillItem(s, { suppressStatusBadge: false }),
				)}
			</ListBox>
		</div>
	);
}

/** Up to two tag chips per row, then a `+N` counter — a long tag list must not
 * push the status badge and agent count off the row. */
function SkillTagChips({ tags }: { tags: string[] }) {
	if (tags.length === 0) return null;
	const shown = tags.slice(0, 2);
	const rest = tags.length - shown.length;
	return (
		<span className="flex shrink-0 items-center gap-1">
			{shown.map((tag) => (
				<Chip key={tag} size="sm" variant="secondary">
					{tag}
				</Chip>
			))}
			{rest > 0 && (
				<Chip size="sm" variant="secondary">
					{`+${rest}`}
				</Chip>
			)}
		</span>
	);
}
