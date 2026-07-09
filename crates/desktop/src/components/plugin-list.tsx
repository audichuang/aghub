"use client";

import { ChevronDownIcon } from "@heroicons/react/24/outline";
import {
	ArrowPathIcon,
	CheckCircleIcon,
	GlobeAltIcon,
	PlusIcon,
	PuzzlePieceIcon,
	RectangleStackIcon,
} from "@heroicons/react/24/solid";
import { Accordion, Button, Label, ListBox, Tooltip } from "@heroui/react";
import Fuse from "fuse.js";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { siGithub } from "simple-icons";
import type { CCPluginResponse } from "../generated/dto";
import { useMultiSelect } from "../hooks/use-multi-select";
import {
	groupByMarketplace,
	pluginMarketplaceKey,
} from "../lib/group-plugins-by-marketplace";
import { cn } from "../lib/utils";
import { ListSearchHeader } from "./list-search-header";
import { MultiSelectFloatingBar } from "./multi-select-floating-bar";

interface PluginListProps {
	plugins: CCPluginResponse[];
	selectedKeys: Set<string>;
	searchQuery: string;
	onSearchChange: (value: string) => void;
	onSelectionChange: (keys: Set<string>, clickedKey?: string) => void;
	onOpenMarket: () => void;
	onToggleMultiSelect: () => void;
	onRefresh: () => void;
	onDeleteSelection: () => void;
	selectedCount: number;
	totalCount: number;
	isRefreshing?: boolean;
	isMultiSelectMode?: boolean;
}

export function PluginList({
	plugins,
	selectedKeys,
	searchQuery,
	onSearchChange,
	onSelectionChange,
	onOpenMarket,
	onToggleMultiSelect,
	onRefresh,
	onDeleteSelection,
	selectedCount,
	totalCount,
	isRefreshing = false,
	isMultiSelectMode = false,
}: PluginListProps) {
	const { t } = useTranslation();

	const fuse = useMemo(
		() =>
			new Fuse(plugins, {
				keys: [
					{ name: "name", weight: 2 },
					{ name: "id", weight: 1 },
					{ name: "description", weight: 0.5 },
				],
				threshold: 0.4,
				includeScore: true,
			}),
		[plugins],
	);

	const filteredPlugins = useMemo(() => {
		if (!searchQuery) return plugins;
		return fuse.search(searchQuery).map((result) => result.item);
	}, [fuse, plugins, searchQuery]);

	const groups = useMemo(
		() =>
			groupByMarketplace(
				filteredPlugins,
				(plugin) => pluginMarketplaceKey(plugin.id),
				(plugin) => ({
					label:
						plugin.source_info.label ||
						pluginMarketplaceKey(plugin.id) ||
						plugin.source ||
						"—",
					isGithub: plugin.source_info.is_github,
				}),
			),
		[filteredPlugins],
	);

	const allGroupKeys = useMemo(
		() => groups.map((group) => group.key),
		[groups],
	);

	const [collapsedKeys, setCollapsedKeys] = useState<Set<string>>(
		() => new Set(),
	);

	// All groups expanded by default; manual collapses are tracked here (and
	// reset on reload — v1 is intentionally non-persistent). During search,
	// expansion is forced so matches never hide inside a collapsed group.
	const expandedKeys = useMemo<Set<string>>(() => {
		if (searchQuery) {
			return new Set(allGroupKeys);
		}
		return new Set(allGroupKeys.filter((key) => !collapsedKeys.has(key)));
	}, [searchQuery, allGroupKeys, collapsedKeys]);

	const handleExpandedChange = (keys: Set<React.Key>) => {
		if (searchQuery) {
			return;
		}
		const expanded = new Set([...keys].map((key) => String(key)));
		const nextCollapsed = new Set<string>();
		for (const key of allGroupKeys) {
			if (!expanded.has(key)) {
				nextCollapsed.add(key);
			}
		}
		setCollapsedKeys(nextCollapsed);
	};

	const { createGroupedSelectionHandler } = useMultiSelect({
		selectedKeys,
		onSelectionChange,
		isMultiSelectMode,
	});

	return (
		<div className="relative flex h-full flex-col">
			<ListSearchHeader
				searchValue={searchQuery}
				onSearchChange={onSearchChange}
				placeholder={t("searchPlugins")}
				ariaLabel={t("searchPlugins")}
			>
				<Tooltip delay={0}>
					<Button
						isIconOnly
						variant="ghost"
						size="sm"
						className="shrink-0"
						onPress={onOpenMarket}
						aria-label={t("installFromMarket")}
					>
						<PlusIcon className="size-4" />
					</Button>
					<Tooltip.Content>{t("installFromMarket")}</Tooltip.Content>
				</Tooltip>
				<Tooltip delay={0}>
					<Button
						isIconOnly
						variant="ghost"
						size="sm"
						className={cn(
							"shrink-0 text-muted",
							isMultiSelectMode && "bg-accent/10 text-accent",
						)}
						aria-label={
							isMultiSelectMode
								? t("doneSelecting")
								: t("multiSelect")
						}
						onPress={onToggleMultiSelect}
					>
						{isMultiSelectMode ? (
							<CheckCircleIcon className="size-4" />
						) : (
							<RectangleStackIcon className="size-4" />
						)}
					</Button>
					<Tooltip.Content>
						{isMultiSelectMode
							? t("doneSelecting")
							: t("multiSelect")}
					</Tooltip.Content>
				</Tooltip>
				<Tooltip delay={0}>
					<Button
						isIconOnly
						variant="ghost"
						size="sm"
						className="shrink-0"
						aria-label={t("refreshPlugins")}
						onPress={onRefresh}
						isDisabled={isRefreshing}
					>
						<ArrowPathIcon
							className={cn(
								"size-4",
								isRefreshing && "animate-spin",
							)}
						/>
					</Button>
					<Tooltip.Content>{t("refreshPlugins")}</Tooltip.Content>
				</Tooltip>
			</ListSearchHeader>
			{filteredPlugins.length === 0 ? (
				<div className="px-3 py-6 text-center">
					<p className="text-sm text-muted">
						{searchQuery
							? t("noPluginsMatch")
							: t("noPluginsInstalled")}
					</p>
					{searchQuery && (
						<p className="mt-1 text-xs text-muted">
							"{searchQuery}"
						</p>
					)}
				</div>
			) : (
				<div className="flex-1 overflow-y-auto [transform:translateZ(0)]">
					<Accordion
						allowsMultipleExpanded
						expandedKeys={expandedKeys}
						onExpandedChange={handleExpandedChange}
						className="p-2"
					>
						{groups.map((group) => {
							const groupIds = group.items.map(
								(plugin) => plugin.id,
							);
							const groupIdSet = new Set(groupIds);
							const groupSelectedKeys = new Set(
								[...selectedKeys].filter((id) =>
									groupIdSet.has(id),
								),
							);
							const handleGroupSelection =
								createGroupedSelectionHandler(groupIds);
							return (
								<Accordion.Item key={group.key} id={group.key}>
									<Accordion.Heading>
										<Accordion.Trigger>
											<div className="flex w-full items-center gap-2">
												{group.isGithub ? (
													<svg
														role="img"
														aria-hidden="true"
														className="size-3.5 shrink-0 text-muted"
														viewBox="0 0 24 24"
														fill="currentColor"
													>
														<path
															d={siGithub.path}
														/>
													</svg>
												) : (
													<GlobeAltIcon className="size-3.5 shrink-0 text-muted" />
												)}
												<span className="min-w-0 flex-1 truncate text-left text-xs font-medium text-muted">
													{group.label}
												</span>
												<span className="shrink-0 text-xs text-muted">
													{group.items.length}
												</span>
												<Accordion.Indicator>
													<ChevronDownIcon className="size-4" />
												</Accordion.Indicator>
											</div>
										</Accordion.Trigger>
									</Accordion.Heading>
									<Accordion.Panel>
										<ListBox
											aria-label={group.label}
											selectionMode="multiple"
											selectionBehavior="toggle"
											selectedKeys={groupSelectedKeys}
											onSelectionChange={
												handleGroupSelection
											}
										>
											{group.items.map((plugin) => (
												<ListBox.Item
													key={plugin.id}
													id={plugin.id}
													textValue={plugin.name}
													className="transition-colors duration-200 data-selected:bg-surface"
												>
													<div className="flex w-full items-center gap-2">
														<PuzzlePieceIcon className="size-4 shrink-0 text-muted" />
														<Label className="flex-1 truncate font-medium">
															{plugin.name}
														</Label>
														<div className="shrink-0 pl-1">
															<div
																className={cn(
																	"size-2.5 rounded-full transition-colors duration-300",
																	plugin.enabled
																		? "bg-success"
																		: "bg-muted shadow-inner",
																)}
																title={
																	plugin.enabled
																		? t(
																				"enabled",
																			)
																		: t(
																				"disabled",
																			)
																}
															/>
														</div>
													</div>
												</ListBox.Item>
											))}
										</ListBox>
									</Accordion.Panel>
								</Accordion.Item>
							);
						})}
					</Accordion>
				</div>
			)}

			{isMultiSelectMode && selectedCount > 0 && (
				<MultiSelectFloatingBar
					selectedCount={selectedCount}
					totalCount={totalCount}
					onDelete={onDeleteSelection}
				/>
			)}
		</div>
	);
}
