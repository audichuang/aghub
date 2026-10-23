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
import { bindSourceCredentialMutationOptions } from "./credentials.ts";
import { queryKeys } from "./keys.ts";
import {
	applySkillUpdateMutationOptions,
	applySkillUpdatesMutationOptions,
	checkSkillUpdatesMutationOptions,
	checkSkillUpdatesQueryOptions,
	gitSyncSkillMutationOptions,
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

// The skill detail panel reads the check through a lazy `enabled: false`
// observer. A single apply-update must refresh it with exactly ONE check from
// the shared options — the panel no longer starts a second one — whether or
// not an auto-check observer on the same key is mounted (settings page) or not
// (project page).
for (const withActiveObserver of [false, true]) {
	test(`single update runs exactly one check (auto-check observer: ${withActiveObserver})`, async () => {
		const client = new QueryClient({
			defaultOptions: { queries: { retry: false } },
		});
		const queryKey = queryKeys.skills.updateChecks("global");
		let fetches = 0;
		const queryFn = async () => {
			fetches += 1;
			return fetches;
		};
		const lazy = new QueryObserver(client, {
			queryKey,
			queryFn,
			enabled: false,
		});
		const unsubLazy = lazy.subscribe(() => {});
		const active = withActiveObserver
			? new QueryObserver(client, {
					queryKey,
					queryFn,
					staleTime: Number.POSITIVE_INFINITY,
				})
			: null;
		const unsubActive = active?.subscribe(() => {});
		await active?.refetch();
		// Lazy-only case: an earlier check left an answer in the cache.
		if (!active) client.setQueryData(queryKey, 0);
		fetches = 0;

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
		await new Promise((resolve) => setTimeout(resolve, 50));

		assert.equal(fetches, 1, "one update must cost one every-source check");
		assert.equal(lazy.getCurrentResult().data, 1);
		unsubLazy();
		unsubActive?.();
	});
}

/**
 * A displayed check that is already in flight and has read the PRE-write
 * server state ("outdated"), plus a way to run one write against it.
 */
async function preWriteCheckInFlight(enabled: boolean) {
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false } },
	});
	let server = "outdated";
	const firstFetch = deferred();
	const firstStarted = deferred();
	let fetches = 0;
	const observer = new QueryObserver(client, {
		queryKey: queryKeys.skills.updateChecks("global"),
		queryFn: async () => {
			fetches += 1;
			const seen = server;
			if (fetches === 1) {
				firstStarted.resolve();
				await firstFetch.promise;
			}
			return seen;
		},
		enabled,
		staleTime: Number.POSITIVE_INFINITY,
	});
	const unsubscribe = observer.subscribe(() => {});
	if (!enabled) {
		client.setQueryData(queryKeys.skills.updateChecks("global"), "old");
		void observer.refetch();
	}
	await firstStarted.promise;
	const api = {
		skills: {
			applyUpdate: async () => ({ success: true }),
			gitSync: async () => ({}),
		},
	} as unknown as ApiClient;
	const write = async (kind: "update" | "sync" = "update") => {
		server = "current";
		const options =
			kind === "update"
				? applySkillUpdateMutationOptions({ api, queryClient: client })
				: gitSyncSkillMutationOptions({ api, queryClient: client });
		await new MutationObserver(client, options as any).mutate({
			body: {},
		} as any);
	};
	const settle = async () => {
		firstFetch.resolve();
		await new Promise((resolve) => setTimeout(resolve, 50));
	};
	return {
		write,
		settle,
		answer: () => observer.getCurrentResult().data,
		fetches: () => fetches,
		unsubscribe,
	};
}

// A single update joined the check that started BEFORE its write and ran
// nothing after it: the badge kept "outdated" for the whole staleTime.
for (const [label, enabled, kind] of [
	["single update, auto-check observer", true, "update"],
	["single update, lazy observer", false, "update"],
	["git sync, auto-check observer", true, "sync"],
] as const) {
	test(`${label}: the final answer comes from a check started after the write`, async () => {
		const race = await preWriteCheckInFlight(enabled);
		await race.write(kind);
		await race.settle();

		assert.equal(race.answer(), "current");
		assert.equal(race.fetches(), 2, "join the stale one, then one more");
		race.unsubscribe();
	});
}

// Several quick updates while that stale check is in flight share ONE
// trailing check: each one re-fetches every source over the network.
test("rapid updates during an in-flight check add one trailing check", async () => {
	const race = await preWriteCheckInFlight(true);
	await race.write();
	await race.write();
	await race.write();
	await race.settle();

	assert.equal(race.answer(), "current");
	assert.equal(race.fetches(), 2, "the in-flight check + one trailing");
	race.unsubscribe();
});

// Disabled-only observers (settings page offline / no project selected) must
// not be forced into a check unless they already hold an answer and we are online.
function setOnline(value: boolean | undefined) {
	Object.defineProperty(globalThis, "navigator", {
		value: { onLine: value },
		configurable: true,
	});
}

async function disabledOnlyFetches(seed: boolean, online: boolean) {
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false } },
	});
	const queryKey = queryKeys.skills.updateChecks("global");
	let fetches = 0;
	const observer = new QueryObserver(client, {
		queryKey,
		queryFn: async () => {
			fetches += 1;
			return fetches;
		},
		enabled: false,
	});
	const unsubscribe = observer.subscribe(() => {});
	if (seed) client.setQueryData(queryKey, 0);
	setOnline(online);
	try {
		const api = {
			skills: { applyUpdate: async () => ({ success: true }) },
		} as unknown as ApiClient;
		await applySkillUpdateMutationOptions({
			api,
			queryClient: client,
		}).onSuccess?.(
			{ success: true } as any,
			{ body: {} as any },
			undefined,
			{} as any,
		);
		await new Promise((resolve) => setTimeout(resolve, 50));
	} finally {
		setOnline(undefined);
		unsubscribe();
	}
	return fetches;
}

test("all-disabled update-check query with no data is not fetched", async () => {
	assert.equal(await disabledOnlyFetches(false, true), 0);
});

test("all-disabled update-check query with data is fetched once", async () => {
	assert.equal(await disabledOnlyFetches(true, true), 1);
});

test("all-disabled update-check query with data is not fetched offline", async () => {
	assert.equal(await disabledOnlyFetches(true, false), 0);
});

// The manual "check for updates" button used to run its own request and
// setQueryData its answer. Clicked just before an apply, that answer (read
// pre-write) landed AFTER the trailing post-write check and pinned "outdated"
// for the 10-min staleTime. A manual check must be one of the query's fetches.
test("a manual check started before a write cannot overwrite the post-write answer", async () => {
	const client = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});
	let server = "outdated";
	const manualFetch = deferred();
	const manualStarted = deferred();
	let calls = 0;
	const api = {
		skills: {
			checkUpdates: async () => {
				calls += 1;
				const seen = server;
				if (calls === 1) {
					manualStarted.resolve();
					await manualFetch.promise;
				}
				return seen;
			},
			applyUpdate: async () => ({ success: true }),
		},
	} as unknown as ApiClient;
	const params = { scope: "global" } as const;
	const observer = new QueryObserver(
		client,
		checkSkillUpdatesQueryOptions({
			api,
			params,
			enabled: false,
			staleTime: Number.POSITIVE_INFINITY,
		}),
	);
	const unsubscribe = observer.subscribe(() => {});
	client.setQueryData(observer.options.queryKey, "outdated" as any);

	const manual = new MutationObserver(
		client,
		checkSkillUpdatesMutationOptions({ api, queryClient: client }),
	).mutate(params);
	await manualStarted.promise;
	server = "current";
	await new MutationObserver(
		client,
		applySkillUpdateMutationOptions({ api, queryClient: client }),
	).mutate({ body: {} as any });
	await new Promise((resolve) => setTimeout(resolve, 20));
	manualFetch.resolve();
	await manual;
	await new Promise((resolve) => setTimeout(resolve, 50));

	assert.equal(observer.getCurrentResult().data, "current");
	assert.equal(calls, 2, "the manual check + one trailing post-write check");
	unsubscribe();
});

/**
 * An auto-check observer whose first check is in flight and has already read
 * `before`; the server then answers `after` from the next call on.
 */
async function autoCheckInFlight(before: string, after: string) {
	const client = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});
	let server = before;
	const firstFetch = deferred();
	const firstStarted = deferred();
	let calls = 0;
	const api = {
		skills: {
			checkUpdates: async () => {
				calls += 1;
				const seen = server;
				if (calls === 1) {
					firstStarted.resolve();
					await firstFetch.promise;
				}
				return seen;
			},
		},
		credentials: { bindSource: async () => ({}) },
	} as unknown as ApiClient;
	const params = { scope: "global" } as const;
	const observer = new QueryObserver(
		client,
		checkSkillUpdatesQueryOptions({ api, params }),
	);
	const unsubscribe = observer.subscribe(() => {});
	await firstStarted.promise;
	server = after;
	return {
		client,
		api,
		params,
		release: () => firstFetch.resolve(),
		answer: () => observer.getCurrentResult().data,
		calls: () => calls,
		unsubscribe,
	};
}

// Codex round 3: binding a valid credential while a check is in flight joined
// the pre-binding request, which cached its auth failure and left the source
// uncheckable for the whole 10-min staleTime.
test("a credential binding during an in-flight check ends on a post-binding answer", async () => {
	const race = await autoCheckInFlight("auth-error", "ok");
	await new MutationObserver(
		race.client,
		bindSourceCredentialMutationOptions({
			api: race.api,
			queryClient: race.client,
		}),
	).mutate({ source: "o/r", credentialId: "c1" });
	race.release();
	await new Promise((resolve) => setTimeout(resolve, 50));

	assert.equal(race.answer(), "ok");
	assert.equal(race.calls(), 2, "the in-flight check + one post-binding");
	race.unsubscribe();
});

// A manual click used `fetchQuery`, which JOINS an in-flight check: the click
// was answered by a check that read the server before the click.
test("a manual check during an in-flight check is answered by a post-click check", async () => {
	const race = await autoCheckInFlight("outdated", "current");
	let delivered: unknown;
	const manual = new MutationObserver(
		race.client,
		checkSkillUpdatesMutationOptions({
			api: race.api,
			queryClient: race.client,
			onSuccess: (data) => {
				delivered = data;
			},
		}),
	).mutate(race.params);
	race.release();
	const returned = await manual;

	assert.equal(race.answer(), "current");
	assert.equal(returned, "current");
	assert.equal(delivered, "current");
	assert.equal(race.calls(), 2, "the in-flight check + one post-click");
	race.unsubscribe();
});

// Rapid clicks during one in-flight check share ONE post-click check.
test("rapid manual clicks during an in-flight check add one trailing check", async () => {
	const race = await autoCheckInFlight("outdated", "current");
	const options = checkSkillUpdatesMutationOptions({
		api: race.api,
		queryClient: race.client,
	});
	const clicks = [1, 2, 3].map(() =>
		new MutationObserver(race.client, options).mutate(race.params),
	);
	race.release();
	assert.deepEqual(await Promise.all(clicks), [
		"current",
		"current",
		"current",
	]);
	assert.equal(race.calls(), 2);
	race.unsubscribe();
});

// A failed check must still reach the mutation's onError.
test("a manual check that fails rejects the mutation", async () => {
	const client = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});
	const api = {
		skills: {
			checkUpdates: async () => {
				throw new Error("offline");
			},
		},
	} as unknown as ApiClient;
	let failed: Error | undefined;
	await new MutationObserver(
		client,
		checkSkillUpdatesMutationOptions({
			api,
			queryClient: client,
			onError: (error) => {
				failed = error;
			},
		}),
	)
		.mutate({ scope: "global" })
		.catch(() => {});
	assert.equal(failed?.message, "offline");
});

// Codex: a connection switch (`ConnectionProvider.setActive` removes every
// skills query) cancels the click's check mid-flight. The click was answered
// from `query.state`, which a removed query leaves at its PRE-click "success",
// so the settings page toasted "check complete" over the stale answer.
test("a manual check cancelled mid-flight rejects instead of returning cached data", async () => {
	const client = new QueryClient({
		defaultOptions: {
			queries: { retry: false },
			mutations: { retry: false },
		},
	});
	const started = deferred();
	const api = {
		skills: {
			checkUpdates: async () => {
				started.resolve();
				await new Promise(() => {}); // never answers; only cancelled
			},
		},
	} as unknown as ApiClient;
	const params = { scope: "global" } as const;
	// The settings page's auto-check observer, holding an earlier answer.
	const observer = new QueryObserver(
		client,
		checkSkillUpdatesQueryOptions({ api, params, enabled: false }),
	);
	const unsubscribe = observer.subscribe(() => {});
	client.setQueryData(observer.options.queryKey, "pre-click" as any);
	const delivered: unknown[] = [];
	const failed: string[] = [];
	const options = checkSkillUpdatesMutationOptions({
		api,
		queryClient: client,
		onSuccess: (data) => {
			delivered.push(data);
		},
		onError: (error) => {
			failed.push(error.message);
		},
	});
	const click = () =>
		new MutationObserver(client, options).mutate(params).catch(() => {});
	// The second click coalesces onto the first one's scheduled check.
	const manual = Promise.all([click(), click()]);
	await started.promise;
	// Mirror setActive: drop the skills namespace.
	client.removeQueries({ queryKey: ["skills"] });
	await manual;

	assert.deepEqual(
		delivered,
		[],
		"the stale pre-click answer must not succeed",
	);
	assert.deepEqual(failed, ["CancelledError", "CancelledError"]);
	unsubscribe();
});
