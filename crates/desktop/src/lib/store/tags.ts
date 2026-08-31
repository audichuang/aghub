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
	const previous = await store.get<SkillTags>(KEY);
	await store.set(KEY, tags);
	try {
		await store.save();
	} catch (error) {
		// `set` already mutated the store's IN-MEMORY map. Leaving it there
		// means the next `save()` from anywhere else in the app (the autostart
		// toggle, the CLI path…) flushes the edit that just failed. Put the old
		// value back before rethrowing.
		if (previous === undefined || previous === null) {
			await store.delete(KEY);
		} else {
			await store.set(KEY, previous);
		}
		throw error;
	}
}
