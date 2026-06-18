interface SkillPathLike {
	skillPath: string;
}

export function allSkillPaths(skills: SkillPathLike[]): string[] {
	return skills.map((skill) => skill.skillPath);
}

export function toggleSkillPath(
	selected: ReadonlySet<string>,
	skillPath: string,
): Set<string> {
	const next = new Set(selected);
	if (next.has(skillPath)) {
		next.delete(skillPath);
	} else {
		next.add(skillPath);
	}
	return next;
}

export function selectedSkills<T extends SkillPathLike>(
	skills: T[],
	selected: ReadonlySet<string>,
): T[] {
	return skills.filter((skill) => selected.has(skill.skillPath));
}
