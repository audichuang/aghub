import { isHTTPError } from "ky";

/** The detail panel's path is gone from disk (content/tree answered 404). */
export function isGoneSkillPath(error: unknown): boolean {
	return isHTTPError(error) && error.response.status === 404;
}

/** One line per failed row, so a partial delete names who failed and why. */
export function failedReconcileRowsMessage(
	results: readonly {
		agent: string;
		success: boolean;
		error?: string | null;
	}[],
	agentName: (id: string) => string,
): string | null {
	const failed = results.filter((row) => !row.success);
	if (failed.length === 0) return null;
	return failed
		.map((row) => `${agentName(row.agent)}: ${row.error ?? "failed"}`)
		.join("\n");
}

/**
 * The server refused the whole reconcile before writing anything — the
 * preflight verdict (core `transfer.rs`: "... preflight failed; nothing was
 * written"), as opposed to a row that failed at runtime.
 */
export function isWholeBatchRefusal(error: unknown): boolean {
	return (
		error instanceof Error && error.message.includes("nothing was written")
	);
}
