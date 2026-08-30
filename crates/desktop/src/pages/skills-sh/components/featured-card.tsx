import { Chip } from "@heroui/react";
import { useTranslation } from "react-i18next";
import { SkillInfoCard } from "../../../components/skill-info-card";
import type { FeaturedSkill } from "../featured";

interface FeaturedCardProps {
	skill: FeaturedSkill;
	installed: boolean;
	onInstall: () => void;
}

export function FeaturedCard({
	skill,
	installed,
	onInstall,
}: FeaturedCardProps) {
	const { t } = useTranslation();
	const summary = skill.summary.trim() || t("featuredEmptySummary");

	return (
		<button
			type="button"
			onClick={onInstall}
			className="flex flex-col gap-3 rounded-lg border border-border bg-surface p-4 text-left transition-colors hover:bg-surface-secondary"
		>
			<div className="flex items-start justify-between gap-2">
				<SkillInfoCard
					name={skill.name}
					source={skill.source}
					className="min-w-0 flex-1 bg-transparent px-0 py-0"
				/>
				{installed && (
					<Chip size="sm" variant="soft" color="default">
						<span className="text-xs text-success">
							{t("installed")}
						</span>
					</Chip>
				)}
			</div>
			<p className="line-clamp-2 text-sm text-muted">{summary}</p>
		</button>
	);
}
