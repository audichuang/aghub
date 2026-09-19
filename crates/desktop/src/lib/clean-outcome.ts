/**
 * What a "clean up this removed skill" click may conclude from a delete's
 * `outcome`.
 *
 * Extracted from the Sources page so it can be tested: this one decision is
 * what the user's original report was about — the page reported success while
 * the skill was still listed, and refreshing never cleared it.
 */
export type CleanVerdict = "cleaned" | "lock-only" | "still-installed";

/**
 * The rows this runs for are built from the SCOPE'S LOCK, so `absent` does not
 * mean "nothing to do". It means the master is already off disk while the lock
 * key survives — the server answers `absent` with `prune: NotRun`, and the
 * refetch the click triggers rebuilds the very same row. Counting that as
 * cleaned is the false success; dropping the key belongs to the orphan-lock
 * banner, whose prune is SCOPE-WIDE and therefore stays a disclosed click
 * rather than a side effect of this one.
 *
 * `kept` (a shared master another agent still reads) and `partial` mean the
 * skill is still installed. `executed` alone can tell none of the three apart,
 * which is why this reads `outcome`.
 */
export function cleanVerdict(outcome: string | null | undefined): CleanVerdict {
	if (outcome === "removed") return "cleaned";
	if (outcome === "absent") return "lock-only";
	return "still-installed";
}
