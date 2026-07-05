import { invoke } from "@tauri-apps/api/core";
import { useCallback, useMemo } from "react";
import type { GitForwardHeaders } from "../lib/api";
import {
	boundSourceTokenMap,
	gitTokensHeader,
	resolveForwardedTokens,
	shouldForwardGitTokens,
} from "../lib/git-token-forwarding";
import { useConnection } from "./use-connection";

/**
 * Per-request git-credential forwarding for the ACTIVE connection.
 *
 * Returns transient header builders that resolve a per-source token on the
 * controller (this Mac, in-process via `resolve_git_token`) and encode it into
 * the `X-Aghub-Git-Tokens` header — but ONLY when the active connection is a
 * remote that advertises the capability. In Local mode (or a non-capable
 * remote) every builder returns `undefined`, so no header is attached and the
 * request behaves exactly as before.
 *
 * TOKEN HYGIENE: the token is resolved fresh inside the returned builders
 * (called from a queryFn/mutationFn right before the request) and never stored,
 * cached, logged, or placed in a query key.
 */
export function useGitForwarding() {
	const { activeId, supportsCredentialForwarding } = useConnection();

	const enabled = useMemo(
		() =>
			shouldForwardGitTokens({
				activeConnectionId: activeId,
				supportsCredentialForwarding,
			}),
		[activeId, supportsCredentialForwarding],
	);

	/** Build the forward header for a single known source, or undefined. */
	const forSource = useCallback(
		async (source: string): Promise<GitForwardHeaders | undefined> => {
			if (!enabled) return undefined;
			const map = await resolveForwardedTokens([source], invoke);
			return gitTokensHeader(map);
		},
		[enabled],
	);

	/**
	 * Build the forward header for `check-updates`: enumerate bound sources and
	 * resolve a token per source. Returns undefined when disabled or nothing is
	 * bound.
	 */
	const forBoundSources = useCallback(async (): Promise<
		GitForwardHeaders | undefined
	> => {
		if (!enabled) return undefined;
		const map = await boundSourceTokenMap(invoke);
		return gitTokensHeader(map);
	}, [enabled]);

	return { enabled, forSource, forBoundSources };
}
