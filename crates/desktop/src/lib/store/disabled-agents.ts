/**
 * The pure half of the per-connection agent selection, kept free of any
 * `@tauri-apps` import so the node test runner can reach it — `store/index.ts`
 * loads the Tauri store plugin at module scope, which a test process cannot.
 */

export const LOCAL_DISABLED_AGENTS_KEY = "disabledAgents";

/** Mirrors `projectsKey`: the bare key stays Local's, remotes get a suffix. */
export function disabledAgentsKey(connectionId: string): string {
	if (connectionId === "local") {
		return LOCAL_DISABLED_AGENTS_KEY;
	}
	return `${LOCAL_DISABLED_AGENTS_KEY}:${connectionId}`;
}

/**
 * Resolve one connection's selection from its own stored value and Local's.
 *
 * A remote that has never been configured inherits Local's selection, so the
 * choice made here carries over the first time you connect; the first toggle
 * on that remote materializes its own key and the two diverge from then on.
 *
 * The `undefined`/`null` check is load-bearing and must not become `?? []` or
 * a length test: a remote where the user re-enabled every inherited agent
 * stores `[]`, and treating that as "unconfigured" would fall back to Local
 * and silently disable them again. `disabled-agents.test.ts` pins that case.
 *
 * Local passes its own value as `localFallback` — `disabledAgentsKey("local")`
 * IS the Local key, so there is nothing else to read and no second store hit.
 */
export function resolveDisabledAgents(
	own: string[] | null | undefined,
	localFallback: string[] | null | undefined,
): string[] {
	if (own !== undefined && own !== null) return own;
	return localFallback ?? [];
}
