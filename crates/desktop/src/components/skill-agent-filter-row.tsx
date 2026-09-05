import { Tooltip } from "@heroui/react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { SkillResponse } from "../generated/dto";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import { estimateSkillContextCost } from "../lib/skill-context-cost";
import { cn } from "../lib/utils";

/**
 * The one agent whose listing arithmetic we read out of a shipped binary.
 * The budget, and dropping a whole description when it is exceeded, are Claude
 * Code's behaviour — Codex shortens descriptions instead and Grok wraps each
 * skill in XML with no comparable budget. So only Claude Code gets the budget
 * verdict; every other agent gets the size of its listing without a claim
 * about what its own budget would do with it.
 */
const BUDGETED_AGENT_ID = "claude";

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

	// `SkillList` hides the rows of agents that are not usable, so a chip for
	// one would filter the list down to nothing.
	const usableAgentIds = useMemo(
		() =>
			new Set(
				availableAgents
					.filter((agent) => agent.isUsable)
					.map((agent) => agent.id),
			),
		[availableAgents],
	);

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
			if (!skill.agent || !usableAgentIds.has(skill.agent)) continue;
			const seen = counts.get(skill.agent) ?? new Set<string>();
			seen.add(skill.name);
			counts.set(skill.agent, seen);
		}
		// A SELECTED agent stays in the row even after its last skill goes —
		// otherwise its chip vanishes with the filter still active and the
		// list is empty with nothing left to click. Same rule the tag filter
		// row states for tags.
		if (selected !== null && !counts.has(selected)) {
			counts.set(selected, new Set());
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
	}, [skills, displayNames, usableAgentIds, selected]);

	/**
	 * The agent the readout describes: the one picked, else the only usable
	 * one on this machine.
	 *
	 * Derived from `rows`, NOT from the raw `skills`: one skill in a shared
	 * referrer directory arrives once per agent that reads it, so a machine
	 * with a single usable agent still sees a dozen distinct `agent` values in
	 * the raw rows — asking those "is there exactly one agent" answers no on
	 * exactly the machine this fallback exists for.
	 */
	const costAgentId = selected ?? (rows.length === 1 ? rows[0].id : null);

	const cost = useMemo(() => {
		if (costAgentId === null) return null;
		return estimateSkillContextCost(
			skills
				.filter((skill) => skill.agent === costAgentId)
				.map((skill) => ({
					name: skill.name,
					description: skill.description,
				})),
		);
	}, [skills, costAgentId]);

	// One usable agent means no chips worth showing — but the cost line is the
	// point of the row, so it still renders. An ACTIVE filter always shows the
	// chips, or there would be no way to clear it.
	const showChips = rows.length >= 2 || selected !== null;
	const showCost = cost !== null && cost.skillCount > 0;
	if (!showChips && !showCost) return null;

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

	const isBudgeted = costAgentId === BUDGETED_AGENT_ID;
	const overBudget = isBudgeted && cost !== null && cost.overBudgetChars > 0;

	return (
		<div className="flex flex-col gap-1 px-3 pb-2">
			{showChips && (
				<div className="flex flex-wrap items-center gap-1.5">
					{chip(null, t("allAgentsFilter"))}
					{rows.map((row) =>
						chip(row.id, row.displayName, row.count),
					)}
				</div>
			)}
			{showCost && cost !== null && (
				<Tooltip delay={0}>
					<Tooltip.Trigger>
						<span
							className={cn(
								"cursor-default text-xs tabular-nums",
								overBudget ? "text-warning" : "text-muted",
							)}
						>
							{isBudgeted
								? t("skillContextCost", {
										tokens: cost.totalTokens.toLocaleString(),
										chars: cost.totalChars.toLocaleString(),
										budget: cost.budgetChars.toLocaleString(),
									})
								: t("skillContextCostNoBudget", {
										tokens: cost.totalTokens.toLocaleString(),
										chars: cost.totalChars.toLocaleString(),
									})}
							{overBudget &&
								` · ${t("skillContextOverBudget", {
									count: cost.minDemotedSkills,
								})}`}
						</span>
					</Tooltip.Trigger>
					<Tooltip.Content>
						{isBudgeted
							? t("skillContextCostTooltip")
							: t("skillContextCostTooltipNoBudget")}
					</Tooltip.Content>
				</Tooltip>
			)}
		</div>
	);
}
