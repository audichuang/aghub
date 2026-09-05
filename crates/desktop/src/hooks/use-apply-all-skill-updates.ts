import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { ApplySkillUpdateResponse } from "../generated/dto";
import { useApi } from "./use-api";
import { useGitForwarding } from "./use-git-forwarding";
import { sendInBatches } from "../lib/batch-names";
import { describeRequestFailure } from "../lib/request-failure";
import { queryKeys } from "../requests/keys";
import {
	applySkillUpdatesMutationOptions,
	invalidateSkillQueries,
} from "../requests/skills";

/**
 * One source's worth of updates. `POST /skills/apply-updates` fetches ONE
 * source per request, so a caller holding outdated skills from several sources
 * must send one batch per source.
 */
export interface SourceUpdateBatch {
	/** Clone URL when the lock recorded one, else the host-blind `owner/repo`. */
	source: string;
	names: string[];
	scope: "global" | "project";
	projectRoot: string | null;
}

export interface ApplyAllOutcome {
	/** Per-skill rows the server actually answered. */
	results: ApplySkillUpdateResponse[];
	failures: ApplySkillUpdateResponse[];
	updated: number;
	/**
	 * A transport error or timeout, which proves NOTHING about what was
	 * written: the server does not abort with the client, it holds the
	 * mutation lock to completion. The caller must not claim a failure count
	 * when this is set.
	 */
	unconfirmed: boolean;
	/**
	 * Skills a DEFINITE error (a 4xx, answered before the handler ran) left
	 * unwritten. Separate from `failures`, which are per-skill rows the server
	 * actually returned — and non-zero here must never be reported as success.
	 */
	definiteFailureCount: number;
	failureDescription?: string;
}

/**
 * Apply every pending skill update across one or more sources.
 *
 * This lives in a hook rather than in a component because BOTH skill views
 * need it — the source view updates one source, the agent view updates every
 * source at once — and a hand-mirrored copy of the batching and the
 * transport-error rules would drift (root AGENTS.md, "NEVER hand-mirror").
 */
export function useApplyAllSkillUpdates() {
	const api = useApi();
	const queryClient = useQueryClient();
	const { forSource: forwardForSource } = useGitForwarding();
	const [isApplying, setIsApplying] = useState(false);

	const applyUpdatesMutation = useMutation(
		applySkillUpdatesMutationOptions({
			api,
			queryClient,
			forwardForSource,
		}),
	);

	const applyAll = async (
		batches: readonly SourceUpdateBatch[],
	): Promise<ApplyAllOutcome | null> => {
		const pending = batches.filter((batch) => batch.names.length > 0);
		if (pending.length === 0 || isApplying) return null;

		setIsApplying(true);
		const results: ApplySkillUpdateResponse[] = [];
		try {
			for (const batch of pending) {
				// Batched, not one body: the server REFUSES an oversized batch
				// rather than truncating it, so a source with more outdated
				// skills than one batch holds would fail entirely instead of
				// updating anything. Ordering and cap live in `sendInBatches`,
				// which is tested.
				await sendInBatches<ApplySkillUpdateResponse>(
					batch.names,
					async (names) => {
						const response = await applyUpdatesMutation.mutateAsync(
							{
								body: {
									source: batch.source,
									names,
									scope: batch.scope,
									projectRoot: batch.projectRoot,
									confirm: true,
								},
								sourceUrl: batch.source,
							},
						);
						return response.results;
					},
					// Accumulate per CHUNK, not per batch: a throw on a later
					// chunk of the same source must not discard the rows the
					// server already returned for the earlier ones.
					(rows) => results.push(...rows),
				);
			}
			const failures = results.filter((result) => !result.success);
			await queryClient.invalidateQueries({
				queryKey: queryKeys.skills.sources.all(),
			});
			return {
				results,
				failures,
				updated: results.length - failures.length,
				unconfirmed: false,
				definiteFailureCount: 0,
			};
		} catch (error) {
			// A 4xx was answered before the handler ran, so nothing was written
			// and reporting failure is accurate. A timeout or transport error
			// proves nothing: the server does not abort with the client (it
			// holds the mutation lock to completion), so claiming N failures
			// would be a lie the user acts on.
			const failure = describeRequestFailure(error);
			// Not the broad `skills.all()` invalidate: that one AWAITS every
			// active refetch (the 120s check and source diff) before the caller
			// re-enables its buttons, over the connection that just failed.
			await invalidateSkillQueries(queryClient);
			void queryClient.invalidateQueries({
				queryKey: queryKeys.skills.sources.all(),
			});
			const requested = pending.reduce(
				(sum, batch) => sum + batch.names.length,
				0,
			);
			// Rows an EARLIER batch already returned are real outcomes; a later
			// batch's 4xx must not erase them, or a run that updated five and
			// failed three reports only the three.
			const answered = results.filter((result) => !result.success);
			return {
				results,
				failures: answered,
				updated: results.length - answered.length,
				unconfirmed: !failure.definite,
				// Only a DEFINITE error licenses a count. Reporting 0 here
				// while `unconfirmed` is set would read as "nothing failed".
				definiteFailureCount: failure.definite
					? Math.max(1, requested - results.length)
					: 0,
				failureDescription: failure.description,
			};
		} finally {
			setIsApplying(false);
		}
	};

	return { applyAll, isApplying };
}
