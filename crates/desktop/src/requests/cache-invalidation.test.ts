import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; these use Node's
// built-in runner, matching the other desktop tests. They drive a REAL
// QueryClient — the thing under test is which cached answers a mutation
// invalidates, which is not observable from the mutation's return value.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import type { ApiClient } from "./client.ts";
import {
	createCredentialMutationOptions,
	deleteCredentialMutationOptions,
} from "./credentials.ts";
import { queryKeys } from "./keys.ts";
import { applySkillUpdateMutationOptions } from "./skills.ts";

/** A client whose queries never refetch on their own, so staleness is visible. */
function freshClient() {
	return new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});
}

/**
 * Seed a query AND keep an observer on it, because `type: "active"` refetching
 * only reaches queries something is currently rendering. Returns a counter of
 * how many times the queryFn ran.
 */
function seedActive(client: QueryClient, queryKey: readonly unknown[]) {
	const state = { fetches: 0 };
	const observer = new QueryObserver(client, {
		queryKey: [...queryKey],
		queryFn: async () => {
			state.fetches += 1;
			return "value";
		},
		staleTime: Number.POSITIVE_INFINITY,
	});
	// Subscribing is what makes the query ACTIVE — `type: "active"` refetching
	// reaches nothing otherwise — and it triggers the initial fetch.
	const unsubscribe = observer.subscribe(() => {});
	return {
		state,
		settled: observer.refetch().then(() => state),
		unsubscribe,
	};
}

test("applying an update refetches the open skill's content and file tree", async () => {
	const client = freshClient();
	const content = seedActive(client, queryKeys.skills.content("a/SKILL.md"));
	const tree = seedActive(client, queryKeys.skills.tree("a/SKILL.md"));
	await content.settled;
	await tree.settled;
	const before = {
		content: content.state.fetches,
		tree: tree.state.fetches,
	};

	const api = {
		skills: { applyUpdate: async () => ({ success: true }) },
	} as unknown as ApiClient;
	const options = applySkillUpdateMutationOptions({
		api,
		queryClient: client,
	});
	await options.onSuccess?.(
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		{ success: true } as any,
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		{ body: {} as any },
		undefined,
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		{} as any,
	);
	// The refetches are deliberately fire-and-forget, so let them land.
	await new Promise((resolve) => setTimeout(resolve, 50));

	assert.ok(
		content.state.fetches > before.content,
		"SKILL.md content must refetch — the path is unchanged, so nothing else brings the open panel up to date",
	);
	assert.ok(
		tree.state.fetches > before.tree,
		"the file tree must refetch for the same reason",
	);
	content.unsubscribe();
	tree.unsubscribe();
});

test("a credential change invalidates the source answers computed with it", async () => {
	for (const mutation of ["create", "delete"] as const) {
		const client = freshClient();
		const diffKey = queryKeys.skills.sources.diff("owner/repo");
		const checksKey = queryKeys.skills.updateChecks();
		client.setQueryData([...diffKey], "cached-diff");
		client.setQueryData([...checksKey], "cached-checks");

		const api = {
			credentials: {
				create: async () => ({ id: "c1", name: "github.com" }),
				delete: async () => undefined,
			},
		} as unknown as ApiClient;
		const options =
			mutation === "create"
				? createCredentialMutationOptions({ api, queryClient: client })
				: deleteCredentialMutationOptions({ api, queryClient: client });
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		await (options.onSuccess as any)?.(
			{ id: "c1", name: "github.com" },
			"c1",
			undefined,
			{},
		);

		for (const [label, key] of [
			["source diff", diffKey],
			["update checks", checksKey],
		] as const) {
			assert.equal(
				client.getQueryState([...key])?.isInvalidated,
				true,
				`${label} must be invalidated on credential ${mutation}: the backend resolves a source's token from the credential store (including a name-matches-host fallback), so the cached answer stops being true`,
			);
		}
	}
});

test("a credential change does not block on a source diff network refetch", async () => {
	const client = freshClient();
	// An active source diff whose refetch never settles — the shape of a real
	// one, which clones the repo behind a 120s timeout.
	const hanging = new QueryObserver(client, {
		queryKey: [...queryKeys.skills.sources.diff("owner/repo")],
		queryFn: () => new Promise(() => {}),
	});
	const unsubscribe = hanging.subscribe(() => {});

	const api = {
		credentials: { delete: async () => undefined },
	} as unknown as ApiClient;
	const options = deleteCredentialMutationOptions({
		api,
		queryClient: client,
	});

	const settled = await Promise.race([
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		(options.onSuccess as any)?.(undefined, "c1", undefined, {}).then(
			() => "done",
		),
		new Promise((resolve) => setTimeout(() => resolve("hung"), 300)),
	]);

	assert.equal(
		settled,
		"done",
		"the mutation must resolve without awaiting the diff refetch — otherwise the bind dialog sits pending long after the write succeeded",
	);
	unsubscribe();
});

test("the git-credential probe is keyed per connection and outside the skills namespace", () => {
	const a = queryKeys.gitCredentialStatus.of("https://x/y.git", "vm-a");
	const b = queryKeys.gitCredentialStatus.of("https://x/y.git", "vm-b");
	assert.notDeepEqual(
		a,
		b,
		"the same URL on two hosts must not share a cache entry — the answer is about the machine running aghub-api",
	);
	assert.notEqual(
		a[0],
		"skills",
		"parked under `skills`, every skill mutation would sweep it stale and re-run `git credential fill`",
	);
});
