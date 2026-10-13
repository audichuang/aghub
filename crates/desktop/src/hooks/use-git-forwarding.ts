import { invoke } from "@tauri-apps/api/core";
import { useCallback, useMemo } from "react";
import type { GitForwardHeaders } from "../lib/api";
import type { CheckSkillUpdatesParams } from "../requests/skills";
import {
	checkForwardSources,
	gitTokensHeader,
	resolveForwardedTokens,
	shouldForwardGitTokens,
} from "../lib/git-token-forwarding";
import { useApi } from "./use-api";
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
	const api = useApi();

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
	 * Build the forward header for `check-updates`: resolve a token for every
	 * lock source in the checked scope. Undefined when disabled, when nothing
	 * resolves, or when the source list cannot be read (the check then runs
	 * unforwarded, as before).
	 */
	const forCheck = useCallback(
		async (
			params?: CheckSkillUpdatesParams,
		): Promise<GitForwardHeaders | undefined> => {
			if (!enabled) return undefined;
			let sources: string[];
			try {
				const { sources: rows } = await api.skills.getSources(
					params?.scope ?? "global",
					params?.projectRoot,
				);
				sources = checkForwardSources(rows);
			} catch (error) {
				// Unforwarded, a private source reads `auth` again — leave a trace.
				console.warn(
					"check forwarding: source list unavailable",
					error,
				);
				return undefined;
			}
			// ponytail: one resolve_git_token IPC (two keyring reads) per source,
			// serially; batch into one command if large scopes make checks slow.
			return gitTokensHeader(
				await resolveForwardedTokens(sources, invoke),
			);
		},
		[enabled, api],
	);

	return { enabled, forSource, forCheck };
}
