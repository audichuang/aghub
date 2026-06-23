"use client";

import {
	ArrowDownTrayIcon,
	ArrowPathIcon,
	CheckCircleIcon,
	TrashIcon,
} from "@heroicons/react/24/solid";
import {
	Button,
	Checkbox,
	Chip,
	Spinner,
	Switch,
	Tooltip,
} from "@heroui/react";
import { useTranslation } from "react-i18next";
import type {
	CCPluginMarketResponse,
	CCPluginResponse,
} from "../../generated/dto";
import { cn } from "../../lib/utils";
import { usePluginDetailActions } from "../plugin-detail/use-plugin-detail-actions";

type PluginScopeValue = "global" | "project" | "local";

/**
 * An installed plugin row in the market view. Reuses `usePluginDetailActions`
 * so update / enable-disable / uninstall funnel through the same
 * invalidate(["plugins"]) as the detail pane — keeping the three views in sync.
 * Rendered only when an installed match exists, so the hook always has a real
 * plugin (hooks can't be called conditionally).
 */
export function MarketInstalledRow({
	entry,
	installed,
	installScope,
}: {
	entry: CCPluginMarketResponse;
	installed: CCPluginResponse;
	installScope: PluginScopeValue;
}) {
	const { t } = useTranslation();

	const currentScope = ((installed.display_scope &&
	installed.scopes.some((s) => s.scope === installed.display_scope)
		? installed.display_scope
		: installed.scopes[0]?.scope) ?? installScope) as PluginScopeValue;
	const currentScopeInfo =
		installed.scopes.find((s) => s.scope === currentScope) ??
		installed.scopes[0] ??
		null;

	const {
		isToggling,
		updateMutation,
		uninstallMutation,
		enableMutation,
		disableMutation,
		handleUpdate,
		handleUninstall,
	} = usePluginDetailActions({
		currentPlugin: installed,
		currentScope,
		currentScopeInfo,
	});

	const canUpdate = installed.source_info.can_reinstall;

	return (
		<div className="flex items-center gap-2 px-4 py-1.5">
			{/* Reserve the checkbox column so installed/available titles align. */}
			<div aria-hidden className="size-4 shrink-0" />
			<div className="min-w-0 flex-1">
				<div className="flex items-center gap-2">
					<span className="truncate text-sm font-medium text-foreground">
						{entry.name}
					</span>
					<Chip variant="soft" size="sm" className="shrink-0">
						{t("pluginStateInstalled")}
					</Chip>
				</div>
				{entry.description && (
					<p className="mt-0.5 line-clamp-2 text-xs text-muted">
						{entry.description}
					</p>
				)}
			</div>
			<div className="flex w-36 shrink-0 items-center justify-end gap-1">
				{canUpdate && (
					<Tooltip delay={0}>
						<Button
							isIconOnly
							variant="ghost"
							size="sm"
							onPress={handleUpdate}
							isDisabled={updateMutation.isPending}
							aria-label={t("updatePlugin")}
						>
							<ArrowPathIcon
								className={cn(
									"size-4",
									updateMutation.isPending && "animate-spin",
								)}
							/>
						</Button>
						<Tooltip.Content>{t("updatePlugin")}</Tooltip.Content>
					</Tooltip>
				)}
				<Tooltip delay={0}>
					<Button
						isIconOnly
						variant="ghost"
						size="sm"
						className="text-danger"
						onPress={handleUninstall}
						isDisabled={uninstallMutation.isPending}
						aria-label={t("uninstallPlugin")}
					>
						<TrashIcon className="size-4" />
					</Button>
					<Tooltip.Content>{t("uninstallPlugin")}</Tooltip.Content>
				</Tooltip>
				<Switch
					isSelected={installed.enabled}
					isDisabled={isToggling}
					onChange={() => {
						if (installed.enabled) {
							disableMutation.mutate(installed.id);
							return;
						}
						enableMutation.mutate(installed.id);
					}}
					aria-label={
						installed.enabled
							? t("disablePlugin")
							: t("enablePlugin")
					}
				>
					<Switch.Control>
						<Switch.Thumb />
					</Switch.Control>
				</Switch>
			</div>
		</div>
	);
}

/** A not-installed plugin row: selectable for batch install + an install button. */
export function MarketAvailableRow({
	entry,
	compactFormatter,
	isSelected,
	onSelectChange,
	onInstall,
	installState,
}: {
	entry: CCPluginMarketResponse;
	compactFormatter: Intl.NumberFormat;
	isSelected: boolean;
	onSelectChange: (selected: boolean) => void;
	onInstall: (pluginId: string) => void;
	installState?: "installing" | "installed";
}) {
	const { t } = useTranslation();

	return (
		<div className="flex items-center gap-2 px-4 py-1.5">
			<Checkbox
				isSelected={isSelected}
				onChange={onSelectChange}
				aria-label={t("select")}
				isDisabled={installState === "installed"}
				className="shrink-0"
			/>
			<div className="min-w-0 flex-1">
				<span className="truncate text-sm font-medium text-foreground">
					{entry.name}
				</span>
				{entry.description && (
					<p className="mt-0.5 line-clamp-2 text-xs text-muted">
						{entry.description}
					</p>
				)}
			</div>
			<div className="flex w-36 shrink-0 items-center justify-end gap-1">
				<span className="text-xs tabular-nums text-muted">
					{compactFormatter.format(entry.installs)}
				</span>
				{installState === "installed" ? (
					<CheckCircleIcon className="size-5 text-success" />
				) : (
					<Button
						variant="secondary"
						size="sm"
						isDisabled={installState === "installing"}
						onPress={() => onInstall(entry.id)}
					>
						{installState === "installing" ? (
							<Spinner size="sm" />
						) : (
							<ArrowDownTrayIcon className="size-4" />
						)}
						{t("install")}
					</Button>
				)}
			</div>
		</div>
	);
}
