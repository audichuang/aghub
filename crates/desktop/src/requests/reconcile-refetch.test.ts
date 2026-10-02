import assert from "node:assert/strict";
import { test } from "node:test";
import {
	MutationObserver,
	QueryClient,
	QueryObserver,
} from "@tanstack/react-query";
import type { ApiClient } from "./client.ts";
import { queryKeys } from "./keys.ts";
import { reconcileSkillsMutationOptions } from "./skills.ts";

test("failed reconcile still invalidates skills list", async () => {
	const client = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});

	let fetches = 0;
	const listKey = queryKeys.skills.list("global", undefined);

	const observer = new QueryObserver(client, {
		queryKey: listKey,
		queryFn: async () => {
			fetches += 1;
			return [];
		},
		staleTime: Number.POSITIVE_INFINITY,
	});
	const unsubscribe = observer.subscribe(() => {});

	// Wait for observer initial fetch
	await client.fetchQuery({
		queryKey: listKey,
		queryFn: async () => {
			fetches += 1;
			return [];
		},
	});
	const baseline = fetches;

	const api = {
		skills: {
			reconcile: async () => {
				throw new Error("boom");
			},
		},
	} as unknown as ApiClient;

	const mutation = new MutationObserver(
		client,
		reconcileSkillsMutationOptions({ api, queryClient: client }),
	);

	try {
		await mutation.mutate({
			source: {
				agent: "claude",
				scope: "global",
				project_root: null,
				name: "test-skill",
			},
			added: null,
			removed: ["claude"],
			confirm: true,
		});
	} catch {
		// Expected rejection
	}

	while (client.isFetching() > 0) {
		await new Promise((resolve) => setTimeout(resolve, 5));
	}

	assert.equal(
		fetches,
		baseline + 1,
		"skills list query should be refetched exactly once after reconcile failure",
	);

	unsubscribe();
});
