import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";
import {
	PreferenceNotReadError,
	preferenceWriteBasis,
	writePreference,
} from "../lib/preference-write";
import {
	getStarredMcps,
	getStarredSkills,
	setStarredMcps,
	setStarredSkills,
} from "../lib/store";

export function useFavorites() {
	const queryClient = useQueryClient();

	// The `= []` defaults are for RENDERING only. A failed read also lands
	// there, and treating it as "nothing is starred" is how one click used to
	// wipe every star: the toggle wrote `[thatOne]` over the real list. The
	// `isSuccess` flags below are what the writes gate on.
	const { data: starredSkills = [], isSuccess: skillsRead } = useQuery({
		queryKey: ["starredSkills"],
		queryFn: getStarredSkills,
	});

	const { data: starredMcps = [], isSuccess: mcpsRead } = useQuery({
		queryKey: ["starredMcps"],
		queryFn: getStarredMcps,
	});

	const starredSkillsSet = useMemo(
		() => new Set(starredSkills),
		[starredSkills],
	);
	const starredMcpsSet = useMemo(() => new Set(starredMcps), [starredMcps]);

	const isSkillStarred = useCallback(
		(name: string) => starredSkillsSet.has(name),
		[starredSkillsSet],
	);

	const isMcpStarred = useCallback(
		(mergeKey: string) => starredMcpsSet.has(mergeKey),
		[starredMcpsSet],
	);

	const toggleSkillStar = useCallback(
		async (name: string) => {
			const basis = preferenceWriteBasis(
				skillsRead,
				queryClient.getQueryData(["starredSkills"]) as
					| string[]
					| undefined,
				[],
			);
			if (basis === null) {
				throw new PreferenceNotReadError("starredSkills");
			}

			const next = new Set(basis);
			if (next.has(name)) next.delete(name);
			else next.add(name);

			await writePreference({
				previous: basis,
				next: Array.from(next),
				setCache: (value) =>
					queryClient.setQueryData(["starredSkills"], value),
				save: setStarredSkills,
			});
		},
		[skillsRead, queryClient],
	);

	const toggleMcpStar = useCallback(
		async (mergeKey: string) => {
			const basis = preferenceWriteBasis(
				mcpsRead,
				queryClient.getQueryData(["starredMcps"]) as
					| string[]
					| undefined,
				[],
			);
			if (basis === null) {
				throw new PreferenceNotReadError("starredMcps");
			}

			const next = new Set(basis);
			if (next.has(mergeKey)) next.delete(mergeKey);
			else next.add(mergeKey);

			await writePreference({
				previous: basis,
				next: Array.from(next),
				setCache: (value) =>
					queryClient.setQueryData(["starredMcps"], value),
				save: setStarredMcps,
			});
		},
		[mcpsRead, queryClient],
	);

	return {
		starredSkills: starredSkillsSet,
		starredMcps: starredMcpsSet,
		isSkillStarred,
		isMcpStarred,
		toggleSkillStar,
		toggleMcpStar,
	};
}
