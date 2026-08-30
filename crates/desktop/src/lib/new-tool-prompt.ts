export interface NewToolPromptAgent {
	id: string;
	isAvailable: boolean;
	isDisabled: boolean;
	skillMutableGlobal: boolean;
	needsLink: boolean;
}

export type NewToolPromptResult =
	| { kind: "seedOnly"; seed: string[] }
	| { kind: "prompt"; ids: string[]; seed: string[] }
	| { kind: "quiet"; seed: string[] };

export interface SkillAgentRow {
	name: string;
	agent?: string | null;
}

export interface NewAgentReconcilePlan {
	name: string;
	sourceAgent: string;
	added: string[];
}

export function eligibleAgentIds(agents: NewToolPromptAgent[]): string[] {
	const ids: string[] = [];
	const seen = new Set<string>();
	for (const agent of agents) {
		if (
			agent.isAvailable &&
			!agent.isDisabled &&
			agent.skillMutableGlobal &&
			agent.needsLink &&
			!seen.has(agent.id)
		) {
			seen.add(agent.id);
			ids.push(agent.id);
		}
	}
	return ids;
}

export function newToolPromptDelta(args: {
	lastKnown: string[] | null;
	agents: NewToolPromptAgent[];
}): NewToolPromptResult {
	const seed = eligibleAgentIds(args.agents);
	if (args.lastKnown === null) {
		return { kind: "seedOnly", seed };
	}
	const known = new Set(args.lastKnown);
	const ids = seed.filter((id) => !known.has(id));
	if (ids.length === 0) {
		return { kind: "quiet", seed };
	}
	return { kind: "prompt", ids, seed };
}

export function reconcileAddsForNewAgents(
	skills: SkillAgentRow[],
	addedIds: string[],
): NewAgentReconcilePlan[] {
	const byName = new Map<string, Set<string>>();
	const order: string[] = [];
	for (const row of skills) {
		if (!row.agent) continue;
		let agents = byName.get(row.name);
		if (!agents) {
			agents = new Set();
			byName.set(row.name, agents);
			order.push(row.name);
		}
		agents.add(row.agent);
	}
	const plans: NewAgentReconcilePlan[] = [];
	for (const name of order) {
		const installed = byName.get(name);
		if (!installed || installed.size === 0) continue;
		const sourceAgent = [...installed][0];
		const added = addedIds.filter((id) => !installed.has(id));
		if (added.length === 0) continue;
		plans.push({ name, sourceAgent, added });
	}
	return plans;
}
