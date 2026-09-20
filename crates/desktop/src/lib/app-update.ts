/**
 * Pure pieces of the app-update flow, kept out of the provider so they can be
 * tested by `node --test` (the provider imports Tauri, which cannot load here).
 */

/** What the update flow is doing, as one value rather than four booleans. */
export type AppUpdatePhase =
	| "idle"
	| "checking"
	// Checked, and there is nothing to install. Distinct from `idle`, which is
	// "not checked yet" — the panel says different things for the two and used
	// to conflate them behind a single "no result yet".
	| "up-to-date"
	| "available"
	| "downloading"
	| "installed"
	| "error";

/**
 * Download progress as a percentage, or `null` for "indeterminate".
 *
 * `contentLength` is OPTIONAL in Tauri's `Started` event and a server may send
 * no `Content-Length` at all, so "unknown total" is a normal state, not an
 * error — a bar that renders `NaN%` or silently 0% for a download that is
 * really moving is worse than an indeterminate spinner. Clamped because a
 * proxy can send more bytes than it announced, and a bar that reports 103%
 * reads as a bug in the updater.
 */
export function downloadPercent(
	downloaded: number,
	contentLength: number | undefined,
): number | null {
	if (!contentLength || contentLength <= 0) return null;
	const percent = (downloaded / contentLength) * 100;
	return Math.min(100, Math.max(0, Math.round(percent)));
}

/**
 * May a new check or download start?
 *
 * The finding this guards: the About panel's mutations were local to the
 * component, so switching pages destroyed the observer while the download
 * carried on. Coming back reset the UI to "check for updates", and pressing it
 * started a SECOND download of the same update. The phase now lives in a
 * provider that never unmounts, and this is the one place that decides.
 *
 * `installed` is terminal on purpose: the bytes are already staged and the
 * only thing left is a restart, so re-running the flow would download an
 * update the app has.
 */
export function canStartUpdateWork(phase: AppUpdatePhase): boolean {
	return (
		phase === "idle" ||
		phase === "up-to-date" ||
		phase === "available" ||
		phase === "error"
	);
}
