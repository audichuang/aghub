/**
 * One-shot, throttled "add a GitHub credential to speed up update checks"
 * hint. Anonymous GitHub REST calls are rate-limited (then fall back to the
 * slower gix clone path), so a stored credential makes checks faster — but
 * the reminder must never nag, so it fires at most once per week.
 */

const STORAGE_KEY = "credentialSpeedHintShownAt";
const SHOW_INTERVAL_MS = 7 * 24 * 60 * 60 * 1000; // at most once a week

interface HintSource {
	sourceType: string;
	// Optional to match `SourceRow` (project-scope rows may omit it); an
	// unknown status never triggers the hint.
	credentialStatus?: string;
}

/**
 * `true` iff the hint should be shown now — and, when it is, records the
 * timestamp so later calls stay silent for the next interval (the claim and
 * the throttle are one atomic step; callers never mark separately).
 *
 * Shown only when at least one GitHub source has no usable credential
 * (`missing`, or `notRequired` — public repos still check faster
 * authenticated, since anonymous REST rate-limits fall back to slow clones).
 * Callers must additionally gate on "the user has no stored credentials":
 * the sources API currently hardcodes `notRequired`, so per-source status
 * alone cannot tell a credentialed user from an uncredentialed one.
 * `now`/`storage` are injectable for tests.
 */
export function claimCredentialSpeedHint(
	sources: readonly HintSource[],
	now: number = Date.now(),
	storage?: Pick<Storage, "getItem" | "setItem">,
): boolean {
	const uncredentialedGithub = sources.some(
		(s) =>
			s.sourceType === "github" &&
			(s.credentialStatus === "missing" ||
				s.credentialStatus === "notRequired"),
	);
	if (!uncredentialedGithub) return false;

	try {
		// Resolved inside the try: merely touching `localStorage` can throw
		// (e.g. a WebView SecurityError), and that must stay silent too.
		const store = storage ?? localStorage;
		const raw = store.getItem(STORAGE_KEY);
		if (raw !== null) {
			const lastShown = Number(raw);
			if (
				Number.isFinite(lastShown) &&
				now - lastShown < SHOW_INTERVAL_MS
			) {
				return false;
			}
		}
		store.setItem(STORAGE_KEY, String(now));
		return true;
	} catch {
		// Storage unavailable → cannot throttle, so stay silent rather than nag.
		return false;
	}
}
