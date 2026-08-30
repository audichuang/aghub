import { getStore } from ".";

const KEY = "aghubCliPath";

/** Absolute path the user pointed at when `aghub-cli` is not on the GUI app's
 * PATH (common on macOS, where a bundle inherits launchd's PATH, not the
 * shell's). Missing key = fall back to PATH resolution. */
export async function getAghubCliPath(): Promise<string | null> {
	const store = await getStore();
	return (await store.get<string>(KEY)) ?? null;
}

export async function setAghubCliPath(path: string | null): Promise<void> {
	const store = await getStore();
	if (path === null) await store.delete(KEY);
	else await store.set(KEY, path);
	await store.save();
}
