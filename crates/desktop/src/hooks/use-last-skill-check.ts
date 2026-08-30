import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";

/** The sidecar the SCHEDULED CLI writes (`check --write-result`). Counts only:
 * the scheduled run is read-only and never tells the app which skill changed —
 * that still comes from the app's own check. */
export interface LastSkillCheck {
	startedAt?: string;
	finishedAt?: string;
	updateAvailable?: number;
	failed?: number;
	/** Private sources the schedule cannot reach: the CLI resolves tokens from
	 * GIT_PASSWORD / GITHUB_TOKEN, never from the desktop keyring. */
	needsAuth?: number;
	skipped?: number;
}

export const LAST_SKILL_CHECK_QUERY_KEY = ["skill-check-last"] as const;

export function useLastSkillCheck() {
	return useQuery({
		queryKey: LAST_SKILL_CHECK_QUERY_KEY,
		queryFn: async () =>
			invoke<LastSkillCheck | null>("get_last_skill_check"),
		retry: false,
	});
}

/**
 * How many updates the BACKGROUND run found that the app has not seen yet.
 *
 * `null` hides the banner. The sidecar is only news while it is NEWER than the
 * app's own last check — otherwise a foreground refresh that already surfaced
 * (or cleared) those rows would be shouted down by a stale file on disk.
 */
export function backgroundUpdateNews(
	last: LastSkillCheck | null | undefined,
	appLastChecked: Date | null,
): number | null {
	const available = last?.updateAvailable ?? 0;
	if (available <= 0) return null;
	const finished = last?.finishedAt
		? Date.parse(last.finishedAt)
		: Number.NaN;
	if (Number.isNaN(finished)) return null;
	if (appLastChecked && finished <= appLastChecked.getTime()) return null;
	return available;
}
