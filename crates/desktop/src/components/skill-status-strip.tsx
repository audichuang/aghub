import { ArrowPathIcon } from "@heroicons/react/24/solid";
import { Button, Spinner } from "@heroui/react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { SkillLayoutMigrationBanner } from "./skill-layout-migration-banner";
import { WithheldSkillsRow } from "./withheld-skills-row";

interface SkillStatusStripProps {
	scope: "global" | "project";
	projectPath?: string;
	/** Count of updatable skills the app already has cached — 0 hides this
	 * row (a background check may still fill `backgroundNews` instead). */
	pendingUpdateCount: number;
	onUpdateAll: () => void;
	isApplyingUpdates: boolean;
	/** `null` = no background-check news to report (or it has already been
	 * superseded by a cached update count, which takes priority). */
	backgroundNews: number | null;
	onRefresh: () => void;
	isRefreshing: boolean;
}

/**
 * The skills page's ONE status strip (update-all, background-check,
 * layout-migration): each true fact gets a single row. Every row is a plain
 * conditional or `SkillLayoutMigrationBanner` (which returns `null` when it
 * has nothing), so with no rows the container is `:empty` and `empty:hidden`
 * hides it — no visibility state to sync by hand.
 *
 * A *persistent* fact list, not a toast (desktop `AGENTS.md` reserves toasts
 * for transient events), so it carries `role="status"` + `aria-live="polite"`.
 * See docs/history/desktop-frontend.md#skills-page-stacked-three-banners
 */
export function SkillStatusStrip({
	scope,
	projectPath,
	pendingUpdateCount,
	onUpdateAll,
	isApplyingUpdates,
	backgroundNews,
	onRefresh,
	isRefreshing,
}: SkillStatusStripProps) {
	const { t } = useTranslation();

	const showUpdateRow = pendingUpdateCount > 0;
	// The background-check row is only worth showing when the cached count
	// above isn't already saying the same thing with a real number.
	const showBackgroundRow = !showUpdateRow && backgroundNews !== null;

	return (
		<div
			role="status"
			aria-live="polite"
			className="mx-3 mb-2 flex flex-col divide-y divide-separator rounded-md border border-separator bg-surface-secondary text-xs empty:hidden"
		>
			{showUpdateRow && (
				<StatusStripRow
					icon={
						<ArrowPathIcon className="size-4 shrink-0 text-warning" />
					}
					text={t("updateAllSkills", { count: pendingUpdateCount })}
					buttonLabel={t("sourceUpdateAll")}
					onPress={onUpdateAll}
					isLoading={isApplyingUpdates}
				/>
			)}
			{showBackgroundRow && (
				<StatusStripRow
					icon={
						<ArrowPathIcon className="size-4 shrink-0 text-accent" />
					}
					text={t("backgroundCheckFoundUpdates", {
						count: backgroundNews,
					})}
					buttonLabel={t("refreshSkills")}
					onPress={onRefresh}
					isLoading={isRefreshing}
				/>
			)}
			<SkillLayoutMigrationBanner
				scope={scope}
				projectPath={projectPath}
				variant="row"
			/>
			<WithheldSkillsRow scope={scope} projectPath={projectPath} />
		</div>
	);
}

function StatusStripRow({
	icon,
	text,
	buttonLabel,
	onPress,
	isLoading,
}: {
	icon: ReactNode;
	text: string;
	buttonLabel: string;
	onPress: () => void;
	isLoading: boolean;
}) {
	return (
		<div className="flex items-center gap-2 px-3 py-2">
			{icon}
			<span className="min-w-0 flex-1 truncate text-foreground">
				{text}
			</span>
			<Button
				size="sm"
				variant="ghost"
				className="shrink-0"
				isDisabled={isLoading}
				// The label must ALSO be an aria-label: while loading the
				// only child is HeroUI's Spinner, a bare <svg> with no role
				// or label, so the button would have no accessible name.
				aria-label={buttonLabel}
				onPress={onPress}
			>
				{isLoading ? <Spinner size="sm" /> : buttonLabel}
			</Button>
		</div>
	);
}
