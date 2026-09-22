import type { ApplyAllOutcome } from "../hooks/use-apply-all-skill-updates";

export interface ApplyAllToast {
	kind: "danger" | "success";
	title: string;
	description?: string;
	/** A failure must outlive the default auto-dismiss: the status strip keeps
	 * showing the skill as updatable and nothing else says why. */
	persistent: boolean;
}

type Translate = (key: string, options?: { count?: number }) => string;

/**
 * The ONE reading of an "update all" outcome, shared by the agent view and
 * the source view — both used to hand-mirror it, and both named only the
 * first failure without saying which skill it belonged to.
 */
export function applyAllToast(
	outcome: ApplyAllOutcome,
	t: Translate,
): ApplyAllToast {
	const lines = outcome.failures.map(
		(failure) => `${failure.name}: ${failure.error ?? "unknown error"}`,
	);
	if (outcome.failureDescription) lines.push(outcome.failureDescription);
	const description = lines.length > 0 ? lines.join("\n") : undefined;

	if (outcome.unconfirmed) {
		// Rows an earlier batch answered are confirmed; only what came after
		// the transport failure is unknown.
		return {
			kind: "danger",
			title:
				outcome.updated > 0
					? t("sourceUpdatePartialUnconfirmed", {
							count: outcome.updated,
						})
					: t("sourceUpdateUnconfirmed"),
			description,
			persistent: true,
		};
	}
	const failureCount = outcome.failures.length + outcome.definiteFailureCount;
	if (failureCount > 0) {
		return {
			kind: "danger",
			title:
				failureCount === 1
					? t("sourceUpdateSomeFailedOne", { count: 1 })
					: t("sourceUpdateSomeFailedMany", { count: failureCount }),
			description,
			persistent: true,
		};
	}
	return {
		kind: "success",
		title: t("sourceUpdatesApplied", { count: outcome.updated }),
		description: undefined,
		persistent: false,
	};
}
