import {
	ArrowPathIcon,
	ExclamationTriangleIcon,
	LockClosedIcon,
	QuestionMarkCircleIcon,
} from "@heroicons/react/24/solid";
import { Chip, Tooltip } from "@heroui/react";
import { useTranslation } from "react-i18next";
import type { SkillUpdateResponse } from "../generated/dto";
import { uncheckableTooltipKey } from "../lib/skill-group-status";

interface SkillStatusBadgeProps {
	/**
	 * Per-skill status from `GET /skills/check-updates`, or undefined when
	 * no check has run yet. `undefined` and `upToDate` both render nothing —
	 * only actionable states render a badge (§12-C5: no version/date in
	 * Phase 1; hash tooltip only).
	 */
	status?: SkillUpdateResponse;
	/** Called when an `uncheckable { reason: "auth" }` badge is activated. */
	onResolveAuth?: () => void;
}

/**
 * Renders a skill's update status as a compact HeroUI Chip + Tooltip.
 *
 * - `undefined` → null (check not run yet)
 * - `upToDate`  → null (no visual noise for the common case; §spec D3)
 * - `updateAvailable` → yellow "可更新"
 * - `renamed`   → purple "已改名"
 * - `uncheckable(auth)` → always shows a credential button (disabled when
 *   `onResolveAuth` is not provided)
 * - `uncheckable(other)` → muted "無法檢查"
 *
 * Phase 1 only shows hash in tooltip (no version/date — §12-C5).
 */
export function SkillStatusBadge({
	status,
	onResolveAuth,
}: SkillStatusBadgeProps) {
	const { t } = useTranslation();

	// undefined = no check run; upToDate = all good → both silent
	if (!status || status.status === "upToDate") {
		return null;
	}

	if (status.status === "updateAvailable") {
		return (
			<Tooltip delay={0}>
				<Tooltip.Trigger>
					<span className="inline-flex">
						<Chip size="sm" variant="soft" color="default">
							<span className="flex items-center gap-1">
								<ArrowPathIcon className="size-3 text-warning" />
								<span className="text-xs text-warning">
									{t("skillUpdateAvailableBadge")}
								</span>
							</span>
						</Chip>
					</span>
				</Tooltip.Trigger>
				<Tooltip.Content>
					{/* The hashes are folder-content hashes, not commits, so
					    there is no changelog to derive from them. The upstream
					    tip's DATE is the one thing that says how old the
					    pending change is, and the check already carries it. */}
					{status.upstreamCommitTime
						? t("skillUpdateAvailableTooltipDated", {
								current: status.current.slice(0, 8),
								available: status.available.slice(0, 8),
								date: new Date(
									status.upstreamCommitTime,
								).toLocaleDateString(),
							})
						: t("skillUpdateAvailableTooltip", {
								current: status.current.slice(0, 8),
								available: status.available.slice(0, 8),
							})}
				</Tooltip.Content>
			</Tooltip>
		);
	}

	if (status.status === "renamed") {
		return (
			<Tooltip delay={0}>
				<Tooltip.Trigger>
					<span className="inline-flex">
						<Chip size="sm" variant="soft" color="default">
							<span className="flex items-center gap-1">
								<ExclamationTriangleIcon className="size-3 text-warning" />
								<span className="text-xs text-warning">
									{t("skillRenamedBadge")}
								</span>
							</span>
						</Chip>
					</span>
				</Tooltip.Trigger>
				<Tooltip.Content>
					{t("skillRenamedTooltip", {
						newName: status.newName,
					})}
				</Tooltip.Content>
			</Tooltip>
		);
	}

	// uncheckable
	const reason = status.reason;
	const tooltipText = t(uncheckableTooltipKey(reason));

	// auth: ALWAYS show credential button — even without onResolveAuth
	// (disabled when the caller cannot handle it; §12-C5 / §4.2)
	if (reason === "auth") {
		return (
			<Tooltip delay={0}>
				<Tooltip.Trigger>
					<button
						type="button"
						onClick={onResolveAuth}
						className="inline-flex cursor-pointer disabled:cursor-default"
						disabled={!onResolveAuth}
					>
						<Chip size="sm" variant="tertiary" color="default">
							<span className="flex items-center gap-1">
								<LockClosedIcon className="size-3 text-muted" />
								<span className="text-xs text-muted">
									{t("skillNeedsCredential")}
								</span>
							</span>
						</Chip>
					</button>
				</Tooltip.Trigger>
				<Tooltip.Content>
					{t("skillNeedsCredentialTooltip")}
				</Tooltip.Content>
			</Tooltip>
		);
	}

	return (
		<Tooltip delay={0}>
			<Tooltip.Trigger>
				<span className="inline-flex">
					<Chip size="sm" variant="tertiary" color="default">
						<span className="flex items-center gap-1">
							<QuestionMarkCircleIcon className="size-3 text-muted" />
							<span className="text-xs text-muted">
								{t("skillUncheckable")}
							</span>
						</span>
					</Chip>
				</span>
			</Tooltip.Trigger>
			<Tooltip.Content>{tooltipText}</Tooltip.Content>
		</Tooltip>
	);
}
