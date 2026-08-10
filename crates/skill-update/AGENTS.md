# SKILL-UPDATE CRATE KNOWLEDGE BASE

**Crate**: `skill-update` — Skill update-check orchestrator + Sources domain +
source-mutation flows.

**Role**: Single shared implementation of "are installed skills out of date?"
used by **both** `GET /skills/check-updates` (API) and `aghub-cli check --online`.
Also hosts Sources domain (`sources` mod: list/diff/classify + token-first fetch)
for API sources routes and CLI `source`, and the shared **source-mutation seam**
(`mutation.rs`: fetch → install / resync / accept-rename) used by the API skills
routes and CLI `apply-update` / `source`. Network/credentials live here;
`aghub-core` stays pure (hash/compare only).

## WHERE TO LOOK

| Task                      | Location                     | Notes                                                                                               |
| ------------------------- | ---------------------------- | --------------------------------------------------------------------------------------------------- |
| Entry point               | `lib.rs` `check_updates()`   | Async; `CheckDeps` + entries; grouping is inline — invariant: each `SourceRef` fetched at most once |
| TTL result cache          | `lib.rs` `ResultCache`       | Keyed by `SourceRef`                                                                                |
| Inject auth               | `lib.rs` `TokenResolver`     | CLI env / API own resolver                                                                          |
| Source list/diff/classify | `sources.rs`                 | API + CLI `source`                                                                                  |
| Source mutation flows     | `mutation.rs`                | install / resync / accept-rename — API + CLI                                                        |
| Repo fetch + catalog      | `repository.rs`              | `SkillRepository` + `FetchSelection` (consumes `aghub-git`'s `RepoSnapshot`)                        |
| Network adapters          | `git.rs`                     | `GitFetcher` / `GitRefResolver`                                                                     |
| Hash compare / rename     | `aghub_core::skills::update` | Pure — never move here                                                                              |

## KEY ABSTRACTIONS

- **`SourceRef { source, ref_ }`**: unique upstream coordinate; shared entries fetch once
- **`EntryInput`**: lock entry projection (`local` → `Uncheckable{Local}`; no path → `NoPath`)
- **`Fetcher` / `RefResolver` / `TokenResolver`**: injected seams for testability
- **`CheckDeps`**: dependency bundle into `check_updates()`

## PREFLIGHT (must stay correct)

The tip preflight skips the treeless fetch **only when** upstream tip
(`ref_commit`) is unchanged **AND** the installed copy is unmodified.
`ref_commit == None` (project lock / npx / legacy) → **never** preflight-skip.
Wrong skip = missed updates; wrong fetch = wasted network.

**It must never download objects** (`SkillRepository::resolve_tip`): REST
`/commits/<ref>` on github hosts — ONE request on the pooled client — else a git
ref advertisement. NOT `RepoFetchBackend::resolve`, which the gix backend answers
by performing the depth-1 fetch, i.e. exactly the cost the preflight exists to
avoid. It runs for EVERY source group including the all-clear case, so its cost
is the floor of a check: a `git ls-refs` handshake is cheap in bytes and ~0.6s in
time against github.com, and that was most of a check's wall clock before REST
took over the github path.

## GIT ADAPTER GOTCHAS

- **`https_only_token`**: never attach tokens to non-https URLs (ssh auth stands;
  `aghub-git`'s `inject_credentials` hard-fails otherwise)
- **`SkillRepository` (`repository.rs`) is the single fallback owner**: REST →
  gix → system `git` + OS helper for HTTPS non-GitHub hosts (TFS/Azure).
  Surfaces reach it via `GitFetcher` or the API's repository factory / pinned
  source sessions — never build a second fallback chain.
- **`fetch_source_with_resolver` is token-first**: resolve once, then fetch once;
  anonymous is used only when no token exists.

## TESTING

```bash
cargo test -p skill-update              # no external network; Unix needs git + loopback
```

The regular suite needs no external network (fake Fetcher / fake transports;
on Unix, some `skill_repository` tests additionally spawn a loopback `git
daemon` — they need a `git` binary and a bindable 127.0.0.1 — to exercise the
real GixShallow TCP path). The ignored `skill_repository` E2E pins a
stable GitHub commit and proves REST catalog + selected install content/hash;
run it with `cargo test -p skill-update --test skill_repository -- --ignored` —
it needs `GITHUB_TOKEN`/`GH_TOKEN`, and **silently skips without one** (green
output proves nothing).
The older `GitFetcher` network E2E also lives in `aghub-api`
(`routes/skills_update.rs`, `#[ignore = "network"]`).

## ANTI-PATTERNS

- **NEVER** move hash/compare here — stays pure in `aghub_core::skills::update`
- **NEVER** hardcode credentials — inject `TokenResolver`
- **NEVER** fetch per-entry — group by `SourceRef`
- **NEVER** skip on `ref_commit == None`
- **NEVER** let CLI and API fork — both call `check_updates()`
