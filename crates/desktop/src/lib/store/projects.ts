import { getStore } from ".";
import type { Project } from "./types";

export const LOCAL_PROJECTS_KEY = "projects";

function projectsKey(connectionId: string): string {
	if (connectionId === "local") {
		return LOCAL_PROJECTS_KEY;
	}
	return `${LOCAL_PROJECTS_KEY}:${connectionId}`;
}

export async function getProjects(connectionId: string): Promise<Project[]> {
	const store = await getStore();
	return (await store.get<Project[]>(projectsKey(connectionId))) ?? [];
}

export interface AddProjectInput {
	connectionId: string;
	project: Omit<Project, "id">;
}

export async function addProject({
	connectionId,
	project,
}: AddProjectInput): Promise<Project> {
	const store = await getStore();
	const key = projectsKey(connectionId);
	const projects = await getProjects(connectionId);
	const newProject: Project = {
		...project,
		id: crypto.randomUUID(),
	};
	await store.set(key, [...projects, newProject]);
	await store.save();
	return newProject;
}

export interface RemoveProjectInput {
	connectionId: string;
	id: string;
}

export async function removeProject({
	connectionId,
	id,
}: RemoveProjectInput): Promise<void> {
	const store = await getStore();
	const key = projectsKey(connectionId);
	const projects = await getProjects(connectionId);
	await store.set(
		key,
		projects.filter((p) => p.id !== id),
	);
	await store.save();
}
