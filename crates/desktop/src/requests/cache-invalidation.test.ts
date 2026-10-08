import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; these use Node's
// built-in runner, matching the other desktop tests. They drive a REAL
// QueryClient — the thing under test is which cached answers a mutation
// invalidates, which is not observable from the mutation's return value.
import { test } from "node:test";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import type { ApiClient } from "./client.ts";
import {
	createCredentialMutationOptions,
	deleteCredentialMutationOptions,
} from "./credentials.ts";
import { queryKeys } from "./keys.ts";
import {
	applySkillUpdateMutationOptions,
	invalidateSkillQueries,
} from "./skills.ts";
import { deleteSkill } from "./delete-skill.ts";

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
	const repair = seedActive(client, queryKeys.skills.repairPreview("global"));
	await content.settled;
	await tree.settled;
	await repair.settled;
	const before = {
		content: content.state.fetches,
		tree: tree.state.fetches,
		repair: repair.state.fetches,
	};

	const api = {
		skills: { applyUpdate: async () => ({ success: true }) },
	} as unknown as ApiClient;
	const options = applySkillUpdateMutationOptions({
		api,
		queryClient: client,
	});
	await options.onSuccess?.(
		{ success: true } as any,
		{ body: {} as any },
		undefined,
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
	assert.ok(
		repair.state.fetches > before.repair,
		"updating divergent copies must refresh the migration conflict preview",
	);
	repair.unsubscribe();
});

// Unticking a skill's last agent (a reconcile) is what makes it withheld. The
// skills page never unmounts, so if every skill mutation did not actively
// refetch the withheld list, its status-strip row would not appear until some
// unrelated refetch happened to run.
test("every skill mutation refetches the withheld-skill list", async () => {
	const client = freshClient();
	const withheld = seedActive(client, queryKeys.skills.withheld("global"));
	await withheld.settled;
	const before = withheld.state.fetches;

	await invalidateSkillQueries(client);
	await new Promise((resolve) => setTimeout(resolve, 50));

	assert.ok(
		withheld.state.fetches > before,
		"the withheld list must refetch after a skill mutation",
	);
	withheld.unsubscribe();
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

test("a credential change does not block on any network refetch", async () => {
	// Both are HTTP round trips that can hang: the source diff clones the repo
	// (120s timeout), the credential list carries a 10s timeout plus a retry.
	for (const [label, queryKey] of [
		["source diff", queryKeys.skills.sources.diff("owner/repo")],
		["credential list", queryKeys.credentials.list()],
	] as const) {
		const client = freshClient();
		const hanging = new QueryObserver(client, {
			queryKey: [...queryKey],
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
			(options.onSuccess as any)?.(undefined, "c1", undefined, {}).then(
				() => "done",
			),
			new Promise((resolve) => setTimeout(resolve, 300, "hung")),
		]);

		assert.equal(
			settled,
			"done",
			`the mutation must resolve without awaiting the ${label} refetch — otherwise the dialog sits pending long after the write succeeded`,
		);
		unsubscribe();
	}
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

test("deleteSkill centralizes invalidation in the request layer and refetches non-blocking", async () => {
	const client = freshClient();
	const list = seedActive(client, queryKeys.skills.list("global"));
	const withheld = seedActive(client, queryKeys.skills.withheld("global"));
	await list.settled;
	await withheld.settled;
	const before = {
		list: list.state.fetches,
		withheld: withheld.state.fetches,
	};

	const api = {
		skills: {
			deleteByPath: async () => ({
				success: true,
				dry_run: false,
				executed: true,
				needs_confirm: false,
				paths: ["/path"],
				skipped: [],
				deleted_path: "/path",
				outcome: "removed",
			}),
		},
	} as any;

	await deleteSkill({
		api,
		queryClient: client,
		name: "my-skill",
		sourcePath: "/path",
		intent: { kind: "by-path", sourcePath: "/path", agents: ["claude"] },
	});

	await new Promise((resolve) => setTimeout(resolve, 50));

	assert.ok(
		list.state.fetches > before.list,
		"the skill list must refetch after deleteSkill",
	);
	assert.ok(
		withheld.state.fetches > before.withheld,
		"the withheld list must refetch after deleteSkill",
	);
	list.unsubscribe();
	withheld.unsubscribe();
});

test("deleteSkill invalidates queries even on partial deletion failure", async () => {
	const client = freshClient();
	const list = seedActive(client, queryKeys.skills.list("global"));
	await list.settled;
	const before = list.state.fetches;

	const api = {
		skills: {
			deleteByPath: async () => ({
				success: false,
				dry_run: false,
				executed: true,
				needs_confirm: false,
				paths: ["/path1"],
				skipped: ["/path2"],
				deleted_path: "/path1",
				outcome: "partial",
			}),
		},
	} as any;

	const result = await deleteSkill({
		api,
		queryClient: client,
		name: "my-skill",
		sourcePath: "/path",
		intent: { kind: "by-path", sourcePath: "/path", agents: ["claude"] },
	});

	assert.equal(result.success, false);
	assert.equal(result.verdict, "partial");

	await new Promise((resolve) => setTimeout(resolve, 50));

	assert.ok(
		list.state.fetches > before,
		"skill queries must be invalidated on partial delete to reflect partially removed paths",
	);
	list.unsubscribe();
});
