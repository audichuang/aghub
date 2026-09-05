import { ListBox, Select, Tooltip } from "@heroui/react";
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

/** Sentinel key for "no agent filter" — `null` is not a valid Select key. */
const ALL_AGENTS_KEY = "__all__";

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

	// One usable agent means no choice worth offering — but the cost line is
	// the point of the row, so it still renders. An ACTIVE filter always shows
	// the Select, or there would be no way to clear it.
	const showSelect = rows.length >= 2 || selected !== null;
	const showCost = cost !== null && cost.skillCount > 0;
	if (!showSelect && !showCost) return null;

	const isBudgeted = costAgentId === BUDGETED_AGENT_ID;
	const overBudget = isBudgeted && cost !== null && cost.overBudgetChars > 0;

	return (
		<div className="flex flex-col gap-1 px-3 pb-2">
			{showSelect && (
				<Select
					aria-label={t("agents")}
					className="min-w-0"
					variant="secondary"
					// `null` is not a valid Select key, so "no filter" gets a
					// sentinel id. Leaving `selectedKey` undefined instead
					// would render the placeholder while the list is in fact
					// unfiltered.
					selectedKey={selected ?? ALL_AGENTS_KEY}
					onSelectionChange={(key) => {
						const k = String(key);
						onChange(k === ALL_AGENTS_KEY ? null : k);
					}}
				>
					<Select.Trigger>
						<Select.Value />
						<Select.Indicator />
					</Select.Trigger>
					<Select.Popover>
						<ListBox>
							<ListBox.Item
								id={ALL_AGENTS_KEY}
								textValue={t("allAgentsFilter")}
							>
								{t("allAgentsFilter")}
								<ListBox.ItemIndicator />
							</ListBox.Item>
							{rows.map((row) => (
								<ListBox.Item
									key={row.id}
									id={row.id}
									// The count belongs in the label, not a
									// sibling node: `Select.Value` renders the
									// item's text, so anything outside it is
									// invisible on the closed trigger.
									textValue={`${row.displayName} (${row.count})`}
								>
									{`${row.displayName} (${row.count})`}
									<ListBox.ItemIndicator />
								</ListBox.Item>
							))}
						</ListBox>
					</Select.Popover>
				</Select>
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
