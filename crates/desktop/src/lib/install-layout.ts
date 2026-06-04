/** Skill install layout the user can pick in the Sources install flow. */
export type InstallLayout = "isolation" | "universal";

/**
 * Spec default is ISOLATION (copy the skill into each selected agent's own
 * skills dir, never touching `.agents`). UNIVERSAL (a single `.agents/skills`
 * master + per-agent symlinks, à la `npx skills`) is explicitly opt-in.
 *
 * The Sources page previously hardcoded `universal: true`, silently writing the
 * `.agents` layout for every install with no user choice — the opposite of the
 * spec default. This makes the choice explicit and the default safe.
 */
export const DEFAULT_INSTALL_LAYOUT: InstallLayout = "isolation";

/** Map a chosen layout to the `universal` flag of the git-install request. */
export function isUniversalLayout(layout: InstallLayout): boolean {
	return layout === "universal";
}
