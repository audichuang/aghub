export interface RemoteErrorPayload {
	kind?: string;
	stderr?: string;
	installHint?: string;
	remoteVersion?: string | null;
	remotePlatform?: string;
	hint?: string;
	message?: string;
}

/** Narrow an unknown invoke rejection to the kind-tagged RemoteError payload. */
export function asRemotePayload(
	error: unknown,
): RemoteErrorPayload | null {
	if (error && typeof error === "object" && "kind" in error) {
		return error as RemoteErrorPayload;
	}
	return null;
}

export function remoteErrorMessage(error: unknown): string {
	if (error instanceof Error) {
		return error.message;
	}
	if (typeof error === "string") {
		return error;
	}
	if (error == null || typeof error !== "object") {
		return String(error);
	}

	const remote = error as RemoteErrorPayload;
	switch (remote.kind) {
		case "unreachable":
			return remote.stderr ?? "SSH connection failed.";
		case "remoteApiMissing":
			return remote.installHint ?? "aghub-api is missing on the remote.";
		case "incompatible":
			return `Remote aghub-api version ${
				remote.remoteVersion ?? "unknown"
			} is incompatible.`;
		case "crossPlatformRedeploy":
			return (
				remote.hint ??
				`Remote platform ${
					remote.remotePlatform ?? "unknown"
				} differs from this desktop; cannot redeploy.`
			);
		case "startTimeout":
			return "Remote aghub-api did not start in time.";
		case "tunnelFailed":
			return remote.message ?? "SSH tunnel failed.";
		case "deployFailed":
			return remote.message ?? "Remote aghub-api install failed.";
		case "remoteDirectoryFailed":
			return remote.message ?? "Remote directory browsing failed.";
		case "alreadyConnecting":
			return "A connection attempt is already in progress.";
		case "internal":
			return remote.message ?? "Internal remote connection error.";
		default:
			try {
				return JSON.stringify(error);
			} catch {
				return String(error);
			}
	}
}

/** Strip ANSI escapes, split on newlines, return the last non-empty line. */
export function remoteOutputSummary(
	value: string | null | undefined,
): string {
	if (!value) return "";
	// eslint-disable-next-line no-control-regex
	const stripped = value.replace(/\x1B\[[0-9;]*[A-Za-z]/g, "");
	const lines = stripped.split("\n");
	for (let i = lines.length - 1; i >= 0; i--) {
		const trimmed = lines[i]?.trim();
		if (trimmed) return trimmed;
	}
	return "";
}
