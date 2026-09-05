import { Tooltip } from "@heroui/react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { SkillResponse } from "../generated/dto";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { estimateSkillContextCost } from "../lib/skill-context-cost";
import { cn } from "../lib/utils";

interface SkillAgentFilterRowProps {
	/**
	 * The raw skill rows, ONE PER (skill, agent) pair — the shape
	 * `GET /agents/all/skills` returns, before the page groups them by name.
	 * A skill in a shared referrer directory comes back once per agent that
	 * reads it, which is exactly what "installed for this agent" means.
	 */
	skills: readonly SkillResponse[];
	/** `null` = no agent filter (show everything). */
	selected: string | null;
	onChange: (agentId: string | null) => void;
}

interface AgentRow {
	id: string;
	displayName: string;
	count: number;
}

/**
 * Filter the skill list down to one agent, and say what that agent pays for
 * those skills on every turn.
 *
 * The cost is only shown for a SINGLE agent because that is the only number
 * that means anything: each agent gets its own skill listing in its own system
 * prompt, so a total across agents would be a sum of things that never share a
 * context window. See `lib/skill-context-cost.ts` for where the arithmetic
 * comes from.
 */
export function SkillAgentFilterRow({
	skills,
	selected,
	onChange,
}: SkillAgentFilterRowProps) {
	const { t } = useTranslation();
	const { allAgents, availableAgents } = useAgentAvailability();

	const displayNames = useMemo(() => {
		const map = new Map<string, string>();
		for (const agent of [...allAgents, ...availableAgents]) {
			map.set(agent.id, agent.display_name);
		}
		return map;
	}, [allAgents, availableAgents]);

	const rows = useMemo<AgentRow[]>(() => {
		const counts = new Map<string, Set<string>>();
		for (const skill of skills) {
			if (!skill.agent) continue;
			const seen = counts.get(skill.agent) ?? new Set<string>();
			seen.add(skill.name);
			counts.set(skill.agent, seen);
		}
		return [...counts.entries()]
			.map(([id, names]) => ({
				id,
				displayName: displayNames.get(id) ?? id,
				count: names.size,
			}))
			.sort(
				(a, b) =>
					b.count - a.count ||
					a.displayName.localeCompare(b.displayName),
			);
	}, [skills, displayNames]);

	const cost = useMemo(() => {
		if (!selected) return null;
		return estimateSkillContextCost(
			skills
				.filter((skill) => skill.agent === selected)
				.map((skill) => ({
					name: skill.name,
					description: skill.description ?? "",
				})),
		);
	}, [skills, selected]);

	// A filter with one option filters nothing.
	if (rows.length < 2) return null;

	const chip = (id: string | null, label: string, count?: number) => {
		const isActive = selected === id;
		return (
			<button
				key={id ?? "__all__"}
				type="button"
				onClick={() => onChange(isActive && id !== null ? null : id)}
				aria-pressed={isActive}
				className={cn(
					"rounded-full border border-separator px-2 py-0.5 text-xs transition-colors",
					isActive
						? "bg-accent/10 text-accent"
						: "text-muted hover:bg-surface-secondary",
				)}
			>
				{count === undefined ? label : `${label} ${count}`}
			</button>
		);
	};

	return (
		<div className="flex flex-col gap-1 px-3 pb-2">
			<div className="flex flex-wrap items-center gap-1.5">
				{chip(null, t("allAgentsFilter"))}
				{rows.map((row) => chip(row.id, row.displayName, row.count))}
			</div>
			{cost !== null && cost.skillCount > 0 && (
				<Tooltip delay={0}>
					<Tooltip.Trigger>
						<span
							className={cn(
								"cursor-default text-xs tabular-nums",
								cost.overBudgetChars > 0
									? "text-warning"
									: "text-muted",
							)}
						>
							{t("skillContextCost", {
								tokens: cost.totalTokens.toLocaleString(),
								chars: cost.totalChars.toLocaleString(),
								budget: cost.budgetChars.toLocaleString(),
							})}
							{cost.overBudgetChars > 0 &&
								` · ${t("skillContextOverBudget", {
									count: cost.minDemotedSkills,
								})}`}
						</span>
					</Tooltip.Trigger>
					<Tooltip.Content>
						{t("skillContextCostTooltip")}
					</Tooltip.Content>
				</Tooltip>
			)}
		</div>
	);
}
