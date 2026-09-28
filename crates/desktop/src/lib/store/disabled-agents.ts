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

/**
 * Where the selection comes from now that the SERVER owns it
 * (`aghub_core::agent_settings`): every server-side fan-out reads the same
 * answer, so a selection kept only here would be ignored by repair, update,
 * rename and delete-from-all.
 */
export interface DisabledAgentsSource {
	/** `null` when the server predates the setting (an older remote). */
	read(): Promise<{ agents: string[]; configured: boolean } | null>;
	write(agents: string[]): Promise<{ agents: string[] }>;
	/** The pre-server, per-connection selection; `null` if never saved. */
	legacy(): Promise<string[] | null>;
	/** Ids the server knows — a stale legacy id would be a 400. */
	knownIds: ReadonlySet<string>;
	/** Agents detected on that machine right now. */
	availableIds: ReadonlySet<string>;
}

/**
 * The effective selection. A server that has never been configured is seeded
 * ONCE: from the legacy selection when there is one (a remote's own key, else
 * Local's — the old first-connect inheritance), so upgrading changes nobody's
 * choice; otherwise with only the agents detected right now turned on. The
 * server stores an allow-list, so any agent that appears LATER stays off until
 * the user turns it on.
 */
export async function loadDisabledAgents(
	source: DisabledAgentsSource,
): Promise<string[]> {
	const stored = await source.read();
	if (stored === null) return (await source.legacy()) ?? [];
	if (stored.configured) return stored.agents;
	const legacy = await source.legacy();
	const seed =
		legacy === null
			? [...source.knownIds].filter((id) => !source.availableIds.has(id))
			: legacy.filter((id) => source.knownIds.has(id));
	return (await source.write(seed)).agents;
}

/** Legacy read: `null` when neither key was ever written. */
export function resolveLegacyDisabledAgents(
	own: string[] | null | undefined,
	localFallback: string[] | null | undefined,
): string[] | null {
	if (own == null && localFallback == null) return null;
	return resolveDisabledAgents(own, localFallback);
}
