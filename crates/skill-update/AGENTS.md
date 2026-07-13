# SKILL-UPDATE CRATE KNOWLEDGE BASE

**Crate**: `skill-update` — Skill update-check orchestrator + Sources domain.

**Role**: Single shared implementation of "are installed skills out of date?"
used by **both** `GET /skills/check-updates` (API) and `aghub-cli check --online`.
Also hosts Sources domain (`sources` mod: list/diff/classify + lazy-auth fetch)
for API sources routes and CLI `source`. Network/credentials live here;
`aghub-core` stays pure (hash/compare only).

## WHERE TO LOOK

| Task                      | Location                       | Notes                                      |
| ------------------------- | ------------------------------ | ------------------------------------------ |
| Entry point               | `lib.rs` `check_updates()`     | Async; `CheckDeps` + entries               |
| Group by repo/ref         | `lib.rs` `group_by_source_ref` | Fetch each `SourceRef` at most once        |
| TTL result cache          | `lib.rs` `ResultCache`         | Keyed by `SourceRef`                       |
| Inject auth               | `lib.rs` `TokenResolver`       | CLI env / API own resolver                 |
| Source list/diff/classify | `sources.rs`                   | API + CLI `source`                         |
| Network adapters          | `git.rs`                       | `GitFetcher` / `GitRefResolver` + fallback |
| Hash compare / rename     | `aghub_core::skills::update`   | Pure — never move here                     |

## KEY ABSTRACTIONS

- **`SourceRef { source, ref_ }`**: unique upstream coordinate; shared entries fetch once
- **`EntryInput`**: lock entry projection (`local` → `Uncheckable{Local}`; no path → `NoPath`)
- **`Fetcher` / `RefResolver` / `TokenResolver`**: injected seams for testability
- **`CheckDeps`**: dependency bundle into `check_updates()`

## PREFLIGHT (must stay correct)

ls-refs skips the treeless fetch **only when** upstream tip (`ref_commit`) is
unchanged **AND** the installed copy is unmodified. `ref_commit == None`
(project lock / npx / legacy) → **never** preflight-skip. Wrong skip = missed
updates; wrong fetch = wasted network.

## GIT ADAPTER GOTCHAS (`git.rs`)

- **`https_only_token`**: never attach tokens to non-https URLs (ssh auth stands;
  `inject_credentials` hard-fails otherwise)
- **`GitFetcherWithFallback`**: gix first, then system `git` + OS helper for
  https **non-github** hosts (TFS/Azure). Kind-2 (final-token) callers only —
  try `GIT_PASSWORD`/token **before** system-git
- **Do NOT wrap** `fetch_source_with_resolver` with `GitFetcherWithFallback` —
  that helper sequences unauth → one token retry; wrapping would fire system-git
  before the token

## TESTING

```bash
cargo test -p skill-update              # network-free (fake Fetcher)
```

This crate has no `#[ignore]` network tests; its suite uses a fake `Fetcher` to
exercise orchestration only (no real network). The real `GitFetcher` network
E2E lives in `aghub-api` (`routes/skills_update.rs`, `#[ignore = "network"]`) —
run with `cargo test -p aghub-api -- --ignored`.

## ANTI-PATTERNS

- **NEVER** move hash/compare here — stays pure in `aghub_core::skills::update`
- **NEVER** hardcode credentials — inject `TokenResolver`
- **NEVER** fetch per-entry — group by `SourceRef`
- **NEVER** skip on `ref_commit == None`
- **NEVER** let CLI and API fork — both call `check_updates()`
