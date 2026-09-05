/**
 * What an installed skill costs an agent at session start.
 *
 * Every agent that supports skills injects ONE LINE per installed skill into
 * the system prompt — the frontmatter `name` plus `description` — and loads
 * the SKILL.md body only when the skill actually runs. So a skill's startup
 * cost is bounded by its description, NOT by how big the skill is on disk.
 * Verified by reading the shipped binaries of Claude Code 2.1.261, Codex
 * 0.153.4 and Grok 1.0.13; the evidence and the per-agent differences are in
 * `docs/specs/2026-09-05-skill-context-cost.md`.
 *
 * The arithmetic below is Claude Code's, because that is the one whose exact
 * algorithm we could read. Codex and Grok inject the same two fields with a
 * different wrapper, so their real numbers are near this one but not equal —
 * hence every value here is presented as an estimate.
 *
 * One deliberate understatement: the budget is shared with slash commands,
 * which aghub does not manage and therefore cannot count. A listing that fits
 * here can still be demoted in the agent. The UI says so; do not "fix" it by
 * inventing a command count.
 */

/** Claude Code's `skillListingMaxDescChars` default. */
export const MAX_DESCRIPTION_CHARS = 1536;

/**
 * Claude Code's own bytes-per-token constant. It derives the listing BUDGET
 * from a context window, so it belongs to the budget arithmetic — not to
 * counting tokens, which `estimateTokens` does separately.
 */
export const BYTES_PER_TOKEN = 4;

/**
 * Characters per token, measured on this project's own corpus (real SKILL.md
 * descriptions and Traditional-Chinese notes) with o200k_base.
 *
 * A single constant is wrong by ~4x on Chinese text: Han characters run about
 * one token EACH, where English prose runs ~4.75 characters per token. Skill
 * descriptions here are routinely Chinese, so a flat chars/4 would quietly
 * understate the very number the feature exists to show.
 *
 * ponytail: two-bucket heuristic, no tokenizer dependency. The BUDGET verdict
 * does not depend on it at all — Claude Code budgets in characters — so this
 * only affects the displayed token figure.
 */
export const CHARS_PER_TOKEN_LATIN = 4.75;
export const CHARS_PER_TOKEN_CJK = 1.03;

/** CJK ideographs, kana and full-width forms. */
const CJK_RE =
	/[\u3000-\u303f\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uff00-\uffef]/u;

/** Estimate tokens for a string, bucketing CJK separately from everything else. */
export function estimateTokens(text: string): number {
	let cjk = 0;
	for (const char of text) {
		if (CJK_RE.test(char)) cjk += 1;
	}
	const latin = [...text].length - cjk;
	return Math.ceil(cjk / CHARS_PER_TOKEN_CJK + latin / CHARS_PER_TOKEN_LATIN);
}

/** Claude Code's `skillListingBudgetFraction` default: 1% of the window. */
export const LISTING_BUDGET_FRACTION = 0.01;

/** Claude Code's default assumed context window. */
export const DEFAULT_CONTEXT_WINDOW = 200_000;

/**
 * Characters one skill contributes to the listing: `- <name>: <description>`.
 * The 4 is `"- "` plus `": "`.
 */
export function skillEntryChars(name: string, description: string): number {
	return (
		name.length + 4 + Math.min(description.length, MAX_DESCRIPTION_CHARS)
	);
}

/**
 * Characters a skill contributes once its description has been dropped for
 * budget: `- <name>`. Claude Code drops whole descriptions rather than
 * truncating them, so this is the floor, not a partial line.
 */
export function skillNameOnlyChars(name: string): number {
	return name.length + 2;
}

/**
 * The listing budget in characters. `SLASH_COMMAND_TOOL_CHAR_BUDGET` overrides
 * it in the agent's own environment, which we cannot see from here — callers
 * must present the result as the default, not as the user's actual budget.
 */
export function listingBudgetChars(
	contextWindow: number = DEFAULT_CONTEXT_WINDOW,
): number {
	return Math.max(
		1,
		Math.floor(contextWindow * BYTES_PER_TOKEN * LISTING_BUDGET_FRACTION),
	);
}

export interface SkillCostInput {
	name: string;
	description: string;
}

export interface SkillContextCost {
	/** Number of skills counted. */
	skillCount: number;
	/** Characters the whole listing occupies, including joining newlines. */
	totalChars: number;
	/** Estimated tokens for that listing, every turn. */
	totalTokens: number;
	/** The default budget these characters are measured against. */
	budgetChars: number;
	/** Characters over budget; 0 when it fits. */
	overBudgetChars: number;
	/**
	 * FEWEST skills whose descriptions must be dropped for the listing to fit
	 * — computed by freeing the largest descriptions first. The agent drops by
	 * least-recently-used instead, which it can only do because it knows usage
	 * counts we do not, so the real number is >= this one. Present it as "at
	 * least".
	 */
	minDemotedSkills: number;
}

/**
 * Estimate what a set of skills costs one agent at every turn.
 *
 * Skills are counted once per name: the same skill reachable through two of an
 * agent's directories is still one line in that agent's listing.
 */
export function estimateSkillContextCost(
	skills: readonly SkillCostInput[],
	contextWindow: number = DEFAULT_CONTEXT_WINDOW,
): SkillContextCost {
	const byName = new Map<string, SkillCostInput>();
	for (const skill of skills) {
		if (!byName.has(skill.name)) byName.set(skill.name, skill);
	}
	const unique = [...byName.values()];

	const entries = unique.map((skill) => ({
		entry: skillEntryChars(skill.name, skill.description ?? ""),
		nameOnly: skillNameOnlyChars(skill.name),
		// The line as the agent writes it, so the token estimate sees the real
		// script mix rather than a character count that has lost it.
		line: `- ${skill.name}: ${(skill.description ?? "").slice(
			0,
			MAX_DESCRIPTION_CHARS,
		)}`,
	}));

	const totalChars =
		entries.reduce((sum, e) => sum + e.entry, 0) +
		Math.max(0, entries.length - 1);
	const budgetChars = listingBudgetChars(contextWindow);
	const overBudgetChars = Math.max(0, totalChars - budgetChars);

	let minDemotedSkills = 0;
	if (overBudgetChars > 0) {
		const savings = entries
			.map((e) => e.entry - e.nameOnly)
			.sort((a, b) => b - a);
		let freed = 0;
		for (const saving of savings) {
			if (freed >= overBudgetChars) break;
			freed += saving;
			minDemotedSkills += 1;
		}
	}

	return {
		skillCount: unique.length,
		totalChars,
		totalTokens: estimateTokens(entries.map((e) => e.line).join("\n")),
		budgetChars,
		overBudgetChars,
		minDemotedSkills,
	};
}
