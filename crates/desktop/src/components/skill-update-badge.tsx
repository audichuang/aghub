import {
	ArrowPathIcon,
	CheckCircleIcon,
	ExclamationTriangleIcon,
	LockClosedIcon,
	QuestionMarkCircleIcon,
} from "@heroicons/react/24/solid";
import { Chip, Tooltip } from "@heroui/react";
import { useTranslation } from "react-i18next";
import type { SkillUpdateResponse } from "../generated/dto";

interface SkillUpdateBadgeProps {
	/** The per-skill status from `GET /skills/check-updates`, or undefined when
	 * no check has run yet (the badge renders nothing in that case). */
	status?: SkillUpdateResponse;
	/** Called when an `uncheckable { reason: "auth" }` badge is activated so the
	 * caller can open a credential picker and retry. */
	onResolveAuth?: () => void;
}

/** Human-readable tooltip text for an `uncheckable` reason. Falls back to a
 * generic message so an unknown reason still renders something useful. */
function uncheckableTooltipKey(reason: string): string {
	switch (reason) {
		case "auth":
			return "skillUncheckableAuth";
		case "network":
			return "skillUncheckableNetwork";
		case "local":
			return "skillUncheckableLocal";
		case "ssh":
		case "unsupportedScheme":
			return "skillUncheckableUnsupported";
		case "noPath":
			return "skillUncheckableNoPath";
		case "timeout":
			return "skillUncheckableTimeout";
		default:
			return "skillUncheckableGeneric";
	}
}

/** Renders a skill's update status as a compact HeroUI Chip + Tooltip. An
 * `auth`-blocked check is actionable (opens the credential picker via
 * `onResolveAuth`); other states are informational. */
export function SkillUpdateBadge({
	status,
	onResolveAuth,
}: SkillUpdateBadgeProps) {
	const { t } = useTranslation();

	if (!status) {
		return null;
	}

	if (status.status === "upToDate") {
		return (
			<Tooltip delay={0}>
				<Tooltip.Trigger>
					<span className="inline-flex">
						<Chip size="sm" variant="soft" color="default">
							<span className="flex items-center gap-1">
								<CheckCircleIcon className="size-3 text-success" />
								<span className="text-xs">
									{t("skillUpToDate")}
								</span>
							</span>
						</Chip>
					</span>
				</Tooltip.Trigger>
				<Tooltip.Content>{t("skillUpToDateTooltip")}</Tooltip.Content>
			</Tooltip>
		);
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
					{t("skillUpdateAvailableTooltip", {
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
	const tooltip = t(uncheckableTooltipKey(reason));

	if (reason === "auth" && onResolveAuth) {
		return (
			<Tooltip delay={0}>
				<Tooltip.Trigger>
					<button
						type="button"
						onClick={onResolveAuth}
						className="inline-flex cursor-pointer"
					>
						<Chip size="sm" variant="tertiary" color="default">
							<span className="flex items-center gap-1">
								<LockClosedIcon className="size-3 text-muted" />
								<span className="text-xs">
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
			<Tooltip.Content>{tooltip}</Tooltip.Content>
		</Tooltip>
	);
}
