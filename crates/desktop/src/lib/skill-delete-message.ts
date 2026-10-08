import { isHTTPError } from "ky";
import { getApiErrorBody, getApiErrorCode } from "./api.ts";

/**
 * The localized text for a `kept` delete answer.
 *
 * The server's `error` on a `kept` answer is English and only ever set for a
 * git refusal (tracked by git, or git could not answer). Showing it verbatim
 * — `result.error || t(...)` — let that English string override all three
 * locales, so a git refusal read in English for every user. The localized
 * message wins; the server text only supplies the path it refers to.
 */
export interface KeptDeleteAnswer {
	error?: string | null;
	skipped: readonly string[];
}

export type KeptDeleteTranslate = (
	key: "deleteSkillKeptSharedMaster" | "deleteSkillKeptGit",
	options: { name: string; path?: string },
) => string;

export function keptDeleteMessage(
	answer: KeptDeleteAnswer,
	name: string,
	t: KeptDeleteTranslate,
): string {
	const path = answer.skipped[0];
	if (answer.error && path) {
		return t("deleteSkillKeptGit", { name, path });
	}
	return t("deleteSkillKeptSharedMaster", { name });
}

export function isSharedMasterRefusal(error: unknown): boolean {
	return (
		isHTTPError(error) &&
		error.response.status === 422 &&
		getApiErrorCode(error) === "UNSUPPORTED_OPERATION"
	);
}

export function keptDeleteMessageFromError(
	error: unknown,
	name: string,
	t: KeptDeleteTranslate,
): string | null {
	if (!isSharedMasterRefusal(error)) {
		return null;
	}
	const body = getApiErrorBody(error);
	const target =
		body?.rejected_targets?.find((t) => t.kind === "git") ??
		body?.rejected_targets?.[0];
	if (target?.kind === "git") {
		return t("deleteSkillKeptGit", { name, path: target.path });
	}
	return t("deleteSkillKeptSharedMaster", { name });
}
