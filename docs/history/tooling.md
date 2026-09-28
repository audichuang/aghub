# Tooling history

Build, task-runner and local-environment incidents (justfile, cargo, disk).

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
