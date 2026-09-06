import { CpuChipIcon } from "@heroicons/react/24/solid";
import { ListBox, Select, Tooltip } from "@heroui/react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { SkillResponse } from "../generated/dto";
import { useAgentAvailability } from "../hooks/use-agent-availability";
import {
	estimateSkillContextCost,
	type SkillContextCost,
} from "../lib/skill-context-cost";
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

interface AgentRow {
	id: string;
	displayName: string;
	count: number;
}

export interface SkillAgentFilterData {
	rows: AgentRow[];
	/** One usable agent means no choice worth offering, unless a filter is
	 * already active (there would be no way to clear it otherwise). */
	showSelect: boolean;
	/** Distinct skill names visible in this scope, for the "All" row —
	 * NOT the sum of `rows[].count`, since one skill in a shared
	 * referrer directory is counted once per agent that reads it. */
	totalSkillCount: number;
	cost: SkillContextCost | null;
	showCost: boolean;
	isBudgeted: boolean;
	overBudget: boolean;
}

/**
 * The data behind both Row A's agent Select and the token-estimate line —
 * shared so the page computes it once and the two halves (now on separate
 * rows) never drift out of sync with each other.
 *
 * The cost is only shown for a SINGLE agent because that is the only number
 * that means anything: each agent gets its own skill listing in its own system
 * prompt, so a total across agents would be a sum of things that never share a
 * context window. See `lib/skill-context-cost.ts` for where the arithmetic
 * comes from.
 */
export function useSkillAgentFilterData(
	/**
	 * The raw skill rows, ONE PER (skill, agent) pair — the shape
	 * `GET /agents/all/skills` returns, before the page groups them by name.
	 * A skill in a shared referrer directory comes back once per agent that
	 * reads it, which is exactly what "installed for this agent" means.
	 */
	skills: readonly SkillResponse[],
	/** `null` = no agent filter (show everything). */
	selected: string | null,
): SkillAgentFilterData {
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

	const isBudgeted = costAgentId === BUDGETED_AGENT_ID;
	const overBudget = isBudgeted && cost !== null && cost.overBudgetChars > 0;

	const totalSkillCount = new Set(
		skills
			.filter((skill) => skill.agent && usableAgentIds.has(skill.agent))
			.map((skill) => skill.name),
	).size;

	return {
		rows,
		showSelect,
		totalSkillCount,
		cost,
		showCost,
		isBudgeted,
		overBudget,
	};
}

interface SkillAgentSelectProps {
	data: SkillAgentFilterData;
	selected: string | null;
	onChange: (agentId: string | null) => void;
}

/**
 * Row A's compact agent Select. A CPU-chip icon is the box's persistent
 * visual label — it says "this control filters by AGENT" at a glance, next
 * to `ScopeControl`'s own value-only trigger — while the accessible name and
 * the option text stay full prose (`aria-label` + each item's "name (count)").
 * Renders nothing when there is no usable filter to offer (see
 * `useSkillAgentFilterData`'s `showSelect`).
 */
export function SkillAgentSelect({
	data,
	selected,
	onChange,
}: SkillAgentSelectProps) {
	const { t } = useTranslation();
	// Distinct skills across every agent — NOT the sum of the row
	// counts, since one skill in a shared referrer directory is
	// counted once per agent that reads it.
	const allLabel = `${t("allAgentsFilter")} (${data.totalSkillCount})`;
	if (!data.showSelect) return null;

	return (
		<Select
			aria-label={t("filterByAgent")}
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
				<CpuChipIcon
					className="me-1.5 size-4 shrink-0 text-muted"
					aria-hidden="true"
				/>
				<Select.Value className="truncate" />
				<Select.Indicator />
			</Select.Trigger>
			<Select.Popover>
				<ListBox>
					<ListBox.Item
						id={ALL_AGENTS_KEY}
						// Same shape as every agent row below: the count
						// lives in the label, because `Select.Value`
						// renders the item's text and nothing else.
						textValue={allLabel}
					>
						{allLabel}
						<ListBox.ItemIndicator />
					</ListBox.Item>
					{data.rows.map((row) => (
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
	);
}

/**
 * The token-estimate line, split out of the Select so the page can lay them
 * out on separate rows (only shown once an agent is picked). Same algorithm
 * and copy as before — only the layout moved.
 */
export function SkillAgentCostLine({ data }: { data: SkillAgentFilterData }) {
	const { t } = useTranslation();
	const { cost, showCost, isBudgeted, overBudget } = data;
	if (!showCost || cost === null) return null;

	return (
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
	);
}
