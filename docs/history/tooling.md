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

- sccache dropped from ci.yml; rust-cache stays. release.yml keeps its own
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

Kept: the 3-OS release-profile CLI build (distinct from debug tests),
test-on-every-main-push (release `verify-ci` depends on it),
`cargo fetch --locked`.
