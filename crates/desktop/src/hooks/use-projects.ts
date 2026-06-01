import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { Project } from "../lib/store";
import { addProject, getProjects, removeProject } from "../lib/store";
import { useConnection } from "./use-connection";

function projectsQueryKey(connectionId: string) {
	return ["projects", connectionId] as const;
}

export function useProjects() {
	const { activeId } = useConnection();

	return useQuery<Project[]>({
		queryKey: projectsQueryKey(activeId),
		queryFn: () => getProjects(activeId),
	});
}

export function useAddProject() {
	const { activeId } = useConnection();
	const queryClient = useQueryClient();

	return useMutation({
		mutationFn: (project: Omit<Project, "id">) =>
			addProject({ connectionId: activeId, project }),
		onSuccess: () =>
			queryClient.invalidateQueries({
				queryKey: projectsQueryKey(activeId),
			}),
	});
}

export function useRemoveProject() {
	const { activeId } = useConnection();
	const queryClient = useQueryClient();

	return useMutation({
		mutationFn: (id: string) =>
			removeProject({ connectionId: activeId, id }),
		onSuccess: () =>
			queryClient.invalidateQueries({
				queryKey: projectsQueryKey(activeId),
			}),
	});
}
