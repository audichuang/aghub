import { isHTTPError } from "ky";

export interface RequestFailureView {
	/**
	 * True when the server provably did nothing. A 4xx is answered by request
	 * validation before the handler runs, so a mutation behind it definitely
	 * did not happen. A timeout or transport error proves nothing: the server
	 * does not abort when the client goes away — a mutation route holds its
	 * lock to completion — so the work may well have been applied.
	 */
	definite: boolean;
	/**
	 * The server's own message, safe to show. Absent for transport failures:
	 * ky only rewrites `HTTPError` messages from the response body, so a
	 * `TimeoutError` would surface its internal
	 * `Request timed out: POST http://127.0.0.1:<port>/...` instead.
	 */
	description?: string;
}

/** Classify a failed API request for user-facing reporting. */
export function describeRequestFailure(error: unknown): RequestFailureView {
	if (isHTTPError(error)) {
		return {
			definite: error.response.status < 500,
			description: error.message || undefined,
		};
	}
	return { definite: false };
}
