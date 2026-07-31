/**
 * A Sources row is identified by its repository ORIGIN
 * (`host[:port]/owner/repo`), while a skill's lock entry records the host-blind
 * `owner/repo`. Navigating from a skill's source group to its Sources row
 * therefore cannot compare the two strings directly — doing so silently matched
 * nothing and left the "open Sources view" button doing nothing at all.
 *
 * Matching on the origin's tail keeps the origin algorithm in Rust, where three
 * call sites already share one definition; recomputing it here would fork that.
 *
 * ponytail: with two forges serving one `owner/repo` this can pick either row.
 * It is a navigation jump, so landing on the sibling is a tolerable miss — carry
 * the origin on the lock-entry DTO if it ever needs to be exact.
 */
export function matchesLockSource(rowSource: string, lockSource: string) {
	if (rowSource === lockSource) return true;
	if (!rowSource.endsWith(`/${lockSource}`)) return false;
	// What precedes the lock source must be exactly the authority — one segment,
	// no slash. The lock records the FULL repository path, so anything left over
	// means this is a partial tail (`repo` inside `owner/repo`), not a match.
	const authority = rowSource.slice(0, -(lockSource.length + 1));
	return authority.length > 0 && !authority.includes("/");
}
