/**
 * Pure helpers for remote git-credential forwarding.
 *
 * In remote mode the controller (this desktop) resolves git tokens from its
 * own keyring and forwards a per-source token to the remote VM `aghub-api`
 * over the SSH tunnel via the `X-Aghub-Git-Tokens` header. See
 * `docs/specs/2026-06-22-remote-credential-forwarding.md`.
 *
 * The encode + gating + map-assembly logic lives here as pure functions so it
 * can be unit-tested with `node --test` (with the Tauri `invoke` mocked).
 *
 * TOKEN HYGIENE: the resolved token is sensitive. It is resolved transiently
 * right before a request, encoded into a header, and discarded. It is NEVER
 * stored in React Query state/cache, a persisted store, a query key, a log, or
 * an error object.
 */

/** The forward header name (matches the remote API + CORS allowlist). */
export const GIT_TOKENS_HEADER = "X-Aghub-Git-Tokens";

/** The id of the implicit Local connection (kept in sync with LOCAL_CONNECTION). */
const LOCAL_CONNECTION_ID = "local";

/**
 * The Tauri `invoke` signature this module depends on, declared structurally so
 * the pure logic can be tested with a mock and stays free of a hard
 * `@tauri-apps/api/core` import in tests.
 */
export type InvokeFn = <T>(
	cmd: string,
	args?: Record<string, unknown>,
) => Promise<T>;

/** Mirrors the Rust `ResolvedOriginDto` returned by `resolve_git_token`. */
export interface ResolvedOrigin {
	scheme: string;
	host: string;
	port: number | null;
}

/** Mirrors the Rust `ResolvedTokenDto` returned by `resolve_git_token`. */
export interface ResolvedToken {
	token: string;
	origin: ResolvedOrigin | null;
}

/**
 * A single forward-map entry: the resolved token plus the origin it is pinned
 * to (the controller-resolved `(scheme, host, port)`). This is the wire shape
 * the Rust decoder parses — see `crates/api/src/credentials/forwarding.rs`.
 *
 * `origin` is non-sensitive metadata used by the remote to reject handing a
 * token to a same-host but different-`(scheme,port)` request; the `token` is
 * the only secret field.
 */
export interface ForwardedTokenEntry {
	token: string;
	origin: ResolvedOrigin | null;
}

/** The forward map sent in the header: `{ <sourceKey>: { token, origin } }`. */
export type ForwardedTokenMap = Record<string, ForwardedTokenEntry>;

/**
 * Encode a `{ source: { token, origin } }` map as base64(JSON) using the
 * STANDARD base64 alphabet (NOT url-safe) so it matches the Rust decoder in the
 * remote API.
 *
 * UTF-8 safe: tokens/sources may contain non-Latin1 code points, so the JSON
 * is first UTF-8 encoded to bytes before `btoa` (which only accepts binary
 * strings / Latin1).
 */
export function encodeGitTokensHeader(map: ForwardedTokenMap): string {
	const json = JSON.stringify(map);
	const bytes = new TextEncoder().encode(json);
	let binary = "";
	for (const byte of bytes) {
		binary += String.fromCharCode(byte);
	}
	return btoa(binary);
}

/**
 * Inputs to the forwarding gating predicate. Kept as a flat record so the
 * decision is a pure, easily-tested function of the active connection state.
 */
export interface ForwardingGateInput {
	/** The active connection id (`"local"` for the implicit Local connection). */
	activeConnectionId: string;
	/** Whether the active remote advertises `supports_credential_forwarding`. */
	supportsCredentialForwarding: boolean;
}

/**
 * Decide whether to attach the forward header for the active connection.
 *
 * True ONLY when ALL hold:
 *  (a) the active connection is remote (`id !== "local"`),
 *  (b) the active remote advertises the capability.
 *
 * The "request goes to the remote client, never local" condition (c) is
 * structurally guaranteed: the request layer uses the ACTIVE baseUrl's client
 * (via `useApi`), which is the remote tunnel exactly when (a) holds — so
 * gating on (a)+(b) here and resolving on the active client never forwards to
 * the local client.
 */
export function shouldForwardGitTokens({
	activeConnectionId,
	supportsCredentialForwarding,
}: ForwardingGateInput): boolean {
	return (
		activeConnectionId !== LOCAL_CONNECTION_ID &&
		supportsCredentialForwarding
	);
}

/**
 * Resolve a forward-token map for the given sources, invoking the
 * `resolve_git_token` Tauri command per source and SKIPPING sources with no
 * credential (a `null` result). The returned map is `{ source: { token, origin } }`
 * — the origin (from the command DTO) is carried through so the remote can
 * origin-pin the forwarded token.
 *
 * The `invoke` is injected so tests can mock it; production callers pass the
 * real `@tauri-apps/api/core` `invoke`.
 */
export async function resolveForwardedTokens(
	sources: string[],
	invokeFn: InvokeFn,
): Promise<ForwardedTokenMap> {
	const map: ForwardedTokenMap = {};
	for (const source of sources) {
		const resolved = await invokeFn<ResolvedToken | null>(
			"resolve_git_token",
			{ source },
		);
		// Skip nulls: a source with no controller-side binding is not forwarded
		// (least-privilege; the remote falls back to its keyring / unauthenticated).
		if (resolved && resolved.token) {
			map[source] = {
				token: resolved.token,
				origin: resolved.origin,
			};
		}
	}
	return map;
}

/**
 * Build the bound-source forward map for `check-updates`: enumerate the
 * explicitly-bound sources via `list_bound_sources`, then resolve a token per
 * bound source (skipping nulls). Returns an empty map when nothing is bound or
 * enumeration fails — forwarding then simply does not engage (graceful).
 */
export async function boundSourceTokenMap(
	invokeFn: InvokeFn,
): Promise<ForwardedTokenMap> {
	const bound = await invokeFn<string[]>("list_bound_sources");
	if (!bound || bound.length === 0) {
		return {};
	}
	return resolveForwardedTokens(bound, invokeFn);
}

/**
 * Build the per-request `ky` headers object carrying the forward header, or
 * `undefined` when the map is empty (no header attached — `ky` then sends no
 * `X-Aghub-Git-Tokens`). Centralizes the "empty map => no header" rule.
 */
export function gitTokensHeader(
	map: ForwardedTokenMap,
): Record<string, string> | undefined {
	if (Object.keys(map).length === 0) {
		return undefined;
	}
	return { [GIT_TOKENS_HEADER]: encodeGitTokensHeader(map) };
}
