import { toast } from "@heroui/react";
import { useQuery } from "@tanstack/react-query";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { claimCredentialSpeedHint } from "../lib/credential-speed-hint";
import { credentialsListQueryOptions } from "../requests/credentials";
import { useApi } from "./use-api";

interface CredentialSpeedHintInput {
	/** True only while an update check is actually running. */
	checking: boolean;
	/** Source rows visible on the page (any scope). */
	sources: readonly { sourceType: string; credentialStatus?: string }[];
}

/**
 * While an update check runs against GitHub sources and the user has stored
 * NO credentials at all, show a one-shot toast that adding one speeds up
 * checks. Throttled inside `claimCredentialSpeedHint` (localStorage, once a
 * week) so it never nags.
 *
 * The zero-credentials gate is queried client-side because the sources API
 * hardcodes `credentialStatus: "notRequired"` — per-source status cannot yet
 * distinguish a credentialed user from an uncredentialed one.
 */
// ponytail: any stored credential silences the hint (CredentialResponse has
// no host); switch to per-source status once the API computes it for real.
export function useCredentialSpeedHint({
	checking,
	sources,
}: CredentialSpeedHintInput) {
	const api = useApi();
	const { t } = useTranslation();

	// Only fetched while a check runs — the hint is pointless otherwise.
	const {
		data: credentials,
		isSuccess,
		isRefetchError,
		fetchStatus,
	} = useQuery(credentialsListQueryOptions({ api, enabled: checking }));

	useEffect(() => {
		if (!checking || sources.length === 0) return;
		// Trust only a SETTLED, successful credentials read: mid-(re)fetch the
		// cached value may belong to another connection, and toasting on it
		// would wrongly consume the weekly claim. Fetching/paused/error stay
		// silent.
		if (!isSuccess || isRefetchError || fetchStatus !== "idle") return;
		if (!credentials || credentials.length > 0) return;
		if (claimCredentialSpeedHint(sources)) {
			toast.info(t("credentialSpeedHint"));
		}
	}, [
		checking,
		sources,
		credentials,
		isSuccess,
		isRefetchError,
		fetchStatus,
		t,
	]);
}
