/**
 * Keep the skill groups an agent actually reads.
 *
 * The rows behind a group are one per (skill, agent) — a skill in a shared
 * referrer directory comes back once per agent that reads it. So "does this
 * agent have it" is `items.some(i => i.agent === id)`, NOT `shared_with`,
 * which this API path leaves unset.
 *
 * This filters WHICH GROUPS are shown and returns the groups UNCHANGED. The
 * members are what tells the detail panel and the manage-agents dialog which
 * agents already hold a skill, so narrowing `items` to the selected agent
 * would make a skill installed for twenty agents look like it belongs to one
 * — in a dialog that then writes that state back.
 */
export function filterGroupsByAgent<
	T extends { items: readonly { agent?: string }[] },
>(groups: readonly T[], agentId: string | null): T[] {
	if (agentId === null) return [...groups];
	return groups.filter((group) =>
		group.items.some((item) => item.agent === agentId),
	);
}
