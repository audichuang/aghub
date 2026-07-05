# SKILL-UPDATE CRATE KNOWLEDGE BASE

**Crate**: `skill-update` — Skill update-check orchestrator\
**Role in monorepo**: The single shared implementation of "are my installed
skills out of date?". Extracted from `crates/api` so **both** the desktop API
(`GET /skills/check-updates`) and the CLI (`aghub-cli check --online`) run the
exact same logic. `crates/core` stays pure (hash/compare only); the network
fetch and credential resolution live here.

## OVERVIEW

Given the lock entries, it groups them by upstream coordinate, does a cheap
ls-refs preflight, fetches changed repos treeless, and compares folder hashes —
with a TTL result cache, bounded concurrency, a per-fetch timeout, and offline
skip. Network access is injected via the `Fetcher`/`RefResolver` traits so the
grouping/cache/timeout/concurrency logic is unit-testable without a network.

## STRUCTURE

```
crates/skill-update/src/
├── lib.rs   # Orchestrator: SourceRef, EntryInput, group_by_source_ref,
│            #   ResultCache, Fetcher/TokenResolver/RefResolver traits,
│            #   CheckDeps, check_updates() (the entry point)
└── git.rs   # GitFetcher / GitRefResolver — the real gix-backed adapters
```

## WHERE TO LOOK

| Task                         | Location                           | Notes                                              |
| ---------------------------- | ---------------------------------- | -------------------------------------------------- |
| Entry point                  | `src/lib.rs` `check_updates()`     | Async; takes `CheckDeps` + entries                 |
| Group entries by repo/ref    | `src/lib.rs` `group_by_source_ref` | Fetch each `SourceRef` at most once                |
| Result caching               | `src/lib.rs` `ResultCache`         | TTL cache keyed by `SourceRef`                     |
| Inject auth                  | `src/lib.rs` `TokenResolver`       | CLI = env `GIT_PASSWORD`/`GITHUB_TOKEN`; API = own |
| Real network fetch / ls-refs | `src/git.rs`                       | `GitFetcher` (treeless) / `GitRefResolver`         |
| Hash compare / rename detect | `aghub_core::skills::update`       | `compare_known_hashes`, `detect_rename` (pure)     |

## KEY ABSTRACTIONS

- **`SourceRef { source, ref_ }`**: a unique upstream coordinate (repo + optional
  branch/tag/SHA). Entries sharing a `SourceRef` are fetched **once**.
- **`EntryInput`**: one lock entry projected to the inputs the orchestrator needs
  (`name`, `scope`, `source_ref`, `source_type`, `skill_path`, `stored_hash`,
  `local_hash`, `ref_commit`). `source_type == "local"` → `Uncheckable{Local}`
  with no fetch; `skill_path == None` → `Uncheckable{NoPath}`.
- **`Fetcher` / `RefResolver` / `TokenResolver`** (traits): the injected seams.
  Each surface supplies its own `TokenResolver`; default git adapters are
  `GitFetcher`/`GitRefResolver` in `git.rs`.
- **`CheckDeps<'a>`**: the dependency bundle passed into `check_updates()`.

## PREFLIGHT (the optimization that must stay correct)

An ls-refs preflight skips the treeless fetch for a group **only when** the
upstream tip (`ref_commit`) is unchanged **AND** the installed copy is
unmodified. `ref_commit == None` (project lock / npx / legacy) → never a
preflight skip. Getting this wrong either misses real updates (false skip) or
fetches needlessly (lost optimization).

## TESTING

```bash
cargo test -p skill-update           # unit tests (grouping/cache/timeout — network-free)
cargo test -p skill-update -- --ignored   # the #[ignore = "network"] E2E paths
```

The grouping/cache/concurrency/timeout logic is covered without a network by
injecting fake `Fetcher`s. Real network paths are behind `#[ignore]` E2E tests.

## ANTI-PATTERNS

- **NEVER** move hash/compare logic here — that stays pure in `aghub_core::skills::update`.
- **NEVER** hardcode credentials — auth comes through the injected `TokenResolver`.
- **NEVER** fetch per-entry — group by `SourceRef` and fetch each repo at most once.
- **NEVER** skip a group on `ref_commit == None` — only skip when tip unchanged AND copy unmodified.
- **NEVER** let one surface (CLI/API) fork the logic — both must call `check_updates()`.
