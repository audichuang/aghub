import { getStore } from ".";
import type { SkillTags } from "../skill-tags";

const KEY = "skillTags";

/** Missing key = no tags yet. Deliberately no store migration: the same
 * `?? default` shape `getStarredSkills` uses, so this overlay does not need a
 * CURRENT_VERSION bump of its own. */
export async function getSkillTags(): Promise<SkillTags> {
	const store = await getStore();
	return (await store.get<SkillTags>(KEY)) ?? {};
}

export async function setSkillTags(tags: SkillTags): Promise<void> {
	const store = await getStore();
	await store.set(KEY, tags);
	await store.save();
}
