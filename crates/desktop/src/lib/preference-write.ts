/**
 * Never persist a decision made on a value you failed to READ. A failed
 * `useQuery` read is `data: undefined`, and the render fallback it gets must
 * not become the basis of the next write, or it overwrites the stored value.
 * `null` means no basis: the caller writes nothing — not the fallback, not a
 * merge. See docs/history/desktop-frontend.md#preference-fallback-written-back-over-the-stored-value
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
 * save fails — the query cache AND the Tauri store's memory (`src/AGENTS.md`:
 * `set()` mutates memory, only `save()` reaches disk, so a rejected value
 * would be flushed by the next unrelated save).
 *
 * The rollback goes through `save` itself, which `set`s the previous value. If
 * that also fails, memory is still correct; the ORIGINAL error is reported
 * because it describes what the user tried to do.
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
