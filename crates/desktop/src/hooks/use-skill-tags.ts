import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { applyTagOp, type SkillTags } from "../lib/skill-tags";
import { getSkillTags, setSkillTags } from "../lib/store";

export const SKILL_TAGS_QUERY_KEY = ["skillTags"] as const;

/** Read/write the local skill-tag overlay. Mirrors `useFavorites`: the query
 * cache is the read model, and every write goes through one mutation so the
 * list and the dialogs never disagree. */
export function useSkillTags() {
	const queryClient = useQueryClient();

	const { data: tags = {} } = useQuery({
		queryKey: SKILL_TAGS_QUERY_KEY,
		queryFn: getSkillTags,
	});

	const { mutateAsync: write } = useMutation({
		mutationFn: async (next: SkillTags) => {
			await setSkillTags(next);
			return next;
		},
		onSuccess: (next) => {
			queryClient.setQueryData(SKILL_TAGS_QUERY_KEY, next);
		},
	});

	const tagsFor = useCallback(
		(name: string): string[] => tags[name] ?? [],
		[tags],
	);

	const applyTag = useCallback(
		(names: string[], op: "add" | "remove", tag: string) => {
			// Read the CACHE, not this render's `tags`: two quick clicks in the
			// dialog would otherwise both derive from the same stale snapshot
			// and the second write would silently discard the first.
			const current =
				queryClient.getQueryData<SkillTags>(SKILL_TAGS_QUERY_KEY) ??
				tags;
			return write(applyTagOp(current, names, op, tag));
		},
		[queryClient, tags, write],
	);

	return { tags, tagsFor, applyTag };
}
