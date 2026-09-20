/**
 * The one rule three preference hooks got wrong the same way: never persist a
 * decision made on a value you failed to READ.
 *
 * `useQuery` hands a failed read back as `data: undefined`, and each hook
 * substituted a fallback for rendering — `DEFAULT_SIDEBAR_ITEMS`, `[]` for
 * stars, "the first installed editor". That fallback is fine on screen for a
 * moment; what is not fine is the next write, which took it as the user's
 * current state and saved it. Toggling one sidebar item after a failed read
 * wrote the DEFAULTS back over the user's hidden/reordered list; starring one
 * skill wrote a one-element array over every other star.
 *
 * So the basis for a write is not "whatever we are rendering" but "what we
 * actually read". `null` means there is none and the caller must write
 * nothing at all — not the fallback, not a merge, nothing.
 */
export function preferenceWriteBasis<T>(
	isSuccess: boolean,
	cached: T | undefined,
	fallback: T,
): T | null {
	if (!isSuccess) return null;
	// A successful read of a store that holds nothing yet IS the fallback:
	// first run, no preferences saved. That is a real answer, not a gap.
	return cached ?? fallback;
}

/**
 * Thrown by a preference write that had no basis. Carries the preference's
 * name so a toast can say which one, and so the throw is distinguishable from
 * the save itself failing — the two need different advice (retry the read vs
 * retry the write).
 */
export class PreferenceNotReadError extends Error {
	readonly preference: string;

	constructor(preference: string) {
		super(
			`refusing to write "${preference}": its stored value was never read, ` +
				`so any write would overwrite it with a fallback`,
		);
		this.name = "PreferenceNotReadError";
		this.preference = preference;
	}
}

/**
 * Apply a preference change optimistically and put BOTH caches back if the
 * save fails.
 *
 * "Both" is the part that is easy to miss, and the desktop AGENTS.md names it:
 * the Tauri store's `set()` mutates its in-memory map and only `save()` reaches
 * disk, so a failed save leaves the rejected value sitting in memory. The next
 * successful `save()` from anywhere else in the app — an autostart toggle, a
 * settings field — then flushes it, and a re-read hands it back as though it
 * had been accepted. Restoring the query cache alone fixes the pixels and
 * leaves that landmine armed.
 *
 * So the rollback goes back through `save` itself, which `set`s the previous
 * value before trying the disk again. If that second attempt also fails the
 * disk is the real problem and there is nothing further to do — memory is
 * correct either way, and the ORIGINAL error is what the caller is told about,
 * because that is the one that describes what the user tried to do.
 */
export async function writePreference<T>(options: {
	previous: T;
	next: T;
	setCache: (value: T) => void;
	save: (value: T) => Promise<void>;
}): Promise<void> {
	const { previous, next, setCache, save } = options;
	setCache(next);
	try {
		await save(next);
	} catch (error) {
		setCache(previous);
		try {
			await save(previous);
		} catch {
			// Deliberately swallowed: see above.
		}
		throw error;
	}
}
