"use client";

import { ChevronDownIcon } from "@heroicons/react/24/outline";
import { ExclamationCircleIcon, GlobeAltIcon } from "@heroicons/react/24/solid";
import { Accordion, Button, Spinner } from "@heroui/react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { siGithub } from "simple-icons";
import type {
	CCPluginMarketResponse,
	CCPluginResponse,
} from "../../generated/dto";
import { groupByMarketplace } from "../../lib/group-plugins-by-marketplace";
import { MarketAvailableRow, MarketInstalledRow } from "./market-plugin-row";

const GITHUB_REPO_RE = /github\.com\/([^/]+\/[^/]+?)(?:\.git)?\/?$/;
const HTTP_PREFIX_RE = /^https?:\/\//;

// Group by the marketplace name (one stable value per marketplace, so the
// official-pin in groupByMarketplace fires just like Part A). The header label
// prefers the source repo from `github_url`, but official-marketplace plugins
// carry a per-plugin `…/tree/main/<name>` url — trim at `/tree/` to land on the
// repo root; fall back to the marketplace name when no repo resolves.
function marketplaceHeader(
	githubUrl: string,
	marketplace: string,
): { label: string; isGithub: boolean } {
	const repoUrl = githubUrl ? githubUrl.split("/tree/")[0]! : "";
	if (repoUrl) {
		const match = repoUrl.match(GITHUB_REPO_RE);
		if (match) {
			return { label: match[1]!, isGithub: true };
		}
		return {
			label: repoUrl.replace(HTTP_PREFIX_RE, ""),
			isGithub: false,
		};
	}
	return { label: marketplace || "—", isGithub: false };
}

interface PluginMarketTableProps {
	plugins: CCPluginMarketResponse[];
	installedById: Map<string, CCPluginResponse>;
	installScope: "global" | "project" | "local";
	isLoading: boolean;
	isError: boolean;
	error: unknown;
	searchQuery: string;
	compactFormatter: Intl.NumberFormat;
	onRetry: () => void;
	onInstall: (pluginId: string) => void;
	onInstallMany: (pluginIds: string[]) => void;
	installStates: Record<string, "installing" | "installed">;
}

export function PluginMarketTable({
	plugins,
	installedById,
	installScope,
	isLoading,
	isError,
	error,
	searchQuery,
	compactFormatter,
	onRetry,
	onInstall,
	onInstallMany,
	installStates,
}: PluginMarketTableProps) {
	const { t } = useTranslation();
	const [selectedIds, setSelectedIds] = useState<Set<string>>(
		() => new Set(),
	);
	const [manualExpanded, setManualExpanded] = useState<Set<string> | null>(
		null,
	);

	const groups = useMemo(
		() =>
			groupByMarketplace(
				plugins,
				(entry) => entry.marketplace,
				(entry) =>
					marketplaceHeader(entry.github_url, entry.marketplace),
			),
		[plugins],
	);

	const allGroupKeys = useMemo(
		() => groups.map((group) => group.key),
		[groups],
	);

	// Default collapsed, but expand any group that still has not-installed
	// plugins (or the first group if none do). Search forces all open.
	const defaultExpanded = useMemo(() => {
		const withUninstalled = groups
			.filter((group) =>
				group.items.some((entry) => !installedById.has(entry.id)),
			)
			.map((group) => group.key);
		if (withUninstalled.length > 0) {
			return withUninstalled;
		}
		return groups.length > 0 ? [groups[0]!.key] : [];
	}, [groups, installedById]);

	const expandedKeys = useMemo<Set<string>>(() => {
		if (searchQuery) {
			return new Set(allGroupKeys);
		}
		return manualExpanded ?? new Set(defaultExpanded);
	}, [searchQuery, manualExpanded, defaultExpanded, allGroupKeys]);

	const handleExpandedChange = (keys: Set<React.Key>) => {
		if (searchQuery) {
			return;
		}
		setManualExpanded(new Set([...keys].map((key) => String(key))));
	};

	// Not-installed plugins currently shown (across all groups).
	const installableIds = useMemo(
		() =>
			plugins
				.filter(
					(entry) =>
						!installedById.has(entry.id) &&
						installStates[entry.id] !== "installed",
				)
				.map((entry) => entry.id),
		[plugins, installedById, installStates],
	);
	const installableIdSet = useMemo(
		() => new Set(installableIds),
		[installableIds],
	);

	const selectedInstallable = useMemo(
		() => [...selectedIds].filter((id) => installableIdSet.has(id)),
		[selectedIds, installableIdSet],
	);

	const toggleSelected = (id: string, selected: boolean) => {
		setSelectedIds((prev) => {
			const next = new Set(prev);
			if (selected) {
				next.add(id);
			} else {
				next.delete(id);
			}
			return next;
		});
	};

	if (isLoading) {
		return (
			<div className="flex flex-1 items-center justify-center py-12">
				<Spinner size="lg" />
			</div>
		);
	}

	if (isError) {
		return (
			<div className="flex flex-1 flex-col items-center justify-center gap-3 py-12">
				<ExclamationCircleIcon className="size-10 text-danger" />
				<p className="text-sm text-muted">
					{error instanceof Error ? error.message : t("unknownError")}
				</p>
				<Button variant="secondary" size="sm" onPress={onRetry}>
					{t("retry")}
				</Button>
			</div>
		);
	}

	if (plugins.length === 0) {
		return (
			<div className="flex flex-1 flex-col items-center justify-center gap-2 py-12">
				<p className="text-sm text-muted">{t("noPluginsFound")}</p>
			</div>
		);
	}

	return (
		<div className="flex min-h-0 flex-1 flex-col">
			<div className="min-h-0 flex-1 overflow-auto [scrollbar-gutter:stable]">
				<Accordion
					allowsMultipleExpanded
					expandedKeys={expandedKeys}
					onExpandedChange={handleExpandedChange}
					className="p-1"
				>
					{groups.map((group) => (
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
												<path d={siGithub.path} />
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
								<div className="flex flex-col">
									{group.items.map((entry) => {
										const installed = installedById.get(
											entry.id,
										);
										return installed ? (
											<MarketInstalledRow
												key={entry.id}
												entry={entry}
												installed={installed}
												installScope={installScope}
											/>
										) : (
											<MarketAvailableRow
												key={entry.id}
												entry={entry}
												compactFormatter={
													compactFormatter
												}
												isSelected={selectedIds.has(
													entry.id,
												)}
												onSelectChange={(sel) =>
													toggleSelected(
														entry.id,
														sel,
													)
												}
												onInstall={onInstall}
												installState={
													installStates[entry.id]
												}
											/>
										);
									})}
								</div>
							</Accordion.Panel>
						</Accordion.Item>
					))}
				</Accordion>
			</div>
			{installableIds.length > 0 && (
				<div className="flex shrink-0 items-center justify-between gap-2 border-t border-separator/70 px-3 py-2">
					<span className="text-xs text-muted">
						{selectedInstallable.length > 0
							? t("pluginSelectedCount", {
									count: selectedInstallable.length,
								})
							: t("pluginInstallableCount", {
									count: installableIds.length,
								})}
					</span>
					<div className="flex items-center gap-2">
						<Button
							variant="secondary"
							size="sm"
							isDisabled={selectedInstallable.length === 0}
							onPress={() => {
								onInstallMany(selectedInstallable);
								setSelectedIds(new Set());
							}}
						>
							{t("pluginInstallSelected")}
						</Button>
						<Button
							variant="primary"
							size="sm"
							onPress={() => {
								onInstallMany(installableIds);
								setSelectedIds(new Set());
							}}
						>
							{t("pluginInstallAll")}
						</Button>
					</div>
				</div>
			)}
		</div>
	);
}
