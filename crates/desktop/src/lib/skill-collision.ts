export interface ExistingSkillLocation {
	name: string;
	agent?: string;
	source_path: string | null;
	canonical_path?: string;
}

export interface SkillCollision {
	name: string;
	locations: ExistingSkillLocation[];
	/** True only when a location has no canonical target to identify it as a link. */
	requiresAdvisory: boolean;
}

/** Find existing same-name skills without guessing ownership or provenance. */
export function findSkillCollisions(
	selectedNames: readonly string[],
	existing: readonly ExistingSkillLocation[],
): SkillCollision[] {
	const names = new Set(selectedNames);
	const grouped = new Map<string, ExistingSkillLocation[]>();
	for (const skill of existing) {
		if (!names.has(skill.name)) continue;
		const locations = grouped.get(skill.name) ?? [];
		if (
			!locations.some(
				(location) =>
					location.agent === skill.agent &&
					location.source_path === skill.source_path &&
					location.canonical_path === skill.canonical_path,
			)
		) {
			locations.push(skill);
		}
		grouped.set(skill.name, locations);
	}
	return [...grouped.entries()].map(([name, locations]) => ({
		name,
		locations,
		requiresAdvisory: locations.some(
			(location) => !location.canonical_path,
		),
	}));
}
