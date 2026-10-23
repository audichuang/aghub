# SKILL-UPDATE CRATE KNOWLEDGE BASE

**Crate**: `skill-update` — Skill update-check orchestrator + Sources domain +
source-mutation flows.

**Role**: Single shared implementation of "are installed skills out of date?"
used by **both** `GET /skills/check-updates` (API) and `aghub-cli check --online`.
Also hosts Sources domain (`sources` mod: list/diff/classify + token-first fetch)
for API sources routes and CLI `source`, and the shared **source-mutation seam**
(`mutation.rs`: fetch → install / resync / accept-rename) used by the API skills
routes and CLI `apply-update` / `source`. Network/credentials live here;
`aghub-core` never fetches (it uses `aghub-git` only for pure source
normalization and redaction). Clone coordinates, host provability, Source
membership and ref cohorts are core `skills::source_coordinate` — call it,
never re-derive them here.

## WHERE TO LOOK

| Task                      | Location                      | Notes                                                                                                                                                                                                                                                                                          |
| ------------------------- | ----------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Entry point               | `lib.rs` `run_update_check()` | The only public check entry; owns timeout/concurrency/cache-TTL constants. Surfaces pass fetcher + ref resolver + token resolver + offline. `check_updates()`/`CheckDeps` are crate-private (tests inject knobs); grouping is inline — invariant: each `SourceRef` fetched at most once        |
| TTL result cache          | `lib.rs` `ResultCache`        | Keyed by `SourceRef`; crate-private, built per call inside `run_update_check()`                                                                                                                                                                                                                |
| Inject auth               | `lib.rs` `TokenResolver`      | CLI env / API own resolver                                                                                                                                                                                                                                                                     |
| Source list/diff/classify | `sources.rs`                  | API + CLI `source`. Three entry points for surfaces — `list_sources`, `diff_source`, `plan_source_sync`; `import_ref` / `recorded_refs` are `pub` only for `fetched_install.rs`                                                                                                                |
| Lock → check inputs       | `projection.rs`               | API route + CLI `check` share it: the `wanted` hash filter, the per-root hash memo, the offline skip, and the lock-before-disk read order. The API's post-check heal writer (`write_auto_healed_hashes`) lives here too; `check` never calls it                                                |
| Source mutation flows     | `mutation.rs`                 | install / resync / accept-rename — API + CLI                                                                                                                                                                                                                                                   |
| Fetched Source install    | `fetched_install.rs`          | /skills/install 與 /skills/git/install 的 ref 決策（先命名 ref 再 resolve）、agent 預檢與逐列歸屬、fetch、每個 skill 各自取鎖安裝、攤平成 rows；expected_ref 是建構子的政策（catalog 一律 Some，session 只在未指定 branch 時 Some，CLI source sync 為 None）；CLI 只共用 `install_lock_source` |
| Repo fetch + catalog      | `repository.rs`               | `SkillRepository` + `FetchSelection` (consumes `aghub-git`'s `RepoSnapshot`)                                                                                                                                                                                                                   |
| Network adapters          | `git.rs`                      | `GitFetcher` / `GitRefResolver`                                                                                                                                                                                                                                                                |
| Hash compare / rename     | `aghub_core::skills::update`  | Pure — never move here                                                                                                                                                                                                                                                                         |

## KEY ABSTRACTIONS

- **`SourceRef { source, ref_ }`**: unique upstream coordinate; shared entries fetch once
- **`EntryInput`**: lock entry projection (`local` → `Uncheckable{Local}`; no path → `NoPath`)
- **`Fetcher` / `RefResolver` / `TokenResolver`**: injected seams for testability
- **`CheckDeps`** (crate-private): dependency bundle into `check_updates()`

## 更新狀態的語意

`check` 與 `source diff` 對可讀的本機副本採同一個內容比較基準。
「可更新」也包含本機修改而上游未變，並不只表示上游有新版本；
`apply-update --yes` 會用來源內容覆蓋本機修改。不要改回只比較 lock。
`UpdateAvailable.current` / `available` 是 comparison hash，只算 Master
copier 會帶進 Master 的檔案（`skill::carried_into_master`：排除 `metadata.json`
與 `.git`/`__pycache__`/`__pypackages__`）；它們不必等於 lock 的 raw hash。
所以上游只改這些被排除的檔案不算更新——安裝也不會帶過去。所有 lock 寫入、
healing 與安全稽核仍使用 raw 內容。只有被排除檔案不同可判最新，但 preflight
仍需 raw 完整性證明；不能拿 comparison hash 與 raw lock 相比來省略 fetch。

## PREFLIGHT (must stay correct)

The tip preflight skips the treeless fetch **only when** upstream tip
(`ref_commit`) is unchanged **AND** the installed copy is unmodified.
`ref_commit == None` (project lock / npx / legacy) → **never** preflight-skip.
Wrong skip = missed updates; wrong fetch = wasted network.

**The preflight hands the fetch a CLAIM, not a cache entry.** `resolve_tip`
returns a `PinnedSnapshot` (REST path only) and `check_updates` carries it into
`Fetcher::fetch_pinned`. A `(url, ref, token) -> snapshot` cache was
last-writer-wins, and since several source spellings normalize to one coordinate
(`acme/skills`, `github:acme/skills`, `https://github.com/acme/skills.git`) while
groups are keyed by the RAW string, a slow observation of an older tip could
overwrite a newer one — the fetch then materialized a tip nobody judged and the
row read `UpToDate` for a source that had moved. A claim also carries its backend,
there is no commit-oid-keyed memo to consult, so list/fetch need no prior `resolve` on the same instance.

**Anything that answers without looking upstream must still verify the local
copy** (`locally_intact`): the preflight skip, the pinned-SHA shortcut, and the
`UpToDate` verdict itself. "Upstream has not moved" is not "your copy is fine" —
skipping the check reports `UpToDate` for a skill whose folder is gone.

**It must never download objects** (`SkillRepository::resolve_tip`): REST
`/commits/<ref>` on github hosts — ONE request on the pooled client — else a git
ref advertisement. NOT `RepoFetchBackend::resolve`, which the gix backend answers
by performing the depth-1 fetch, i.e. exactly the cost the preflight exists to
avoid. It runs only for groups that COULD still skip (`preflight_can_skip`): a
group with no `ref_commit`, with members recording different commits, or whose
local copy has drifted can never skip whatever the remote answers, so it must not
spend a request to be told so. That gate is a QUOTA decision as much as a speed
one — the REST path spends anonymous GitHub's 60/hr. A `git ls-refs` handshake is
cheap in bytes and ~0.6s against github.com, and that was most of a check's wall
clock before REST took over the github path.

## TREE-FIRST CONTENT PROOF (#77, #13)

refCommit is per-entry provenance, not a group cache key: one single-skill
update, an upstream-deleted member, or a lock without refCommit (npx / project)
makes `preflight_can_skip` false for the whole group forever. So when the
preflight cannot skip, check and `source diff` fetch with
`FetchSelection::Verify`: read the tree (REST: one request), compare each
installed folder's sorted `(rel, blob oid)` list with the local copy's
(`proof.rs` `LocalManifest`, built in the SAME walk and read as the hashes),
and materialize only folders that are not proven; `source diff` also fetches
just each catalog skill's `SKILL.md` (+ root `CHANGELOG.md`, `.ignore`).

- Equal lists ⇒ identical content; anything uncertain ⇒ materialize. The
  fallback list (root skill, symlink entry, `.git`/`node_modules`, invalid or
  non-ASCII path, case-fold collision, native path ≠ normalized rel, any entry
  the Master copier drops) lives in `proof.rs` — extend it there, never relax it.
- **Every consumer hashes a fetched folder through `FetchedRepo::folder_hashes`.**
  A proven folder is not on disk; hashing `repo.root` directly reads it as
  missing and reports every unchanged skill as outdated.
- `source sync`, install and rename stay on `CatalogSnapshot` / `Skills`: they
  install from the fetched tree ("Fetch ONCE").
- gix and system-git download every blob in their depth-1 fetch anyway (gix has
  no partial clone yet), so the saving is REST-only; the verdict is identical.
- Count REST requests (FakeTransport), never time it: REST and gix each cache.

**Apply installs only the commit the check judged.** `CheckOutput.judged_commit`
(API `judgedCommit`) is carried into `resync_judged_skill(s)`; a fetch that lands
on another commit refuses the row with `SKILL_SOURCE_CHANGED_DURING_FETCH` and
writes nothing. CLI `apply-update --outdated` and the API apply routes share it.

## GIT ADAPTER GOTCHAS

- **`https_only_token`**: never attach tokens to non-https URLs (ssh auth stands;
  `aghub-git`'s `inject_credentials` hard-fails otherwise)
- **`SkillRepository` (`repository.rs`) is the single fallback owner**: REST →
  gix → system `git` + OS helper for HTTPS non-GitHub hosts (TFS/Azure).
  Surfaces reach it via `GitFetcher` or the API's repository factory / pinned
  source sessions — never build a second fallback chain.
- **`fetch_source_with_resolver` is token-first**: resolve once, then fetch once;
  anonymous is used only when no token exists.

## Deliberate surface differences

CLI and API call the same entries; these are the divergences this crate owns (the API's fail-open lock read and `source diff ?scope=all` are in `crates/api/AGENTS.md`).

- **check auto-heal**: `GET /skills/check-updates` calls
  `projection::write_auto_healed_hashes` after the check; `aghub-cli check` never
  does — check does not write the lock. A busy mutation lock is reported in the
  response's `healError` (retryable `SKILL_MUTATION_LOCK_BUSY`), never a 500, and
  the results are still returned.

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
- **NEVER** let CLI and API fork — both call `run_update_check()`, and both project
  the lock through `projection.rs`
- **NEVER** reorder `check_updates`'s group gates. Precheck runs FIRST so a
  permanently unfetchable source keeps its own reason offline; `offline` runs
  before the pinned-SHA shortcut so an offline check never reads `local_hash`
  (i.e. never hashes disk)
- **NEVER** re-assemble a deep entry point's preamble at a call site. Resolving
  the coordinate, refusing an ambiguous source and prechecking it is ONE private
  `prepare()` behind `diff_source` / `plan_source_sync`
