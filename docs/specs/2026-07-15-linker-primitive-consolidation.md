# Consolidate link-preserving filesystem primitives into the core Linker

**Status**: proposed → implementing on `refactor/codebase-deepening`
**Scope**: candidate C from the 2026-07-15 architecture review. The safe,
in-process on-ramp to candidate A (skill-rename transaction extraction).

## Problem

The skill-rename transaction needs three low-level, cross-platform filesystem
primitives that **preserve reparse points** (Unix symlinks and Windows
symlinks/junctions) as it snapshots, restores, and rolls back:

- `xplat_symlink(target, link)` — create a raw link, no conflict detection
- `create_junction(abs_target, link)` — Windows `mklink /J` fallback
- `copy_dir_recursive(src, dst)` — deep copy that **re-creates** links as links

These are declared **twice**, byte-for-byte, in the two surfaces that run the
rename:

- `crates/cli/src/commands/source.rs` (`xplat_symlink`, `create_junction`,
  `copy_dir_recursive`)
- `crates/api/src/routes/skills_update.rs` (same three)

The API side has rename tests; the CLI side has **none**. The Windows-junction
invariant they encode is subtle and safety-critical — "never `remove_dir_all` a
junction, it deletes the Master" — yet a fix must be applied in both copies by
hand. `crates/api/.../skills_update.rs:822` even documents the coupling:
_"Mirrors the project linker's `create_junction` (which is crate-private to
`aghub-core`)."_ The primitive already exists in core; it is just unreachable.

### What is NOT duplication (do not touch)

The name `copy_dir_recursive` appears in three more places, but each has a
**deliberately different** copy semantics. Merging them would be a behavior bug:

| Location                                                   | Semantics                                                              | Why distinct                                            |
| ---------------------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------- |
| `core/skills/linker/mod.rs` (private)                      | **dereferences** symlinks + applies npx `EXCLUDE_FILES`/`EXCLUDE_DIRS` | materializes the Master so it hashes identically to npx |
| `core/skills/update.rs` `copy_dir_recursive_skip_symlinks` | **skips** symlinks                                                     | skill-swap staging must not carry links                 |
| `core/transfer.rs`                                         | plain deep copy (`fs::copy` follows links)                             | isolated-copy install to a foreign agent dir            |

Only the **link-preserving** pair (CLI + API) is the true twin. This corrects
the architecture-review card, which over-counted "five scattered copies" — there
is one genuinely-duplicated strategy, plus three same-named-but-different ones.

## Solution

Add two public methods to the existing zero-sized `Linker` namespace in
`crates/core/src/skills/linker/mod.rs`, reusing the module's existing private
`create_junction`:

```rust
impl Linker {
    /// Create a raw cross-platform link at `link` pointing at `target`
    /// (no conflict detection — the caller guarantees an empty slot).
    /// Unix: one `symlink` syscall. Windows: pick `symlink_dir`/`symlink_file`
    /// by the resolved target's metadata, junction-fallback for dir targets.
    pub fn symlink(target: &Path, link: &Path) -> io::Result<()>;

    /// Deep-copy a real directory tree, RE-CREATING every reparse point
    /// (via `Self::is_link` + `Self::symlink`) as a link rather than
    /// deep-copying it. The link-preserving copy the rename snapshot needs.
    pub fn copy_preserving_links(src: &Path, dst: &Path) -> io::Result<()>;
}
```

- `Linker::symlink` is the verbatim port of the CLI/API `xplat_symlink`,
  delegating on Windows to the module-private `create_junction`.
- `Linker::copy_preserving_links` is the verbatim port of the CLI/API
  link-preserving `copy_dir_recursive`. It is **named to distinguish it** from
  the module's private master-materialization `copy_dir_recursive` — the module
  now holds two copy strategies and their difference must read at the call site.
- The Windows `create_junction` stays module-private; it is an implementation
  detail of `symlink`, not part of the interface.

CLI and API then **delete** their three helpers and call `Linker::symlink` /
`Linker::copy_preserving_links`. They already import
`aghub_core::skills::linker::Linker`.

### Interface (the test surface)

Two methods, both `(&Path, &Path) -> io::Result<()>`. Everything hard —
junction fallback, reparse-point detection, file-vs-dir link kind on Windows —
sits behind them. Depth: the whole cross-platform link/copy problem behind two
one-line signatures. This is where the tests go.

## Dependency category

**In-process** (pure filesystem, no I/O injection needed). Tested directly
through the interface with `tempfile::tempdir()` in the linker's existing
`#[cfg(test)] mod tests` — no adapter, no seam beyond the method boundary.

## Tests (add to `linker/mod.rs` tests, Unix-gated where needed)

Written first (red), then the methods (green):

1. `symlink_roundtrips_a_dir_target` — `symlink` a real dir, assert
   `is_link(link)` and that `read_link` resolves to the target.
2. `copy_preserving_links_recreates_a_symlink` — a tree containing a symlink;
   after `copy_preserving_links`, the copied entry is still a link
   (`is_link` true), not a materialized directory.
3. `copy_preserving_links_deep_copies_real_dirs` — a nested real dir copies as
   real dirs/files, and mutating the copy does not touch the source.

Unix-gate the symlink assertions (the existing module already gates Windows
paths behind `#[cfg(windows)]`). The Windows junction arm keeps compiling via
the shared `create_junction`; no behavior change on Windows.

## Non-goals

- No change to `Linker::link` (high-level, conflict-detecting) or to the three
  distinct copy strategies above.
- No API/CLI behavior change — pure relocation of identical code behind one
  tested interface.

## Wins

- **locality**: the junction-safety invariant lives in one place; a fix lands
  once, both surfaces inherit it.
- **leverage**: two methods, two call sites today, plus every future caller
  (candidate A's transaction).
- **interface is the test surface**: primitives that had zero tests anywhere
  gain coverage in core.
- de-risks candidate A: its snapshot/restore is then built on one trusted
  linker surface instead of its own copies.

## Codex-review follow-up (2026-07-16)

Codex's adversarial pass flagged that routing snapshot/restore through the
module's `create_junction` was NOT byte-equivalent on Windows: `create_junction`
runs `normalize_path`, whose old body used `to_string_lossy()` — an ill-formed
UTF-16 component would be corrupted to `U+FFFD`, so a junction snapshot could
capture the wrong target and a later rollback delete the live skill. The deleted
helpers passed the raw `Path`.

Fix (root-cause, benefits the pre-existing `link()` path too): `normalize_path`
now rewrites `/`→`\` over the raw UTF-16 code units (`encode_wide`/`from_wide`),
never through a lossy `String`. For a real (valid, backslash) path the result is
byte-identical to the raw path, so the port is behavior-preserving on Windows.
Also strengthened the copy test to use a RELATIVE symlink target and assert
`read_link` equality (the target is preserved verbatim, not rebased).

Known gap (accepted): the Windows junction arm is not exercised by a test that
runs on the Linux dev/CI host; adding an unverifiable `#[cfg(windows)]` test was
declined over the risk of shipping broken Windows-only test code.

## Rollback

Pure relocation + one lossless-ness fix to `normalize_path`; revert the single
commit to restore the inline copies. No lock format, on-disk layout, or public
API DTO changes.
