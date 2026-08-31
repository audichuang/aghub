import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { applyTagOp, type SkillTags } from "../lib/skill-tags";
import { getSkillTags, setSkillTags } from "../lib/store";

export const SKILL_TAGS_QUERY_KEY = ["skillTags"] as const;

/** Read/write the local skill-tag overlay. Mirrors `useFavorites`: the query
 * cache is the read model, and every write goes through one function so the
 * list and the dialogs never disagree. */
export function useSkillTags() {
	const queryClient = useQueryClient();

	const { data: tags = {} } = useQuery({
		queryKey: SKILL_TAGS_QUERY_KEY,
		queryFn: getSkillTags,
	});

	const tagsFor = useCallback(
		(name: string): string[] => tags[name] ?? [],
		[tags],
	);

	const applyTag = useCallback(
		async (names: string[], op: "add" | "remove", tag: string) => {
			// Read AND advance the cache SYNCHRONOUSLY, before awaiting the
			// store. Only reading it is not enough: the cache would move after
			// the await, so two quick clicks would both derive from the
			// pre-edit value and the second write would discard the first.
			const before =
				queryClient.getQueryData<SkillTags>(SKILL_TAGS_QUERY_KEY) ??
				tags;
			const next = applyTagOp(before, names, op, tag);
			if (next === before) return before; // blank tag / empty selection
			queryClient.setQueryData(SKILL_TAGS_QUERY_KEY, next);
			try {
				await setSkillTags(next);
			} catch (error) {
				// Re-read rather than restoring `before`: another edit may have
				// landed while this one was in flight, and the store is the
				// authority on what actually persisted.
				queryClient.setQueryData(
					SKILL_TAGS_QUERY_KEY,
					await getSkillTags(),
				);
				throw error;
			}
			return next;
		},
		[queryClient, tags],
	);

	return { tags, tagsFor, applyTag };
}
