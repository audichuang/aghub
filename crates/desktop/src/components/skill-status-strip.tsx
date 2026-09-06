import { ArrowPathIcon } from "@heroicons/react/24/solid";
import { Button, Spinner } from "@heroui/react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { SkillLayoutMigrationBanner } from "./skill-layout-migration-banner";

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
 * The three banners `pages/settings/skills.tsx` used to stack (update-all,
 * background-check, layout-migration) collapsed into ONE status strip: each
 * true fact gets a single row (icon, text, right-aligned button), and the
 * whole container renders nothing when there is nothing to report.
 *
 * The container relies on the same `empty:hidden` idiom already used for the
 * migration Alert wrapper elsewhere in this file's siblings: every row here
 * is either a plain conditional (the two JS-computed facts) or
 * `SkillLayoutMigrationBanner` itself, which already returns `null` when its
 * own preview has nothing to report. When every row is absent the container
 * has no DOM children and `:empty` hides it — no extra visibility state to
 * keep in sync by hand.
 *
 * This is a *persistent* fact list, not a toast (desktop `AGENTS.md`
 * reserves toasts for transient events), so it carries `role="status"` +
 * `aria-live="polite"`.
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
