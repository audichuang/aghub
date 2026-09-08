import { ArrowTopRightOnSquareIcon } from "@heroicons/react/24/outline";
import { Chip, Checkbox, toast } from "@heroui/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import type { SourceSkillDiff } from "../generated/dto";
import { cn } from "../lib/utils";
import { sourceSkillUrl } from "../lib/source-skill-url";

export interface SourceSkillRowProps {
	skill: SourceSkillDiff;
	sourceUrl?: string;
	gitRef?: string;
	isExpanded: boolean;
	onToggle: () => void;
	action?: React.ReactNode;
	isSelected?: boolean;
	onToggleSelected?: () => void;
	isSelectionDisabled?: boolean;
	muted?: boolean;
	showReason?: boolean;
}

export function SourceSkillRow({
	skill,
	sourceUrl,
	gitRef,
	isExpanded,
	onToggle,
	action,
	isSelected = false,
	onToggleSelected,
	isSelectionDisabled = false,
	muted = false,
	showReason = false,
}: SourceSkillRowProps) {
	const { t } = useTranslation();
	const detailText = skill.description || skill.skillPath;
	const documentUrl =
		skill.state === "removed"
			? null
			: sourceSkillUrl(sourceUrl, skill.skillPath, gitRef);

	return (
		<li className="flex items-center gap-3 border-b border-border px-3 py-2.5 last:border-b-0 hover:bg-surface-secondary/70">
			{onToggleSelected && (
				<Checkbox
					value={skill.skillPath}
					isSelected={isSelected}
					isDisabled={isSelectionDisabled}
					onChange={() => onToggleSelected()}
					variant="secondary"
					aria-label={t("sourceSelectSkill", {
						name: skill.name,
					})}
					className="shrink-0"
				>
					<Checkbox.Control>
						<Checkbox.Indicator />
					</Checkbox.Control>
				</Checkbox>
			)}
			<button
				type="button"
				className="min-w-0 flex-1 text-left"
				aria-expanded={isExpanded}
				onClick={onToggle}
			>
				<div className="flex min-w-0 items-center gap-2">
					<span
						className={cn(
							"truncate text-sm font-medium",
							muted ? "text-muted" : "text-foreground",
						)}
					>
						{skill.name}
					</span>
					{skill.version && (
						<Chip size="sm" variant="secondary">
							v{skill.version}
						</Chip>
					)}
					<span className="truncate font-mono text-[11px] text-muted/80">
						{skill.skillPath}
					</span>
				</div>
				{detailText && (
					<p
						className={cn(
							"mt-0.5 text-xs leading-5 text-muted",
							!isExpanded && "line-clamp-1",
						)}
					>
						{detailText}
					</p>
				)}
				{showReason && skill.reason && (
					<p className="mt-0.5 text-xs text-muted">{skill.reason}</p>
				)}
				{skill.state === "renamed" && skill.previousName && (
					<p className="mt-0.5 text-xs text-warning">
						{t("sourceRenamedHint", {
							oldName: skill.previousName,
							newName: skill.name,
						})}
					</p>
				)}
			</button>
			{documentUrl && (
				<a
					href={documentUrl}
					target="_blank"
					rel="noopener noreferrer"
					aria-label={t("sourceViewSkillLabel", { name: skill.name })}
					className="flex shrink-0 items-center gap-1 rounded px-2 py-1 text-xs text-accent hover:bg-surface-secondary focus-visible:outline-2 focus-visible:outline-focus"
					onClick={(event) => {
						event.preventDefault();
						void openUrl(documentUrl).catch(() =>
							toast.danger(t("sourceViewSkillFailed")),
						);
					}}
				>
					<ArrowTopRightOnSquareIcon
						aria-hidden="true"
						className="size-3.5"
					/>
					{t("sourceViewSkill")}
				</a>
			)}
			{action && <div className="shrink-0">{action}</div>}
		</li>
	);
}
