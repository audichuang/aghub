import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this drives a REAL
// QueryClient with Node's built-in runner, like cache-invalidation.test.ts.
import { test } from "node:test";
import {
	MutationObserver,
	QueryClient,
	QueryObserver,
} from "@tanstack/react-query";
import type { ApiClient } from "./client.ts";
import { queryKeys } from "./keys.ts";
import {
	applySkillUpdatesMutationOptions,
	refetchUpdateChecksAfterWrites,
} from "./skills.ts";

function deferred() {
	let resolve!: () => void;
	const promise = new Promise<void>((r) => {
		resolve = r;
	});
	return { promise, resolve };
}

// The incident's second half: every batch's refetch joined the ONE check that
// started after the first batch, so the badge settled on an answer computed
// before the later batches wrote — and stayed there for the 10-min staleTime.
test("the final check starts after the last write, even when one is in flight", async () => {
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false } },
	});
	let serverVersion = 0;
	const firstFetch = deferred();
	const firstStarted = deferred();
	let fetches = 0;
	const observer = new QueryObserver(client, {
		queryKey: [...queryKeys.skills.updateChecksAll(), "global"],
		queryFn: async () => {
			fetches += 1;
			const seen = serverVersion;
			if (fetches === 1) {
				firstStarted.resolve();
				await firstFetch.promise;
			}
			return seen;
		},
		staleTime: Number.POSITIVE_INFINITY,
	});
	const unsubscribe = observer.subscribe(() => {});

	// A check is in flight and has already read the pre-write state…
	await firstStarted.promise;
	serverVersion = 1; // …then the last batch writes.
	const done = refetchUpdateChecksAfterWrites(client);
	firstFetch.resolve();
	await done;

	assert.equal(
		observer.getCurrentResult().data,
		1,
		"the cached answer must come from a check that began after the write",
	);
	assert.equal(fetches, 2, "join the in-flight check, then exactly one more");
	unsubscribe();
});

test("with nothing in flight it runs exactly one check", async () => {
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false } },
	});
	let fetches = 0;
	const observer = new QueryObserver(client, {
		queryKey: [...queryKeys.skills.updateChecksAll(), "global"],
		queryFn: async () => {
			fetches += 1;
			return fetches;
		},
		staleTime: Number.POSITIVE_INFINITY,
	});
	const unsubscribe = observer.subscribe(() => {});
	await observer.refetch();
	fetches = 0;

	await refetchUpdateChecksAfterWrites(client);

	assert.equal(
		fetches,
		1,
		"an every-source check is too expensive to double",
	);
	unsubscribe();
});

// The real call path of a single-source "update all": the batch mutation's
// onSuccess ran, THEN applyAll's final refetch. If the batch still started its
// own check, the final helper saw it in flight and paid for a second full
// every-source check — each one spending anonymous REST budget, the very thing
// this incident ran out of.
test("one batch plus the final refetch runs exactly one check", async () => {
	const client = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});
	let fetches = 0;
	const observer = new QueryObserver(client, {
		queryKey: [...queryKeys.skills.updateChecksAll(), "global"],
		queryFn: async () => {
			fetches += 1;
			return fetches;
		},
		staleTime: Number.POSITIVE_INFINITY,
	});
	const unsubscribe = observer.subscribe(() => {});
	await observer.refetch();
	fetches = 0;

	const api = {
		skills: { applyUpdates: async () => ({ results: [] }) },
	} as unknown as ApiClient;
	const batch = new MutationObserver(
		client,
		applySkillUpdatesMutationOptions({ api, queryClient: client }),
	);
	await batch.mutate({
		body: {
			source: "https://github.com/o/r",
			names: ["a"],
			scope: "global",
			projectRoot: null,
			confirm: true,
		},
	});
	await refetchUpdateChecksAfterWrites(client);

	assert.equal(fetches, 1);
	unsubscribe();
});
