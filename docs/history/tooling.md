# tooling history

Incidents in build tooling and the local environment: the `justfile`, cargo and
disk.

## Target dir filled the disk

`target/` reached 383G once (2026-08) and filled the disk again on 2026-09-28,
mid-session, truncating two source files an agent was writing to 0 bytes.

Measured in an isolated worktree (clippy `--all-targets` + `test --no-run`):
a cold build is 13G / ~7 min; an unchanged rerun adds nothing; editing a core
source file 3× only grew 13→14G because test binaries are overwritten in place.
What accumulates is a **dependency bump**: every bump leaves a full superseded
artifact set (8–10G) behind, because cargo never GCs old hashes. A day of a
dozen bumps fills the disk.

Rejected:

- `cargo-sweep --stamp` / `--maxsize`: it ranks artifacts by the oldest
  fingerprint atime, and a `relatime` mount refreshes atime at most daily, so a
  same-day sweep deleted live artifacts — the next clippy rebuilt 302 crates and
  the tests 644, i.e. a cold build anyway. A plain `cargo clean` costs the same
  and is predictable.
- `cargo clean gc` (cargo 1.98) only collects `~/.cargo`, not `target/`;
  `-Zgc` is nightly-only.
- Lower `debug`: already `line-tables-only`; lower loses panic line numbers.
- `CARGO_INCREMENTAL=0`: incremental is ~30% of target but its growth comes
  from the same superseded sets; turning it off slows every edit-rebuild.
- A git hook: a `du` over target on every commit, and `cargo clean` is the only
  same-day tool anyway.

Rule: `just target-cap` wipes `target/` once it exceeds `AGHUB_TARGET_CAP_GB`
(default 60), and `preflight` runs it last. It is bash, so it is never wired
into `test`, which CI runs on Windows under `cmd.exe`. Machine-level, the
user's weekly `cargo-sweep --time 30` timer cleaned 0 B on 2026-09-28 (nothing
is 30 days stale on an active repo) and was moved to daily `--time 3`.

Also observed: on this 31G machine a cold `aghub-api` lib-test build under
`-j8` or more is OOM-killed (signal 9); `CARGO_BUILD_JOBS=4..6` passes.

2026-10-22: the tracked `.cargo/config.toml` no longer names a
`rustc-wrapper` (it was `sccache`). The maintainer's machine builds through
mbx (mr-boxington), a shared cross-worktree cache with a disk budget, and mbx
silently defers to any configured wrapper and caches nothing. A wrapper is a
per-machine choice; set it in `~/.cargo/config.toml` or `RUSTC_WRAPPER`, never
in the tracked file. Under mbx `target` is a symlink into its managed root, so
`target-cap` measures with `du -L`. mbx needs `build_script_execution = false`
(mbx 1.26.0): its build-script cache drops `links` metadata whose key holds a
`:`, which is how tauri passes `DEP_TAURI_CORE:<plugin>__…` to the desktop
build script, so every `core:*` capability failed with `Permission … not found`.

## Preflight tested a stale node_modules

`justfile` `preflight` — the two `bun install --frozen-lockfile` lines and
`bun run test`, commit `b846a5b9`.

CI always installs frozen, but preflight used the developer's existing
`node_modules`, so the local gates could pass against a different dependency
set than the one that ships. That is how a HeroUI bump that killed every
Checkbox/Switch got through preflight, typecheck and lint untouched: the
lockfile said 3.2.5, `node_modules` was still 3.0.2. Separately, the frontend
unit tests (including the source-scan guards under `src/lib`) ran in CI but
were absent from the release gate, so the guard for that bug class would not
have been consulted before a tag.

Rule: preflight installs frozen (root and desktop) before any frontend gate,
and runs `bun run test`.

## cargo test serialized on the env mutex

`justfile` `test` / `preflight`, `ci.yml` test job (installs `cargo-nextest`).

`cargo test --workspace` took ~73s wall on a warm build (2026-10-11, 20 cores),
48s of it the `aghub-api` lib binary alone: its route tests all take the one
`test_env_lock`, and libtest runs a binary's tests as threads of ONE process,
so they ran one at a time. `cargo nextest run --workspace` runs each test in
its own process — every test owns its env — and finished the same 2873 tests
in ~9.6s, green 6 runs in a row. Compile was not the bottleneck: an edit to a
core source file re-links every test binary in ~11s (sccache + incremental +
the default `rust-lld`).

Rejected: merging the 44 integration-test files into one binary per crate. It
would trim link time, but it merges their env mutexes' scopes and breaks the
"one env mutex per binary" rule in `crates/core/AGENTS.md` Testing for little
gain over 11s.

Re-measured on CI 2026-10-23 (PR #86, closed): only the 25 files that write
no env moved into one `tests/it/` binary per crate (44 -> 23 integration
binaries). The Windows test compile step, the job that motivated it, stayed
put: 1m48s and 2m03s against 1m49s and 1m56s before; Ubuntu went 56s -> 42s
in one sample, off the critical path. The step is a serial chain, `aghub-core`
-> `skill-update` -> the `aghub-api` lib -> the `aghub-api` lib's unit-test
binary (the largest single unit, `cargo --timings`) -> the desktop crate; the
integration binaries are small and link in parallel beside it. Shortening
that chain, not the binary count, is what would move this job.

Rule: `just test` and `preflight` use nextest; doc tests stay on `cargo test
--doc`. Plain `cargo test` must keep passing, so the env-mutex rule still
applies.

## CI cache thrash and redundant steps

`.github/workflows/ci.yml`, measured on main run of 2026-10-11.

The repo's GHA cache sat at 11.46GB / 3937 entries, over GitHub's 10GB cap:
sccache-GHA writes one entry per compiled object, rust-cache one ~850MB entry
per OS × job key. LRU eviction left only 2 of the 9 rust-cache keys alive
(`Windows-test`, `macOS-test`), so Test (ubuntu) ran its tests step cold in
669s and Rust Lint (macOS) its clippy in 532s, against ~150s on warm legs.
Two layers caching the same dependency artifacts were evicting each other.

Changes:

- sccache dropped from ci.yml; rust-cache stays. ci.yml must also set
  `RUSTC_WRAPPER: ""`: the tracked `.cargo/config.toml` names sccache as
  `rustc-wrapper`, and the first push without the override failed every
  cargo step with `could not execute process sccache`. release.yml keeps its own
  sccache setup (separate cache-scoping rules, see `releasing-aghub`).
- Agent-validation step removed. `--features agent-validation` changes core's
  feature set, so it recompiled core and every dependent and reran the suite
  (66–139s per OS); the runners have no `claude`/`opencode` CLIs and the step
  was `continue-on-error`, so it never gated anything.
  `just test-with-validation` stays for local use.
- Keyring smoke uses `--workspace --test keyring_smoke` instead of
  `-p aghub-inference`: after `just test`, the `-p` form recompiled 59 crates
  under a different feature fingerprint, the `--workspace` form 0.
- TS typecheck moved from the 3-OS Test job into Frontend Lint (ubuntu, still
  forced on every main push); the frontend Vite build runs on ubuntu only.
  Rust Lint and Test no longer install bun — no Rust build reads
  `node_modules`.

rust-cache saves only from main (`save-if`), and no longer with
`cache-on-failure`. The first sccache-less push failed before compiling, and
`cache-on-failure: true` saved its empty target (1183 bytes) under the
exact key; rust-cache never overwrites an exact hit, so every later run
restored nothing and compiled cold until Cargo.lock changed.

Kept: the 3-OS release-profile CLI build (distinct from debug tests),
test-on-every-main-push (release `verify-ci` depends on it),
`cargo fetch --locked`.

## Release build speed

`.github/workflows/release.yml` `build-tauri` "Stage version-locked
aghub-api", and both build jobs' cache setup. Measured 2026-10-11 with a
throwaway branch workflow that ran the release builds without uploading.

The sidecar step built `-p aghub-api` alone, then `tauri build` ran
`cargo build --bins --features tauri/custom-protocol` for the desktop crate.
The two resolve features differently (e.g. `proc-macro2/span-locations`,
`serde_json/unbounded_depth`, `rustls/ring`), so ~445 shared crates compiled
twice. Building the sidecar as
`-p aghub -p aghub-api --bin aghub-api --features tauri/custom-protocol`
gives it the desktop's feature set; `Build Tauri` then compiles only the
Tauri-specific crates. The api binary gains no behaviour: the desktop already
embeds the same api with those features.

Cold desktop job, before → after: macOS arm64 1006s → 588s, Linux 1177s →
638s. Windows 1163s → 808–1106s: Windows runners swung ±300s between
identical runs, which is larger than any knob tried.

Release drops sccache (`RUSTC_WRAPPER: ""`). A tag run's caches are scoped to
that tag, so no later release can read them; v2.40.1 alone left 1179 such
entries that only evicted main's CI caches.

2026-10-21: the release jobs restore (never save) a rust-cache entry that
ci.yml's `build` job saves on main, one per target, because every ref can read
the default branch's caches. That job runs release.yml's own cargo commands
(so a PR also proves the release build) and replaced CI's separate
release-CLI caches. Before it, v2.41.8 took 19.5
min with the Windows desktop job at 18.5 (9.7 sidecar + 6.1 Tauri step, ~600
crates compiled cold); a run on a busy day took 40 min because its macOS jobs
queued ~24 min for the free plan's macOS concurrency. The same change dropped
the Intel Mac legs (0 downloads), which removed two of the four macOS jobs.
rust-cache strips workspace versions from the key, so `just bump` and the
release's version sed do not miss it, and the bump commit's own CI run
refreshes it. It also hashes every non-empty `CARGO*`/`RUST*` env var, which
is why release.yml's build jobs repeat ci.yml's workflow env.

The 10GB repo cache had reached 10.3GB, 3.1GB of it keys superseded by a
Cargo.lock change and kept until LRU eviction, which also takes live entries.
ci.yml's `prune-caches` job deletes those and every tag-scoped cache after
each main run. `[profile.dev.package."*"] debug = false` drops dependency
line tables from test builds (smaller caches, faster links) while workspace
crates keep theirs, which is what the `CARGO_PROFILE_DEV_DEBUG=0` attempt
above lost.

Rejected (two Windows samples each, within noise): a Dev Drive (ReFS) for
target/cargo/rustup; building the desktop crate in the same cargo pass after
an early `bun run build`; `rust-lld` as the MSVC linker; a Defender exclusion
in CI. `CARGO_PROFILE_DEV_DEBUG=0` cut one macOS CI test build 593s → 397s
but was within noise on Windows and drops line numbers from CI backtraces.

## Windows test speed

`.github/workflows/ci.yml` `test`, "Exclude test and build dirs from Windows
Defender". On 2026-10-21 (PR #69, warm cache) Windows ran the suite in 147s
against 23s on Linux. aghub-inference's 94 tests took 186 test-seconds there
(2.8 on Linux): `InferenceProviderStore::open_db` opens a fresh connection and
runs the migrations on every call, so each test's new database is ~20-30
committed transactions, each creating and deleting a journal file that
Defender scans. The CLI `inference_*` tests took 13-37s each (0.07s on Linux).

Excluding the temp dir from Defender took the suite to 83s but left
aghub-inference at 165 test-seconds: the cost was the journal's fsyncs, not
the scan. `open_db` now opens with `journal_mode = WAL` and
`synchronous = NORMAL` (pinned by `database_is_left_in_wal_mode`), which also
speeds every desktop and CLI inference call on Windows.

## CI cache bloat

`.github/workflows/ci.yml` "Drop a partially restored target (main only)".
rust-cache prunes workspace crates and packages no longer in the lockfile
when it saves, but keeps every fingerprint of a crate that is still in it. A
key miss restores the closest older entry, so a Cargo.lock or profile change
compiled the new artifacts next to the old ones and saved both: after the
2026-10-21 `.cargo/config.toml` change, test caches went 1.2 -> 1.8GB and
lint 0.8 -> 1.1GB, the repo hit 11.1GB of its 10GB, and GitHub's LRU evicted
the superseded keys before `prune-caches` could. Main now drops `target/` on
any non-exact hit and saves a clean entry; PRs keep the partial restore. The
cost is one cold main run per lockfile change.

## cargo audit ignores the updater's zip 4

On 2026-10-20 three `zip` advisories (RUSTSEC-2026-0367, -0368, -0369: an
archive extract can write through or create symlinks outside the destination)
turned `cargo audit` red, which blocked the v2.41.3 release at `ci.yml`. Our
own `zip` 8 dependency (`crates/skill`, `src-tauri`) moved to 9. The 4.x copy
comes only from `tauri-plugin-updater`, on Windows, and every published
updater release through 2.14.0 and 3.0.0-alpha.3 still requires `zip ^4`, so
no upgrade removes it.

`.cargo/audit.toml` ignores those three IDs. That is safe only while the
updater extracts nothing it has not verified: `Update::download()` checks the
minisign signature against the `tauri.conf.json` pubkey before `install()`
hands the bytes to `extract_zip`, so an attacker-built archive never reaches
the extractor. Remove the ignores when the updater depends on `zip >= 9`, and
never widen them to cover a `zip` our own code calls.

## Literal tabs inside a string literal

A string whose `\`+newline continuation was removed kept the next line's
indentation tabs inside the text, on the same line. It shipped in a user-facing rename refusal
(`source \t\t\t 'o/r' does not name a host`), and three more copies sat in
test messages. rustfmt and clippy accept it, and the tests only asserted
`contains(..)`. Because the Rust here is tab-indented, a tab after non-blank
text on a line is this bug in practice (intended tab data uses `\t`; a tab
aligning a trailing comment would also trip it). `scripts/check-string-tabs.sh` (preflight
and the CI Format job) fails on it.

## CI apt tail and cargo index

`scripts/ci-apt-install.sh` and the "Cargo index cache" steps in
`.github/workflows/ci.yml`. Measured over the 60 CI runs up to 2026-10-23.

The GTK/webkit install is ~36s at the median on each of the three Linux jobs,
but 5 of 138 took 8-23 minutes: the runner's azure mirror served the 73MB
download at ~60 kB/s, so one slow leg set the whole run's wall clock. The
script downloads with a 90s timeout (download only, so dpkg is never
interrupted) and retries against `archive.ubuntu.com`. Its first PR run hit
the slow mirror once; the retry finished in 13s.

Every cargo job printed `Updating crates.io index` even on an exact rust-cache
hit: 3s on Linux, 17s on macOS, 44-48s on each Windows job, one of them on the
PR critical path (Build Windows). rust-cache prunes the index before saving it,
and the pruned copy is incomplete: `CARGO_NET_OFFLINE` on an exact hit failed
with `no matching package named dirs`. A separate cache of
`~/.cargo/registry/index`, keyed on Cargo.lock and restored after rust-cache,
gives cargo the whole index.

Warm PR rerun, Windows: `cargo fetch` 44s -> 34s (no index download; the rest
is unpacking `.crate` sources, which rust-cache does not keep), the Build job's
gap before the first compile 51s -> 36s.

Rejected: a Windows bun package cache. `bun install` fell 58s -> 10s, but the
cache restore took 14s and the following `tsc` paid ~20s more reading the
freshly linked node_modules, so the step went 68s -> 63s.

Not changed: the three release cargo builds in `build` recompile the
workspace crates under two feature sets on purpose (see Release build speed);
merging them would leave release.yml's `build-cli` cold.

## PR builds Linux only

`.github/workflows/ci.yml` `build` matrix. On 2026-10-23 the warm PR wall
clock was ~8.5 min, all of it Build Windows (three release cargo builds,
~6 min, kept for release-cache parity, see Release build speed); every other
PR job finished within ~5 min.

What only `build` proves is the release shape: `tauri/custom-protocol` and
the embedded `../dist` (Test runs debug, which serves `devUrl` and never
embeds the frontend). That is the same code on every OS, and the
Windows/macOS-specific parts (`tauri-build` resources, `cfg` code) already
compile in Test and Rust Lint on those runners. PRs therefore build Linux
only; main still builds all three targets, saves their caches, and must be
green for `verify-ci` before any tag, so a Windows/macOS-only release break
surfaces on main after merge instead of on the PR. That is why `build` runs on
every main push again (it had been path-filtered there for a day): a docs-only
successor would otherwise skip it and give `verify-ci` a green run on code
whose Windows build had failed. Measured on the PR itself: wall clock
~8.5 -> 5.6 min, Build Linux now the longest job.

The PR's Linux build then ran its four cargo builds in sequence (257s), each
ending in one large release crate (aghub-api, the desktop crate). A PR splits
it into two parallel legs on the same restored cache: `app` (sidecar +
desktop, needs the frontend) and `bins` (standalone CLI + api). Main keeps one
`all` job per target, since that is the job that saves the cache.
