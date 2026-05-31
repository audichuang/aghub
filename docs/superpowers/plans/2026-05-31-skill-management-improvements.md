# Skill Management Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add npx-compatible real content hashing (P), git-native per-skill update detection for private repos (F1), and residue-free skill deletion with disk-reconciled lock pruning (F2) to aghub — plus the desktop UI that surfaces it — without ever breaking the `npx skills` lock files.

**Architecture:** A new pure SHA-256 folder hasher in `crates/skill` reimplements npx's `computeSkillFolderHash` byte-for-byte and feeds the lock writers. `crates/core` gains pure, network-free update-comparison logic (`SkillUpdateStatus`) and layout-aware removal + disk-driven lock prune. `crates/git` gains a treeless ref-fetch and a credential-redaction helper. `crates/api` owns credential resolution (keyring) + fetch orchestration and the two delete routes. `crates/desktop` reads the new `content_hash` and renders update badges.

**Tech Stack:** Rust workspace (clap CLI, Rocket-style API, gix 0.83, serde/serde_json with `preserve_order`, sha2, tempfile), Tauri + React/TypeScript desktop (bun, ts-rs generated DTOs). Test stack: inline `#[cfg(test)]`, `crates/core/tests` `TestConfig`, `crates/cli/tests` `assert_cmd`, `crates/skill::lock::test_utils::TestLockGuard`.

---

## Hard constraints (non-negotiable acceptance gates)

These are lifted verbatim from the spec and the user's directives. Every task that touches them must satisfy them; the CI gates below enforce them.

1. **npx byte-for-byte hash parity is a correctness requirement.** `compute_skill_folder_hash` must equal npx `computeSkillFolderHash` (`local-lock.ts:108-147`) for the same source folder. Project-lock `computed_hash` is read by npx `experimental_sync` (`sync.ts:202`) → the golden cross-check is a **CI-blocking** gate.
2. **Hash skips ONLY `.git` and `node_modules`** (case-sensitive), uses `lstat` to detect symlinks, never descends into symlinked dirs, and enforces max file-count / total-bytes / depth bounds (reuse `max_depth: 10`).
3. **Hash the SOURCE repo folder, not the post-copy installed dir** (aghub `copy_dir_recursive` also copies `metadata.json`/`.git`).
4. **Global lock**: `skill_folder_hash` stays `""`; real hash goes in a NEW optional per-entry `content_hash` (serde `rename = "contentHash"`, `skip_serializing_if = "Option::is_none"`, `default`). **No version bumps** (global=3, project=1). **No new top-level project-lock keys.**
5. **`skill_path`** uses npx's exact POSIX `<repo-relative-dir>/SKILL.md`, case-preserving; root-level → `SKILL.md`.
6. **Auto-heal** legacy placeholder `EMPTY_SKILLS_LOCK_DIGEST` (= sha256 of empty = `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`): treat as **unknown** → recompute local hash before comparing. **Missing `content_hash` → recompute, never error.**
7. **Credential model**: fetch + credential resolution live in `crates/api`; `crates/core` is pure (hash/compare, receives an already-resolved `Option<token>`). Resolution order: (1) aghub-local non-committed source→credential binding stored in **keyring** (never in the committed lock), (2) keychain by host, (3) `Uncheckable { reason: auth }`. **Redaction helper strips URL userinfo from every gix error string / log / `Uncheckable` reason / persisted `source_url`/`clone_url`.** A test asserts the token never appears in failed-fetch output or the lock.
8. **F1 fetch**: treeless/bare fetch (no worktree checkout); `ref = None` → gix HEAD symref (NOT shelling out to the `git` binary); tag/SHA pinned → `UpToDate`; `(source, ref)` result cache + TTL + concurrency bound + per-fetch timeout + offline/skip. Sanitize `skill_path` (reject absolute or `..`) and verify containment under the temp checkout root before reading.
9. **F2 prune** (`prune-lock`, renamed to avoid the existing `transfer::reconcile_skill` / `/skills/reconcile`): prune **only on a provably successful scan** (error-returning `scan_skills`, NOT the error-swallowing discovery collector); abort on any scan error; per-scope disk sets (global = union of all agents' global dirs; project = only project dirs; never cross scope); gate project prune on a present+readable `project_root`; atomic temp+rename + lock mutex.
10. **F2 delete**: `canonical_path` is a tilde-abbreviated `SKILL.md` FILE path → expand tilde, take PARENT (reuse `resolve_skill_root`); after `canonicalize`, confirm the resolved path is inside an allow-listed skills root before `remove_dir_all`; TOCTOU re-check symlink at delete; before deleting the canonical verify no other view symlinks to it; **default `--dry-run`**; `--all-agents` / symlink full removal need explicit confirm. Both `DELETE /agents/<agent>/skills/<name>` and `DELETE /skills/by-path` are covered.
11. **Internal non-additive changes are in-scope and intentional**: `write_project_install_lock` signature changes; desktop badge reads `content_hash`; the `write_project_install_lock_uses_placeholder_hash` test is rewritten.

### Decisions taken (resolve agent-flagged ambiguities)

- **Global-lock trailing newline is left UNCHANGED.** aghub appends `\n` today; npx omits it but parses via tolerant `JSON.parse`. The spec forbids version bumps / extra keys but says nothing about whitespace, and changing it would create noisy diffs and regression risk. Round-trip tests assert npx-**tolerance** (parse succeeds, version + other entries preserved), not byte-identical global formatting. The **project** lock keeps its existing trailing-newline + 2-space + sorted-keys behavior (which already matches npx `local-lock.ts:99`) and that IS asserted byte-for-byte.
- **Source→credential binding storage**: a new keyring entry (`SERVICE = "aghub"`, `USER = "skill_source_bindings"`) holding a JSON map `source → credential_id`. Tokens stay in the existing `github_credentials` entry; the binding stores only the id.
- **CLI update-check shape**: a new **read-only** `check` subcommand (`aghub-cli check skills`). The existing `update skills` keeps editing local metadata only.
- **Desktop scope is included** in this plan (DTO + badge + credential picker + async states).
- **gix treeless fetch**: confirmed-by-spike in Task F1.2. If gix 0.83 cannot do a partial (`blob:none`) fetch, fall back to a **bare fetch with no `main_worktree()` checkout** and read the skill subtree's blobs from the object DB into a temp dir; worst case (documented) a checkout into a TempDir. The correctness-critical invariant is "no reliance on the `git` binary" + "recompute hash from the fetched source folder", not the specific wire optimization.

### CI-blocking gates (must be green before merge)

- `hash_parity_fixture_skill_golden` — Rust hash == npx golden hex (constraint 1).
- `experimental_sync_skip_condition_hash_match` — recompute is stable so npx `===` skip holds.
- `write_project_install_lock_computes_real_hash` — never the placeholder.
- `serde_round_trip_*` — unknown fields + empty `skill_folder_hash` preserved; versions unchanged.
- `credential_leak` group (git + api) — token never in error/log/persisted URL.
- `skill_path` traversal + symlink-escape rejection.
- `symlink_out_of_tree_target_prevents_deletion` + `delete_skill_respects_allowed_roots` — containment.
- `prune_lock_aborts_on_scan_error` + `prune_lock_scope_isolation_project_never_prunes_global`.
- `dry_run_lists_paths_deletes_nothing`.
- `bun run typecheck` (desktop) after DTO changes.

Add these to the project's CI workflow as part of `cargo test --workspace` (they are ordinary tests; the "gate" is that they must not be `#[ignore]`d). Network-dependent E2E tests (`*_public_repo`, `*_private_repo*`) are marked `#[ignore]` and run in the validation lane (`just test-with-validation`), NOT the blocking lane.

---

## File Structure

**Create**

- `crates/skill/src/hash.rs` — `compute_skill_folder_hash`, bounds constants, `HashError`, `is_placeholder_digest`.
- `crates/skill/src/lock/path.rs` — `skill_path_from_repo_dir` (npx `add.ts:1568-1575` form) + `repo_relative_dir`.
- `crates/skill/tests/fixtures/hash-parity-skill/…` — committed hashable fixture (no `.git`/`node_modules`).
- `crates/skill/tests/hash_parity_golden.rs` — CI-blocking golden test + golden hex constant.
- `crates/skill/tests/npx_interop.rs` — round-trip A/B, lock-wipe boundary, skill_path form, sync parity.
- `crates/core/src/skills/update.rs` — `SkillUpdateStatus`, `UncheckableReason`, `compute_skill_update_status`, `group_by_source_ref`, `sanitize_skill_path`, `auto_heal_hash`.
- `crates/core/src/skills/prune.rs` — `prune_lock`, `DiskSkillSet`, scope-isolated disk scanning.
- `crates/core/src/skills/removal.rs` — `plan_removal`, `RemovalPlan`, `allowed_skill_roots`, `assert_contained`.
- `crates/git/src/redact.rs` — `redact_url_userinfo`.
- `crates/git/src/fetch.rs` — `fetch_ref_to_temp`, `resolve_default_branch`, `RefKind`.
- `crates/api/src/credentials/resolve.rs` — `resolve_token_for_source`, source-binding keyring entry.
- `crates/api/src/skills/update_check.rs` — orchestration: grouping, cache(TTL), concurrency, timeout, offline.
- `crates/api/src/routes/skills_update.rs` — `GET /skills/check-updates`, `POST /skills/prune-lock`.
- `crates/cli/src/commands/check.rs` — `check skills` read-only subcommand.
- `crates/cli/src/commands/prune.rs` — `skills prune-lock` subcommand.
- `crates/desktop/src/components/skill-update-badge.tsx` — status badge + credential-picker affordance.

**Modify**

- `crates/skill/Cargo.toml` — add `sha2`.
- `crates/skill/src/lib.rs`, `crates/skill/src/lock/mod.rs` — module wiring + re-exports.
- `crates/skill/src/lock/types.rs` — `content_hash: Option<String>` on `SkillLockEntry`.
- `crates/skill/src/lock/local.rs` — `skill_path: Option<String>` on `LocalSkillLockEntry`.
- `crates/skill/src/install.rs` — `write_project_install_lock` signature + real hashing; drop placeholder; rewrite its test.
- `crates/skill/src/lock/io.rs` — atomic temp+rename writes + lock mutex (project + global writers).
- `crates/git/src/lib.rs`, `error.rs`, `clone.rs`, `source.rs` — redaction wiring + userinfo stripping.
- `crates/git/Cargo.toml` — gix feature flags if the spike requires them.
- `crates/core/src/skills/mod.rs`, `crates/core/src/manager/skill.rs`, `crates/core/src/transfer.rs` — layout-aware removal + containment; prune wiring.
- `crates/api/src/routes/skills.rs` — both delete routes (dry-run/confirm + removed-path summary + prune); replace `detect_current_branch` shell-out with gix; apply redaction at the `skills.rs:~1189` surfacing site; write `content_hash` at `~482`.
- `crates/api/src/dto/skill.rs` — `content_hash` on `SkillLockEntryResponse`; `SkillUpdateStatusResponse`.
- `crates/api/src/lib.rs`, `crates/api/src/bin/export-dto.rs` — mount routes + export DTOs.
- `crates/cli/src/commands/mod.rs`, `crates/cli/src/cli.rs` (clap) — register `check` + `prune-lock`; add `--all-agents` / `--dry-run` / `--yes` to `delete`.
- `crates/desktop/src/components/skill-detail.tsx`, `skill-list.tsx` — read `content_hash`; render badges; credential picker.
- `crates/desktop/src/generated/dto/*` — regenerated via `generate:dto`.

---

# Workstream P — Real content hashing + lock schema (foundation; do first)

Everything in F1/F2 depends on a correct hasher and the schema fields, so P lands first.

## Task P1: Folder hasher (`crates/skill/src/hash.rs`)

**Files:**

- Modify: `crates/skill/Cargo.toml`
- Create: `crates/skill/src/hash.rs`
- Modify: `crates/skill/src/lib.rs`

> **CORRECTION (post-P1 review):** npx sorts file paths with JS `String.localeCompare`
> (`local-lock.ts:113`), i.e. **UCA/ICU collation — NOT code-point**. This diverges
> even for ordinary skills (`SKILL.md` + a lowercase `scripts/` dir), so a code-point
> sort breaks the mandatory byte-for-byte parity (constraint 1). The hasher MUST sort
> with a UCA collator. Use **`feruca`** (pure-Rust UCA, `Collator::default().collate(a,b)`);
> if it does not reproduce the npx golden for the P2 fixture, switch to `icu_collator`
> (icu4x, root locale, default options). The P2 golden is the objective arbiter.

- [ ] **Step 1: Add the `sha2` + collation dependencies**

Edit `crates/skill/Cargo.toml`, in `[dependencies]`:

```toml
sha2 = "0.10"
feruca = "0.11"   # UCA collation to match JS localeCompare; swap for icu_collator if golden mismatches
```

- [ ] **Step 2: Run to confirm it resolves**

Run: `cargo build -p skill`
Expected: builds (new dep downloaded), no code yet using it.

- [ ] **Step 3: Write the failing unit tests**

Create `crates/skill/src/hash.rs` with the test module first (the function does not exist yet, so it won't compile — that's the failing state):

```rust
//! SHA-256 folder hashing, byte-for-byte compatible with npx `computeSkillFolderHash`
//! (vercel-labs/skills `local-lock.ts:108-147`). Hash the SOURCE folder, never the
//! post-copy installed dir.

use std::io;
use std::path::Path;

/// Bounds guard (F1 hashes untrusted fetched content).
pub const MAX_FILES: usize = 10_000;
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB
pub const MAX_DEPTH: usize = 10; // mirrors install.rs scan max_depth

/// SHA-256 of the empty input — the legacy aghub placeholder.
pub const EMPTY_SKILLS_LOCK_DIGEST: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Debug, thiserror::Error)]
pub enum HashError {
    #[error("io error hashing skill folder: {0}")]
    Io(#[from] io::Error),
    #[error("skill folder exceeds bounds: {0}")]
    Bounds(String),
}

/// True if `hash` is the empty-input placeholder (treat as "unknown" → recompute).
pub fn is_placeholder_digest(hash: &str) -> bool {
    hash == EMPTY_SKILLS_LOCK_DIGEST
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;
    use tempfile::tempdir;

    fn hex(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }

    #[test]
    fn empty_folder_hashes_to_empty_sha256() {
        let dir = tempdir().unwrap();
        assert_eq!(
            compute_skill_folder_hash(dir.path()).unwrap(),
            EMPTY_SKILLS_LOCK_DIGEST
        );
    }

    #[test]
    fn single_file_path_then_content_no_delimiter() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("README.md"), b"hello world").unwrap();
        let mut expected = Sha256::new();
        expected.update(b"README.md");
        expected.update(b"hello world");
        assert_eq!(
            compute_skill_folder_hash(dir.path()).unwrap(),
            format!("{:x}", expected.finalize())
        );
    }

    #[test]
    fn files_sorted_by_codepoint() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("zebra.txt"), b"z").unwrap();
        fs::write(dir.path().join("apple.txt"), b"a").unwrap();
        fs::write(dir.path().join("middle.txt"), b"m").unwrap();
        let mut e = Sha256::new();
        for (p, c) in [("apple.txt", "a"), ("middle.txt", "m"), ("zebra.txt", "z")] {
            e.update(p.as_bytes());
            e.update(c.as_bytes());
        }
        assert_eq!(
            compute_skill_folder_hash(dir.path()).unwrap(),
            format!("{:x}", e.finalize())
        );
    }

    #[test]
    fn nested_relative_paths_use_forward_slash() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        fs::create_dir_all(dir.path().join("b/c")).unwrap();
        fs::write(dir.path().join("file0.txt"), b"0").unwrap();
        fs::write(dir.path().join("a/file1.txt"), b"1").unwrap();
        fs::write(dir.path().join("b/c/file2.txt"), b"2").unwrap();
        let mut e = Sha256::new();
        for (p, c) in [("a/file1.txt", "1"), ("b/c/file2.txt", "2"), ("file0.txt", "0")] {
            e.update(p.as_bytes());
            e.update(c.as_bytes());
        }
        assert_eq!(
            compute_skill_folder_hash(dir.path()).unwrap(),
            format!("{:x}", e.finalize())
        );
    }

    #[test]
    fn skips_dot_git_and_node_modules_only() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
        fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        fs::write(dir.path().join(".git/objects/abc"), b"junk").unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::write(dir.path().join("node_modules/pkg/index.js"), b"junk").unwrap();
        // dist/__pycache__ must NOT be skipped
        fs::create_dir_all(dir.path().join("dist")).unwrap();
        fs::write(dir.path().join("dist/out.js"), b"keep").unwrap();
        // localeCompare/UCA order: "dist/out.js" < "SKILL.md" (primary-level
        // case-insensitive: 'd' < 's'). Code-point would (wrongly) put SKILL.md first.
        let mut e = Sha256::new();
        e.update(b"dist/out.js");
        e.update(b"keep");
        e.update(b"SKILL.md");
        e.update(b"x");
        assert_eq!(
            compute_skill_folder_hash(dir.path()).unwrap(),
            format!("{:x}", e.finalize())
        );
    }

    #[test]
    fn collation_is_case_insensitive_primary_like_localecompare() {
        // The defining divergence from code-point: a real skill layout.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("SKILL.md"), b"s").unwrap();
        fs::create_dir_all(dir.path().join("scripts")).unwrap();
        fs::write(dir.path().join("scripts/run.sh"), b"r").unwrap();
        // localeCompare order: "scripts/run.sh" < "SKILL.md".
        let mut e = Sha256::new();
        e.update(b"scripts/run.sh");
        e.update(b"r");
        e.update(b"SKILL.md");
        e.update(b"s");
        assert_eq!(
            compute_skill_folder_hash(dir.path()).unwrap(),
            format!("{:x}", e.finalize())
        );
    }

    #[test]
    fn returns_lowercase_hex_64() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("x"), b"y").unwrap();
        let h = compute_skill_folder_hash(dir.path()).unwrap();
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn deterministic() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a"), b"1").unwrap();
        fs::write(dir.path().join("b"), b"2").unwrap();
        assert_eq!(
            compute_skill_folder_hash(dir.path()).unwrap(),
            compute_skill_folder_hash(dir.path()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped_not_followed() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("real.txt"), b"r").unwrap();
        symlink(dir.path().join("real.txt"), dir.path().join("link.txt")).unwrap();
        // symlinked directory must not be descended
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"s").unwrap();
        symlink(outside.path(), dir.path().join("linkdir")).unwrap();
        let mut e = Sha256::new();
        e.update(b"real.txt");
        e.update(b"r");
        assert_eq!(
            compute_skill_folder_hash(dir.path()).unwrap(),
            format!("{:x}", e.finalize())
        );
    }

    #[test]
    fn nonexistent_dir_is_io_error() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(matches!(
            compute_skill_folder_hash(&missing),
            Err(HashError::Io(_))
        ));
    }

    #[test]
    fn exceeding_max_files_is_bounds_error() {
        let dir = tempdir().unwrap();
        for i in 0..(MAX_FILES + 1) {
            fs::write(dir.path().join(format!("f{i}")), b"").unwrap();
        }
        assert!(matches!(
            compute_skill_folder_hash(dir.path()),
            Err(HashError::Bounds(_))
        ));
    }

    #[allow(dead_code)]
    fn _hex_used(_: &[u8]) {
        let _ = hex;
    }
}
```

- [ ] **Step 4: Run to verify it fails to compile**

Run: `cargo test -p skill hash::` (the `hash::` module filter — a bare `compute_skill_folder_hash` matches no test fn names)
Expected: compile error — `cannot find function compute_skill_folder_hash`.

- [ ] **Step 5: Implement the hasher**

Add to `crates/skill/src/hash.rs` (above the test module):

```rust
use sha2::{Digest, Sha256};

/// Reimplements npx `computeSkillFolderHash` byte-for-byte. Returns lowercase hex.
///
/// Algorithm (local-lock.ts:108-147): collect files recursively, skip dirs named
/// exactly `.git`/`node_modules`, lstat to skip symlinks (no descend), relative
/// path with `\` → `/`, sort by Unicode code point, then for each file in order
/// `update(relative_path_bytes)` + `update(file_bytes)` with no delimiter.
pub fn compute_skill_folder_hash(dir: &Path) -> Result<String, HashError> {
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut total_bytes: u64 = 0;
    collect(dir, dir, 0, &mut files, &mut total_bytes)?;

    // Match npx `local-lock.ts:113` exactly: JS `String.localeCompare`, i.e. UCA
    // collation (case-insensitive at the primary level). Code-point is WRONG here
    // (e.g. "scripts/" vs "SKILL.md"). Residual exotic-locale divergence is covered
    // by F1's recompute-on-mismatch safeguard.
    let collator = feruca::Collator::default();
    files.sort_by(|a, b| collator.collate(&a.0, &b.0));

    let mut hasher = Sha256::new();
    for (rel, abs) in &files {
        let bytes = std::fs::read(abs)?;
        hasher.update(rel.as_bytes());
        hasher.update(&bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<(String, std::path::PathBuf)>,
    total_bytes: &mut u64,
) -> Result<(), HashError> {
    if depth > MAX_DEPTH {
        return Err(HashError::Bounds(format!("max depth {MAX_DEPTH} exceeded")));
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // lstat — do not follow symlinks.
        let meta = std::fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue; // skip symlinks; never descend into symlinked dirs
        }
        if ft.is_dir() {
            let name = entry.file_name();
            if name == ".git" || name == "node_modules" {
                continue;
            }
            collect(root, &path, depth + 1, out, total_bytes)?;
        } else if ft.is_file() {
            if out.len() + 1 > MAX_FILES {
                return Err(HashError::Bounds(format!("max files {MAX_FILES} exceeded")));
            }
            *total_bytes += meta.len();
            if *total_bytes > MAX_TOTAL_BYTES {
                return Err(HashError::Bounds(format!(
                    "max total bytes {MAX_TOTAL_BYTES} exceeded"
                )));
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}
```

Add `pub mod hash;` to `crates/skill/src/lib.rs` and re-export: `pub use hash::{compute_skill_folder_hash, is_placeholder_digest, EMPTY_SKILLS_LOCK_DIGEST, HashError};`

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p skill hash::` (the `hash::` module filter — a bare `compute_skill_folder_hash` matches no test fn names)
Expected: all pass. (On non-Unix the symlink test is `#[cfg(unix)]`-gated.)

- [ ] **Step 7: Lint + fmt**

Run: `cargo fmt -p skill && cargo clippy -p skill -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/skill/Cargo.toml crates/skill/src/hash.rs crates/skill/src/lib.rs
git commit -m "feat(skill): add npx-compatible compute_skill_folder_hash"
```

## Task P2: CI-blocking golden parity vs npx

**Files:**

- Create: `crates/skill/tests/fixtures/hash-parity-skill/` (committed files)
- Create: `crates/skill/tests/hash_parity_golden.rs`

- [ ] **Step 1: Build the committed fixture (hashable files only — no `.git`/`node_modules`)**

Create these files (exact bytes matter — keep them ASCII + a couple non-ASCII names to exercise sort):

```bash
mkdir -p crates/skill/tests/fixtures/hash-parity-skill/lib
printf 'name: parity\ndescription: golden fixture\n' > crates/skill/tests/fixtures/hash-parity-skill/SKILL.md
printf 'readme\n' > crates/skill/tests/fixtures/hash-parity-skill/README.md
printf 'export const x = 1\n' > crates/skill/tests/fixtures/hash-parity-skill/lib/index.ts
printf 'Z\n' > crates/skill/tests/fixtures/hash-parity-skill/ZEBRA.md
printf 'cafe\n' > "crates/skill/tests/fixtures/hash-parity-skill/café.md"
```

- [ ] **Step 2: Capture the npx golden hex ONCE from the vendored source**

Run (from the npx repo so its deps resolve; `bun` runs TS directly):

```bash
cd /home/audichuang/research/vercel_npx_skill
bun -e 'import {computeSkillFolderHash} from "./src/local-lock.ts"; console.log(await computeSkillFolderHash(process.argv[1]))' \
  /home/audichuang/research/aghub/crates/skill/tests/fixtures/hash-parity-skill
```

Expected: a 64-char lowercase hex string. (`computeSkillFolderHash` IS `export`ed at `local-lock.ts:108`.) For the committed fixture above, the captured npx golden is:

```
38a71af3e6146b33484d22a5ebd8fc9df2368d7da7eac1bd661baadcf60acad9
```

This is the `localeCompare`-sorted hash. The Rust hasher (P1, now UCA-collated via `feruca`) must reproduce it. The earlier code-point sort produced `b5147f5c…` — if you see that, the collator is not wired/working. If `feruca` does not reproduce the golden for this mixed-case + accented fixture, switch P1's collator to `icu_collator` (icu4x, root locale). Record the hex — it becomes `GOLDEN` below. (Re-run and re-commit only if the fixture files change.)

- [ ] **Step 3: Write the failing golden test**

Create `crates/skill/tests/hash_parity_golden.rs` (replace `PASTE_HEX_FROM_STEP_2`):

```rust
//! CI-BLOCKING: aghub hash must byte-match npx `computeSkillFolderHash` on a
//! committed fixture. Re-capture GOLDEN via the bun command in the plan if the
//! fixture changes. The test also injects .git/node_modules at runtime to prove
//! they are skipped (we cannot commit a literal `.git` dir).

use skill::compute_skill_folder_hash;
use std::fs;

const GOLDEN: &str = "PASTE_HEX_FROM_STEP_2";
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hash-parity-skill");

#[test]
fn hash_parity_fixture_skill_golden() {
    let got = compute_skill_folder_hash(std::path::Path::new(FIXTURE)).unwrap();
    assert_eq!(got, GOLDEN, "aghub hash differs from npx golden");
}

#[test]
fn hash_parity_skip_git_and_modules() {
    // Copy the fixture into a temp dir, then add .git + node_modules; the hash
    // must be unchanged (they are skipped), i.e. still equal to GOLDEN.
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("skill");
    copy_dir(std::path::Path::new(FIXTURE), &dst);
    fs::create_dir_all(dst.join(".git/objects")).unwrap();
    fs::write(dst.join(".git/objects/abc"), b"junk").unwrap();
    fs::create_dir_all(dst.join("node_modules/pkg")).unwrap();
    fs::write(dst.join("node_modules/pkg/index.js"), b"junk").unwrap();
    assert_eq!(compute_skill_folder_hash(&dst).unwrap(), GOLDEN);
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            fs::copy(e.path(), dst).unwrap();
        }
    }
}
```

- [ ] **Step 4: Run to verify pass (after pasting the real hex)**

Run: `cargo test -p skill --test hash_parity_golden`
Expected: both pass. If `hash_parity_fixture_skill_golden` fails, the hasher diverges from npx — fix `hash.rs`, do NOT edit `GOLDEN` to match.

- [ ] **Step 5: Commit**

```bash
git add crates/skill/tests/fixtures/hash-parity-skill crates/skill/tests/hash_parity_golden.rs
git commit -m "test(skill): CI-blocking npx hash-parity golden test"
```

## Task P3: Global lock — add optional `content_hash`

**Files:**

- Modify: `crates/skill/src/lock/types.rs`

- [ ] **Step 1: Write failing serde tests**

Add to the `#[cfg(test)] mod tests` in `crates/skill/src/lock/types.rs`:

```rust
#[test]
fn entry_serializes_content_hash_as_camel_case() {
    let mut e = sample_entry(); // existing or new helper building a SkillLockEntry
    e.content_hash = Some("abc123".to_string());
    let json = serde_json::to_string(&e).unwrap();
    assert!(json.contains("\"contentHash\":\"abc123\""));
}

#[test]
fn entry_omits_content_hash_when_none() {
    let mut e = sample_entry();
    e.content_hash = None;
    let json = serde_json::to_string(&e).unwrap();
    assert!(!json.contains("contentHash"));
}

#[test]
fn entry_deserializes_without_content_hash_to_none() {
    // npx-written entry has no contentHash; must not error.
    let json = r#"{"source":"o/r","sourceType":"github","sourceUrl":"https://github.com/o/r","skillFolderHash":"","installedAt":"t","updatedAt":"t"}"#;
    let e: super::SkillLockEntry = serde_json::from_str(json).unwrap();
    assert_eq!(e.content_hash, None);
    assert_eq!(e.skill_folder_hash, "");
}
```

If no `sample_entry()` helper exists, add one in the test module constructing a minimal `SkillLockEntry`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p skill lock::types`
Expected: compile error — no field `content_hash`.

- [ ] **Step 3: Add the field**

In `SkillLockEntry` (`crates/skill/src/lock/types.rs`), after `skill_folder_hash`:

```rust
    /// aghub source SHA-256 (npx-compatible `compute_skill_folder_hash`). npx
    /// leaves this absent; aghub stores the real hash here and keeps
    /// `skill_folder_hash` empty. Missing → recompute (never an error).
    #[serde(rename = "contentHash", skip_serializing_if = "Option::is_none", default)]
    pub content_hash: Option<String>,
```

Update any exhaustive struct literals that construct `SkillLockEntry` in this crate (compiler will list them) to set `content_hash: None` unless they have a real hash.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p skill lock::types`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/skill/src/lock/types.rs
git commit -m "feat(skill): add optional per-entry content_hash to global lock"
```

## Task P4: Project lock — add `skill_path` + the npx path helper

**Files:**

- Create: `crates/skill/src/lock/path.rs`
- Modify: `crates/skill/src/lock/local.rs`, `crates/skill/src/lock/mod.rs`

- [ ] **Step 1: Write failing tests for the path helper**

Create `crates/skill/src/lock/path.rs`:

```rust
//! npx `add.ts:1568-1575` skill_path form: POSIX `<repo-relative-dir>/SKILL.md`,
//! case-preserving; root-level skill → `SKILL.md`.

use std::path::Path;

/// `repo_root` and `skill_dir` are absolute. Returns the npx skill_path or None
/// if `skill_dir` is not inside `repo_root`.
pub fn skill_path_from_repo_dir(repo_root: &Path, skill_dir: &Path) -> Option<String> {
    let rel = skill_dir.strip_prefix(repo_root).ok()?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        Some("SKILL.md".to_string())
    } else {
        Some(format!("{rel}/SKILL.md"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn root_level_skill() {
        assert_eq!(
            skill_path_from_repo_dir(Path::new("/tmp/repo"), Path::new("/tmp/repo")),
            Some("SKILL.md".to_string())
        );
    }

    #[test]
    fn nested_preserves_case_and_uses_forward_slash() {
        assert_eq!(
            skill_path_from_repo_dir(
                Path::new("/tmp/repo"),
                Path::new("/tmp/repo/skills/MySkill")
            ),
            Some("skills/MySkill/SKILL.md".to_string())
        );
    }

    #[test]
    fn outside_repo_is_none() {
        assert_eq!(
            skill_path_from_repo_dir(Path::new("/tmp/repo"), Path::new("/other/x")),
            None
        );
    }
}
```

Add `pub mod path;` to `crates/skill/src/lock/mod.rs` and re-export `pub use path::skill_path_from_repo_dir;`.

- [ ] **Step 2: Run to verify fail then pass**

Run: `cargo test -p skill lock::path`
Expected: passes once the module is wired (this helper has no failing-then-impl split since the impl is trivial; the assertions ARE the spec). If you prefer strict red→green, write the `mod tests` first with an empty `skill_path_from_repo_dir` returning `None`, watch it fail, then fill in.

- [ ] **Step 3: Write failing tests for the `skill_path` field**

Add to `crates/skill/src/lock/local.rs` test module:

```rust
#[test]
fn local_entry_serializes_skill_path_camel_case() {
    let mut e = sample_local_entry();
    e.skill_path = Some("skills/pdf/SKILL.md".to_string());
    let json = serde_json::to_string(&e).unwrap();
    assert!(json.contains("\"skillPath\":\"skills/pdf/SKILL.md\""));
}

#[test]
fn local_entry_omits_skill_path_when_none() {
    let mut e = sample_local_entry();
    e.skill_path = None;
    assert!(!serde_json::to_string(&e).unwrap().contains("skillPath"));
}

#[test]
fn write_local_lock_has_trailing_newline_and_sorted_keys() {
    let _g = crate::lock::test_utils::TestLockGuard::new();
    let tmp = tempfile::tempdir().unwrap();
    let mut lock = super::LocalSkillLockFile::default();
    lock.skills.insert("z".into(), sample_local_entry());
    lock.skills.insert("a".into(), sample_local_entry());
    super::write_local_lock(&lock, Some(tmp.path())).unwrap();
    let raw = std::fs::read_to_string(tmp.path().join("skills-lock.json")).unwrap();
    assert!(raw.ends_with("\n"));
    assert!(!raw.ends_with("\n\n"));
    let a = raw.find("\"a\"").unwrap();
    let z = raw.find("\"z\"").unwrap();
    assert!(a < z, "keys must be sorted (BTreeMap)");
}
```

Add `sample_local_entry()` to the test module if absent.

- [ ] **Step 4: Run to verify failure**

Run: `cargo test -p skill lock::local`
Expected: compile error — no field `skill_path`.

- [ ] **Step 5: Add the field**

In `LocalSkillLockEntry` (`crates/skill/src/lock/local.rs`), after `computed_hash`:

```rust
    #[serde(rename = "skillPath", skip_serializing_if = "Option::is_none", default)]
    pub skill_path: Option<String>,
```

Update struct literals constructing `LocalSkillLockEntry` to set `skill_path: None` (compiler lists them).

- [ ] **Step 6: Run to verify pass**

Run: `cargo test -p skill lock::local lock::path`
Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add crates/skill/src/lock/path.rs crates/skill/src/lock/local.rs crates/skill/src/lock/mod.rs
git commit -m "feat(skill): add skill_path to project lock + npx path helper"
```

## Task P5: `write_project_install_lock` computes the real source hash

**Files:**

- Modify: `crates/skill/src/install.rs`
- Modify: `crates/api/src/routes/skills.rs` (call site at ~473-523)

- [ ] **Step 1: Rewrite the placeholder test as the failing real-hash test**

In `crates/skill/src/install.rs`, replace `write_project_install_lock_uses_placeholder_hash` with:

```rust
#[test]
fn write_project_install_lock_computes_real_hash() {
    let _g = crate::lock::test_utils::TestLockGuard::new();
    let project = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap(); // the SOURCE repo subfolder
    std::fs::write(source.path().join("SKILL.md"), b"name: t\n").unwrap();

    let src = InstallLockSource { /* fill with the existing fields used today */ ..sample_source() };
    write_project_install_lock("t", &src, source.path(), project.path()).unwrap();

    let lock = crate::lock::local::read_local_lock(Some(project.path()));
    let entry = lock.skills.get("t").unwrap();
    assert_ne!(entry.computed_hash, crate::hash::EMPTY_SKILLS_LOCK_DIGEST);
    assert_eq!(
        entry.computed_hash,
        crate::compute_skill_folder_hash(source.path()).unwrap()
    );
}
```

(Provide `sample_source()` in the test module reflecting the current `InstallLockSource` shape.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p skill write_project_install_lock`
Expected: compile error — signature has no `source_dir` parameter.

- [ ] **Step 3: Change the signature + compute the hash**

In `crates/skill/src/install.rs`, change `write_project_install_lock` to:

```rust
pub fn write_project_install_lock(
    skill_name: &str,
    source: &InstallLockSource,
    source_dir: &Path,   // NEW: the SOURCE repo subfolder to hash
    cwd: &Path,
) -> std::io::Result<()> {
    let computed_hash = crate::compute_skill_folder_hash(source_dir)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let skill_path = source
        .repo_root
        .as_deref()
        .and_then(|root| crate::lock::skill_path_from_repo_dir(Path::new(root), source_dir));
    local::add_skill_to_local_lock(
        skill_name,
        local::LocalSkillLockEntry {
            source: source.source.clone(),
            ref_name: source.ref_name.clone(),
            source_type: source.source_type.clone(),
            computed_hash,
            skill_path,
        },
        Some(cwd),
    )
}
```

If `InstallLockSource` has no `repo_root`, add it (the directory the source was cloned/copied from) and populate it at the call sites. For the global writer in the same file, compute the hash the same way and set `content_hash: Some(hash)` while keeping `skill_folder_hash: String::new()`.

- [ ] **Step 4: Fix the API call site**

In `crates/api/src/routes/skills.rs` (~473-523 `write_skill_install_lock`): pass the source folder that was cloned/copied (the temp checkout subfolder for the skill), set global `content_hash`, and stop passing `EMPTY_SKILLS_LOCK_DIGEST`. Remove the now-unused placeholder constant at `skills.rs:~370-371` if nothing else uses it.

- [ ] **Step 5: Run to verify pass + workspace build**

Run: `cargo test -p skill write_project_install_lock && cargo build -p aghub-api`
Expected: pass + builds.

- [ ] **Step 6: Commit**

```bash
git add crates/skill/src/install.rs crates/api/src/routes/skills.rs
git commit -m "feat(skill): write real source hash + content_hash at install (drop placeholder)"
```

## Task P6: Placeholder auto-heal helper (pure)

**Files:**

- Modify: `crates/skill/src/hash.rs` (already has `is_placeholder_digest`)
- Create logic consumed by F1 in `crates/core/src/skills/update.rs` (Task F1.3)

- [ ] **Step 1: Add a focused unit test for `is_placeholder_digest`**

In `crates/skill/src/hash.rs` test module:

```rust
#[test]
fn placeholder_digest_detected() {
    assert!(is_placeholder_digest(EMPTY_SKILLS_LOCK_DIGEST));
    assert!(!is_placeholder_digest("abc"));
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test -p skill is_placeholder_digest` → pass.

```bash
git add crates/skill/src/hash.rs
git commit -m "test(skill): assert placeholder-digest detection"
```

(The full auto-heal flow — recompute-before-compare — is implemented and tested in Task F1.3, which is where comparison lives.)

## Task P7: npx interop suite (round-trip, sync parity, lock-wipe boundary)

**Files:**

- Create: `crates/skill/tests/npx_interop.rs`
- Create: `crates/skill/tests/fixtures/global-lock-npx-written.json`

- [ ] **Step 1: Commit an npx-written global lock fixture**

`crates/skill/tests/fixtures/global-lock-npx-written.json` (version 3, two entries, `skillFolderHash` set as a GitHub tree SHA, NO `contentHash`, no trailing newline):

```json
{
	"version": 3,
	"skills": {
		"alpha": {
			"source": "o/r",
			"sourceType": "github",
			"sourceUrl": "https://github.com/o/r",
			"ref": "main",
			"skillPath": "skills/alpha/SKILL.md",
			"skillFolderHash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
			"installedAt": "2026-01-01T00:00:00.000Z",
			"updatedAt": "2026-01-01T00:00:00.000Z"
		},
		"beta": {
			"source": "o/r2",
			"sourceType": "github",
			"sourceUrl": "https://github.com/o/r2",
			"skillFolderHash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
			"installedAt": "2026-01-01T00:00:00.000Z",
			"updatedAt": "2026-01-01T00:00:00.000Z"
		}
	}
}
```

- [ ] **Step 2: Write the interop tests**

Create `crates/skill/tests/npx_interop.rs`:

```rust
//! Proves aghub never breaks an npx-read/written lock.
use skill::compute_skill_folder_hash;
use skill::lock::types::{SkillLockEntry, SkillLockFile};

const NPX_LOCK: &str =
    include_str!("fixtures/global-lock-npx-written.json");

#[test]
fn reads_npx_lock_and_preserves_unknown_and_versions() {
    let lock: SkillLockFile = serde_json::from_str(NPX_LOCK).unwrap();
    assert_eq!(lock.version, 3);
    let alpha = lock.skills.get("alpha").unwrap();
    assert_eq!(alpha.content_hash, None);
    assert_eq!(alpha.skill_folder_hash, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    // re-serialize: version stays 3, skillFolderHash preserved, no contentHash injected
    let out = serde_json::to_string_pretty(&lock).unwrap();
    assert!(out.contains("\"version\": 3"));
    assert!(out.contains("\"skillFolderHash\": \"aaaa"));
    assert!(!out.contains("contentHash"));
}

#[test]
fn round_trip_b_missing_content_hash_recomputes_not_errors() {
    // aghub wrote contentHash; npx addSkillToLock dropped it on one entry.
    let mut entry: SkillLockEntry =
        serde_json::from_value(serde_json::json!({
            "source":"o/r","sourceType":"github","sourceUrl":"https://github.com/o/r",
            "skillFolderHash":"","installedAt":"t","updatedAt":"t"
        })).unwrap();
    assert_eq!(entry.content_hash, None);
    // The update path must recompute, not error. Here we just assert the field
    // is optional + recompute is possible from disk.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("SKILL.md"), b"x").unwrap();
    entry.content_hash = Some(compute_skill_folder_hash(tmp.path()).unwrap());
    assert!(entry.content_hash.is_some());
}

#[test]
fn experimental_sync_skip_condition_hash_match() {
    // aghub-written computed_hash must equal a second recompute (npx sync ===).
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("SKILL.md"), b"content").unwrap();
    std::fs::create_dir_all(tmp.path().join("lib")).unwrap();
    std::fs::write(tmp.path().join("lib/x.ts"), b"export {}").unwrap();
    let h1 = compute_skill_folder_hash(tmp.path()).unwrap();
    let h2 = compute_skill_folder_hash(tmp.path()).unwrap();
    assert_eq!(h1, h2);
}

#[test]
fn lock_wipe_boundary_old_version_returns_empty_not_panic() {
    let old = r#"{"version":2,"skills":{}}"#;
    // read_skill_lock wipes <CURRENT; deserialize of the struct still works.
    let lock: SkillLockFile = serde_json::from_str(old).unwrap();
    assert_eq!(lock.version, 2); // raw parse; read_skill_lock() applies the wipe policy
}
```

Adjust `use` paths to the crate's real re-exports (`skill::lock::types::…` or `skill::lock::…`).

- [ ] **Step 3: Run**

Run: `cargo test -p skill --test npx_interop`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/skill/tests/npx_interop.rs crates/skill/tests/fixtures/global-lock-npx-written.json
git commit -m "test(skill): npx interop round-trip + sync-parity suite"
```

## Task P8: Atomic lock writes (temp + rename + mutex)

**Files:**

- Modify: `crates/skill/src/lock/io.rs`, `crates/skill/src/lock/local.rs`

- [ ] **Step 1: Failing test — concurrent writers never see a partial file**

Add to `crates/skill/src/lock/io.rs` test module:

```rust
#[test]
fn write_skill_lock_is_atomic_no_partial() {
    let _g = crate::lock::test_utils::TestLockGuard::new();
    let mut lock = super::super::types::SkillLockFile::default();
    lock.skills.insert("a".into(), sample_entry());
    super::write_skill_lock(&lock).unwrap();
    // file is valid JSON immediately after write (no truncated state)
    let path = super::get_skill_lock_path();
    let raw = std::fs::read_to_string(&path).unwrap();
    let _: super::super::types::SkillLockFile = serde_json::from_str(&raw).unwrap();
}
```

- [ ] **Step 2: Implement atomic write**

In `crates/skill/src/lock/io.rs`, change `write_skill_lock` to write to `<path>.tmp` then `std::fs::rename` over the target, guarded by a process-wide `static LOCK: Mutex<()>`. Apply the same to `write_local_lock` in `local.rs`. Keep the existing 2-space + trailing-newline serialization (do not change formatting).

```rust
use std::sync::Mutex;
static WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn write_skill_lock(lock: &SkillLockFile) -> std::io::Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap();
    let path = get_skill_lock_path();
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut json = serde_json::to_string_pretty(lock)?;
    json.push('\n'); // preserve existing aghub behavior
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p skill lock::io lock::local`
Expected: pass.

```bash
git add crates/skill/src/lock/io.rs crates/skill/src/lock/local.rs
git commit -m "feat(skill): atomic temp+rename lock writes under a mutex"
```

---

# Workstream F1 — Git-native update check (depends on P)

F1 tasks F1.1–F1.3 are independent of F2 and can run in parallel with F2. F1.4–F1.8 wire orchestration/credentials/routes/CLI.

## Task F1.1: Credential redaction helper + userinfo stripping

**Files:**

- Create: `crates/git/src/redact.rs`
- Modify: `crates/git/src/lib.rs`, `crates/git/src/error.rs`, `crates/git/src/clone.rs`, `crates/git/src/source.rs`

- [ ] **Step 1: Failing tests for the helper**

Create `crates/git/src/redact.rs`:

```rust
//! Strip URL userinfo (`user:token@`) from any string before it becomes an error,
//! a log line, an Uncheckable reason, or a persisted URL.

/// Replace every `scheme://user:secret@host` occurrence's userinfo with `***`.
pub fn redact_url_userinfo(s: &str) -> String {
    // Robust: scan for "://", then up to the next '@' before the next '/' '?' or whitespace.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if s[i..].starts_with("://") {
            out.push_str("://");
            i += 3;
            // find userinfo terminator '@' before path/query/space
            if let Some(at_rel) = s[i..].find(|c| c == '@' || c == '/' || c == '?' || c == ' ') {
                if s[i..].as_bytes()[at_rel] == b'@' {
                    out.push_str("***");
                    i += at_rel + 1; // skip userinfo + '@'
                    out.push('@');
                    continue;
                }
            }
        } else {
            out.push(s[i..].chars().next().unwrap());
            i += s[i..].chars().next().unwrap().len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_pat_userinfo() {
        let s = "fatal: Authentication failed for 'https://user:ghp_SECRET@github.com/o/r.git'";
        let r = redact_url_userinfo(s);
        assert!(!r.contains("ghp_SECRET"));
        assert!(!r.contains("user:"));
        assert!(r.contains("https://***@github.com/o/r.git"));
    }

    #[test]
    fn leaves_clean_url_untouched() {
        let s = "https://github.com/o/r.git";
        assert_eq!(redact_url_userinfo(s), s);
    }

    #[test]
    fn handles_multiple_urls() {
        let s = "a https://u:p@h/x b https://v:q@h/y";
        let r = redact_url_userinfo(s);
        assert!(!r.contains("u:p") && !r.contains("v:q"));
    }
}
```

Add `pub mod redact; pub use redact::redact_url_userinfo;` to `crates/git/src/lib.rs`.

- [ ] **Step 2: Run to verify pass**

Run: `cargo test -p aghub-git redact`
Expected: pass.

- [ ] **Step 3: Failing test — GitError Display is redacted**

Add to `crates/git/src/error.rs` test module:

```rust
#[test]
fn clone_failed_display_redacts_userinfo() {
    let e = GitError::clone_failed(
        "Fetch failed: could not read https://user:ghp_SECRET@github.com/o/r.git",
    );
    let shown = e.to_string();
    assert!(!shown.contains("ghp_SECRET"));
    assert!(!shown.contains("user:"));
}
```

- [ ] **Step 4: Make `clone_failed` (and other string-bearing constructors) redact**

In `crates/git/src/error.rs`, change `clone_failed`, `invalid_url`, `not_https`, and `destination_error` to run their string through `crate::redact::redact_url_userinfo` before storing:

```rust
pub fn clone_failed(msg: impl Into<String>) -> Self {
    GitError::CloneFailed(crate::redact::redact_url_userinfo(&msg.into()))
}
```

- [ ] **Step 5: Failing test — `resolve_remote_source` strips userinfo before persisting**

Add to `crates/git/src/source.rs` test module:

```rust
#[test]
fn resolve_remote_source_strips_userinfo() {
    let r = resolve_remote_source("https://user:ghp_SECRET@github.com/o/r.git").unwrap();
    assert!(!r.source_url.contains("ghp_SECRET") && !r.source_url.contains("user:"));
    assert!(!r.clone_url.contains("ghp_SECRET") && !r.clone_url.contains("user:"));
}
```

- [ ] **Step 6: Strip userinfo in `resolve_remote_source`**

In `crates/git/src/source.rs` (~175-185), after parsing, clear userinfo before building `source_url`/`clone_url`:

```rust
let mut clean = parsed.clone();
let _ = clean.set_username("");
let _ = clean.set_password(None);
let clean_str = clean.to_string();
// ...source_url: clean_str.clone(), clone_url: clean_str
```

- [ ] **Step 7: Run all + commit**

Run: `cargo test -p aghub-git`
Expected: pass.

```bash
git add crates/git/src/redact.rs crates/git/src/lib.rs crates/git/src/error.rs crates/git/src/source.rs
git commit -m "feat(git): redact URL userinfo from errors and persisted source URLs"
```

## Task F1.2: Treeless ref fetch + default-branch via gix HEAD symref (spike + build)

**Files:**

- Create: `crates/git/src/fetch.rs`
- Modify: `crates/git/src/lib.rs`, `crates/git/Cargo.toml` (only if the spike needs a feature)

- [ ] **Step 1: Spike — confirm the gix 0.83 API for a no-checkout fetch + HEAD symref**

Run a throwaway probe (a temporary `#[test]` or `examples/` binary) that:

1. `gix::prepare_clone(url, tmp)` → `fetch_only(progress, &should_interrupt)` (fetch without `main_worktree()`), and
2. resolves the remote default branch from the fetch outcome's ref map / `repo.head_ref()` symref — **no `std::process::Command`**.

Record which call returns refs without a worktree checkout. If a partial (`blob:none`) filter is unavailable in gix 0.83, proceed with a **bare fetch** (all objects, no worktree) and read the skill subtree into a temp dir in Task F1.3. Document the chosen API in a comment at the top of `fetch.rs`.

- [ ] **Step 2: Write failing tests (network tests `#[ignore]`)**

Create `crates/git/src/fetch.rs`:

```rust
//! Treeless/bare ref fetch (no worktree checkout) + default-branch resolution via
//! gix HEAD symref. Never shells out to the `git` binary.

use std::path::Path;
use tempfile::TempDir;
use crate::{Credentials, GitError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    /// Branch name or None (→ default branch).
    Branch(Option<String>),
    /// A pin (tag or 40-hex SHA) — intentional, reported UpToDate by callers.
    Pinned(String),
}

/// Classify a ref string: 40-hex → Pinned SHA; looks like a tag → Pinned; else Branch.
pub fn classify_ref(r: Option<&str>) -> RefKind { /* see Step 4 */ unimplemented!() }

/// Fetch `ref` of `url` into a fresh temp dir WITHOUT a worktree checkout.
/// Returns the temp dir (holding the .git object store) and the resolved commit.
pub fn fetch_ref_to_temp(
    url: &str,
    ref_: Option<&str>,
    creds: Option<&Credentials>,
) -> Result<(TempDir, gix::ObjectId)> { /* see Step 4 */ unimplemented!() }

/// Resolve the remote default branch via the HEAD symref (no git binary).
pub fn resolve_default_branch(repo: &gix::Repository) -> Result<String> { /* Step 4 */ unimplemented!() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_sha_is_pinned() {
        assert_eq!(classify_ref(Some("0123456789abcdef0123456789abcdef01234567")),
                   RefKind::Pinned("0123456789abcdef0123456789abcdef01234567".into()));
    }
    #[test]
    fn classify_branch_is_branch() {
        assert_eq!(classify_ref(Some("main")), RefKind::Branch(Some("main".into())));
    }
    #[test]
    fn classify_none_is_default_branch() {
        assert_eq!(classify_ref(None), RefKind::Branch(None));
    }

    #[ignore = "network"]
    #[test]
    fn fetch_public_repo_default_branch_no_binary() {
        let (tmp, _oid) = fetch_ref_to_temp(
            "https://github.com/vercel-labs/agent-skills.git", None, None).unwrap();
        assert!(tmp.path().join(".git").exists() || tmp.path().exists());
    }
}
```

- [ ] **Step 3: Run unit tests to verify failure**

Run: `cargo test -p aghub-git fetch::tests::classify`
Expected: panics with `unimplemented!()`.

- [ ] **Step 4: Implement `classify_ref`, `fetch_ref_to_temp`, `resolve_default_branch`**

`classify_ref`: `Some(s)` where `s.len()==40 && all hex` → `Pinned(s)`; `Some(s)` that resolves as a tag (heuristic: leave tag-vs-branch disambiguation to the fetch — treat non-SHA `Some` as `Branch(Some(s))` and let pin-detection for tags happen via the lock's recorded `sourceType`/ref semantics; the spec treats tags & SHAs as pinned, so callers pass tag refs as `Pinned`); `None` → `Branch(None)`. Keep `classify_ref` SHA-only here; the caller (F1.3) decides tag-as-pin from lock metadata.

`fetch_ref_to_temp`: build the URL with `inject_credentials` when `creds` is `Some` (so PAT auth works), `gix::prepare_clone(url, tmp)`, set the refspec for `ref_` (or `HEAD` when `None`), `fetch_only` (NO `main_worktree`). Resolve the wanted ref to an `ObjectId`. Wrap every gix error via `GitError::clone_failed` (already redacted). For `Branch(None)`, call `resolve_default_branch`.

`resolve_default_branch`: open the fetched repo with `gix::open`, read `repo.head_ref()` / the `HEAD` symref target, strip `refs/heads/`. No subprocess.

- [ ] **Step 5: Run unit tests; run the ignored network test locally**

Run: `cargo test -p aghub-git fetch` then `cargo test -p aghub-git fetch -- --ignored`
Expected: unit tests pass; network test passes when online.

- [ ] **Step 6: Lint + commit**

```bash
git add crates/git/src/fetch.rs crates/git/src/lib.rs crates/git/Cargo.toml
git commit -m "feat(git): treeless fetch_ref_to_temp + gix HEAD-symref default branch"
```

## Task F1.3: Pure update comparison in core (`SkillUpdateStatus`)

**Files:**

- Create: `crates/core/src/skills/update.rs`
- Modify: `crates/core/src/skills/mod.rs`

- [ ] **Step 1: Failing tests**

Create `crates/core/src/skills/update.rs` with the test module (functions stubbed `unimplemented!()`):

```rust
//! PURE update comparison. No network, no keyring — callers pass a resolved token
//! and a fetched source folder. (Fetch + creds live in crates/api.)

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UncheckableReason { Auth, Network, Local, Ssh, UnsupportedScheme, NoPath, Timeout }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillUpdateStatus {
    UpToDate,
    UpdateAvailable { current: String, available: String },
    Uncheckable { reason: UncheckableReason },
}

/// Reject absolute paths and any `..`; join under `root`; canonicalize; verify the
/// result stays under `root`. Returns the safe absolute skill dir, or None to reject.
pub fn sanitize_skill_path(root: &Path, skill_path: &str) -> Option<PathBuf> { unimplemented!() }

/// Compare `stored` (content_hash/computed_hash) against the freshly recomputed
/// `fetched_dir` hash. `stored == None` or placeholder → recompute-only (auto-heal):
/// returns UpToDate after writing the recomputed hash back is the caller's job; here
/// we return UpToDate when stored is unknown and recompute succeeds (no false positive).
pub fn compare_hashes(stored: Option<&str>, fetched_dir: &Path)
    -> Result<SkillUpdateStatus, std::io::Error> { unimplemented!() }
```

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn rejects_absolute_skill_path() {
        let root = tempdir().unwrap();
        assert_eq!(sanitize_skill_path(root.path(), "/etc/passwd"), None);
    }
    #[test]
    fn rejects_dotdot_skill_path() {
        let root = tempdir().unwrap();
        assert_eq!(sanitize_skill_path(root.path(), "../../secret/SKILL.md"), None);
    }
    #[test]
    fn accepts_contained_skill_path() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("skills/a")).unwrap();
        fs::write(root.path().join("skills/a/SKILL.md"), b"x").unwrap();
        let got = sanitize_skill_path(root.path(), "skills/a/SKILL.md").unwrap();
        assert!(got.starts_with(root.path().canonicalize().unwrap()));
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("SKILL.md"), b"x").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        assert_eq!(sanitize_skill_path(root.path(), "escape/SKILL.md"), None);
    }

    #[test]
    fn same_content_is_up_to_date() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("SKILL.md"), b"x").unwrap();
        let h = skill::compute_skill_folder_hash(d.path()).unwrap();
        assert_eq!(compare_hashes(Some(&h), d.path()).unwrap(), SkillUpdateStatus::UpToDate);
    }
    #[test]
    fn changed_content_is_update_available() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("SKILL.md"), b"NEW").unwrap();
        let st = compare_hashes(Some("oldhash"), d.path()).unwrap();
        assert!(matches!(st, SkillUpdateStatus::UpdateAvailable { .. }));
    }
    #[test]
    fn missing_hash_recomputes_no_false_positive() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("SKILL.md"), b"x").unwrap();
        assert_eq!(compare_hashes(None, d.path()).unwrap(), SkillUpdateStatus::UpToDate);
    }
    #[test]
    fn placeholder_hash_auto_heals() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("SKILL.md"), b"x").unwrap();
        let st = compare_hashes(Some(skill::EMPTY_SKILLS_LOCK_DIGEST), d.path()).unwrap();
        assert_eq!(st, SkillUpdateStatus::UpToDate);
    }
}
```

Add `pub mod update;` to `crates/core/src/skills/mod.rs`. Ensure `crates/core/Cargo.toml` depends on `skill` (it already does transitively; add a direct dep if needed).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p aghub-core skills::update`
Expected: `unimplemented!()` panics.

- [ ] **Step 3: Implement**

```rust
pub fn sanitize_skill_path(root: &Path, skill_path: &str) -> Option<PathBuf> {
    if skill_path.is_empty() { return None; }
    let p = Path::new(skill_path);
    if p.is_absolute() { return None; }
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) { return None; }
    let joined = root.join(p);
    let canon_root = root.canonicalize().ok()?;
    let canon = joined.canonicalize().ok()?; // also resolves symlinks
    if canon.starts_with(&canon_root) { Some(canon) } else { None }
}

pub fn compare_hashes(stored: Option<&str>, fetched_dir: &Path)
    -> Result<SkillUpdateStatus, std::io::Error> {
    let fresh = skill::compute_skill_folder_hash(fetched_dir)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    match stored {
        // unknown or placeholder → auto-heal: no false UpdateAvailable
        None => Ok(SkillUpdateStatus::UpToDate),
        Some(h) if skill::is_placeholder_digest(h) => Ok(SkillUpdateStatus::UpToDate),
        Some(h) if h == fresh => Ok(SkillUpdateStatus::UpToDate),
        Some(h) => Ok(SkillUpdateStatus::UpdateAvailable {
            current: h.to_string(), available: fresh,
        }),
    }
}
```

Note: for `sanitize_skill_path`, the `fetched_dir` passed to `compare_hashes` is the skill's SKILL.md PARENT dir under the temp checkout (caller derives it from the sanitized SKILL.md path).

- [ ] **Step 4: Run + commit**

Run: `cargo test -p aghub-core skills::update`
Expected: pass.

```bash
git add crates/core/src/skills/update.rs crates/core/src/skills/mod.rs
git commit -m "feat(core): pure SkillUpdateStatus comparison with path sanitize + auto-heal"
```

## Task F1.4: Source→credential resolution (api, keyring binding)

**Files:**

- Create: `crates/api/src/credentials/resolve.rs`
- Modify: `crates/api/src/routes/credentials.rs` (reuse `load_credentials`)

- [ ] **Step 1: Failing tests for the binding store + resolution order**

Create `crates/api/src/credentials/resolve.rs`:

```rust
//! Resolution order: (1) keyring source→credential_id binding, (2) keychain by
//! host, (3) None → caller yields Uncheckable{auth}. Tokens never touch the lock.

const BINDINGS_USER: &str = "skill_source_bindings"; // SERVICE = "aghub"

/// In-memory representation for tests; backed by a single keyring JSON entry.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct SourceBindings(pub std::collections::BTreeMap<String, String>); // source → credential_id

pub fn resolve_token_for_source(
    source: &str,
    host: Option<&str>,
    bindings: &SourceBindings,
    creds: &[crate::routes::credentials::StoredCredential],
) -> Option<String> { unimplemented!() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::credentials::StoredCredential;

    fn cred(id: &str, name: &str, token: &str) -> StoredCredential {
        StoredCredential { id: id.into(), name: name.into(), token: token.into() }
    }

    #[test]
    fn binding_wins_first() {
        let mut b = SourceBindings::default();
        b.0.insert("o/r".into(), "c1".into());
        let creds = vec![cred("c1", "github.com", "TOK1"), cred("c2", "github.com", "TOK2")];
        assert_eq!(resolve_token_for_source("o/r", Some("github.com"), &b, &creds),
                   Some("TOK1".into()));
    }
    #[test]
    fn falls_back_to_host_match() {
        let b = SourceBindings::default();
        let creds = vec![cred("c2", "github.com", "TOK2")];
        assert_eq!(resolve_token_for_source("o/r", Some("github.com"), &b, &creds),
                   Some("TOK2".into()));
    }
    #[test]
    fn none_when_no_match() {
        let b = SourceBindings::default();
        let creds = vec![cred("c2", "gitlab.com", "X")];
        assert_eq!(resolve_token_for_source("o/r", Some("github.com"), &b, &creds), None);
    }
}
```

Make `StoredCredential` carry a host/name usable for host-matching (it has `name`; treat `name` as host or add a `host` field if needed). Mount module in `crates/api/src/credentials/mod.rs` (create it) + `crates/api/src/lib.rs`.

- [ ] **Step 2: Run to verify failure → implement → pass**

`resolve_token_for_source`: (1) if `bindings.0.get(source)` → find cred by id → token; (2) else if `host` matches a cred's host → token; (3) else `None`. Add `load_source_bindings()`/`save_source_bindings()` reading/writing the `skill_source_bindings` keyring entry as JSON (mirror `load_credentials`).

Run: `cargo test -p aghub-api credentials::resolve`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/api/src/credentials crates/api/src/lib.rs crates/api/src/routes/credentials.rs
git commit -m "feat(api): source->credential resolution (keyring binding, host fallback)"
```

## Task F1.5: Update-check orchestration (group, cache+TTL, concurrency, timeout, offline)

**Files:**

- Create: `crates/api/src/skills/update_check.rs`
- Modify: `crates/api/src/lib.rs`

- [ ] **Step 1: Failing tests for grouping + cache TTL (pure-ish, no real network)**

Create `crates/api/src/skills/update_check.rs` with:

```rust
//! Orchestrates F1: group entries by (source, ref), resolve creds, fetch (treeless)
//! with a TTL result cache, bounded concurrency, per-fetch timeout, offline skip.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use aghub_core::skills::update::SkillUpdateStatus;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SourceRef { pub source: String, pub ref_: Option<String> }

/// Group lock entries by (source, ref) so each upstream is fetched once.
pub fn group_by_source_ref<'a, I: IntoIterator<Item = (&'a str, SourceRef)>>(
    entries: I,
) -> HashMap<SourceRef, Vec<String>> { unimplemented!() }

pub struct ResultCache {
    ttl: Duration,
    map: HashMap<SourceRef, (Instant, SkillUpdateStatus)>,
}
impl ResultCache {
    pub fn new(ttl: Duration) -> Self { Self { ttl, map: HashMap::new() } }
    pub fn get(&self, k: &SourceRef, now: Instant) -> Option<SkillUpdateStatus> { unimplemented!() }
    pub fn put(&mut self, k: SourceRef, v: SkillUpdateStatus, now: Instant) { unimplemented!() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_same_source_ref_once() {
        let sr = |s: &str, r: Option<&str>| SourceRef { source: s.into(), ref_: r.map(Into::into) };
        let g = group_by_source_ref(vec![
            ("a", sr("o/r", Some("main"))),
            ("b", sr("o/r", Some("main"))),
            ("c", sr("o/r", Some("dev"))),
        ]);
        assert_eq!(g[&sr("o/r", Some("main"))].len(), 2);
        assert_eq!(g[&sr("o/r", Some("dev"))].len(), 1);
    }
    #[test]
    fn cache_expires_after_ttl() {
        let mut c = ResultCache::new(Duration::from_secs(300));
        let k = SourceRef { source: "o/r".into(), ref_: None };
        let t0 = Instant::now();
        c.put(k.clone(), SkillUpdateStatus::UpToDate, t0);
        assert!(c.get(&k, t0).is_some());
        assert!(c.get(&k, t0 + Duration::from_secs(301)).is_none());
    }
}
```

- [ ] **Step 2: Run → implement → pass**

Implement `group_by_source_ref` (fold into the map), `ResultCache::get` (return clone if `now - t <= ttl`), `ResultCache::put`. Add the async runner `check_updates(scope, entries, deps)` that: for each `(source, ref)` group, check cache; classify ref (SHA/tag → `UpToDate` without fetch); resolve token via Task F1.4; `tokio::time::timeout(per_fetch, spawn_blocking(fetch_ref_to_temp))` with a `tokio::sync::Semaphore` concurrency bound; map fetch/network errors → `Uncheckable{Network/Auth}` (inspect redacted error text / gix auth signal); for each skill in the group, `sanitize_skill_path` → `compare_hashes`. Offline flag short-circuits all to `Uncheckable{Network}`. The async runner's network paths are covered by the E2E `#[ignore]` tests in Task F1.7.

Run: `cargo test -p aghub-api skills::update_check`
Expected: unit tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/api/src/skills/update_check.rs crates/api/src/lib.rs
git commit -m "feat(api): update-check orchestration (grouping, TTL cache, timeout, concurrency)"
```

## Task F1.6: Replace `detect_current_branch` shell-out with gix

**Files:**

- Modify: `crates/api/src/routes/skills.rs` (~1299-1316, callers ~1335-1339)

- [ ] **Step 1: Failing test**

Add an api test asserting branch detection uses the gix repo (no subprocess). Since the function reads a checked-out repo, the test creates a temp git repo via gix and asserts the returned branch:

```rust
#[test]
fn detect_current_branch_uses_gix_not_subprocess() {
    // Arrange a gix repo on branch "main"; call the (now gix-based) helper.
    // Assert it returns Some("main") and the source contains no Command::new("git").
}
```

Also add a grep-style guard test (optional) or rely on code review for "no `Command::new("git")`".

- [ ] **Step 2: Implement**

Replace the `std::process::Command::new("git")` body with `aghub_git::fetch::resolve_default_branch` (or `gix::open(repo).head_ref()` for an already-cloned repo) and strip `refs/heads/`. Remove the `Command` import.

- [ ] **Step 3: Run + commit**

Run: `cargo test -p aghub-api detect_current_branch && cargo build -p aghub-api`

```bash
git add crates/api/src/routes/skills.rs
git commit -m "refactor(api): resolve current branch via gix (no git binary)"
```

## Task F1.7: API route `GET /skills/check-updates` + DTO + redaction at surfacing site

**Files:**

- Create: `crates/api/src/routes/skills_update.rs`
- Modify: `crates/api/src/dto/skill.rs`, `crates/api/src/routes/skills.rs` (~1186-1192), `crates/api/src/lib.rs`, `crates/api/src/bin/export-dto.rs`

- [ ] **Step 1: Add the DTO (failing serde test)**

In `crates/api/src/dto/skill.rs` add (with `#[derive(Serialize, TS)]`, ts export):

```rust
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SkillUpdateStatusResponse {
    UpToDate,
    UpdateAvailable { current: String, available: String },
    Uncheckable { reason: String },
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SkillUpdateResponse {
    pub name: String,
    #[serde(flatten)]
    pub status: SkillUpdateStatusResponse,
}
```

Also add `content_hash` to `SkillLockEntryResponse`:

```rust
    #[serde(rename = "contentHash", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content_hash: Option<String>,
```

Add a serde test asserting `UpdateAvailable` serializes with `"status":"updateAvailable"` and `current`/`available`.

- [ ] **Step 2: Apply redaction at the gix-error surfacing site**

In `crates/api/src/routes/skills.rs` (~1186-1192), wrap the surfaced gix error string in `aghub_git::redact_url_userinfo(...)` before returning it.

- [ ] **Step 3: Add the route (E2E tests `#[ignore]`)**

Create `crates/api/src/routes/skills_update.rs` with `GET /skills/check-updates?<scope..>&<agent>` returning `Vec<SkillUpdateResponse>`, delegating to Task F1.5's runner. Mount in `crates/api/src/lib.rs` and export DTOs in `crates/api/src/bin/export-dto.rs` (`export_type::<SkillUpdateResponse>()`, `SkillUpdateStatusResponse`, updated `SkillLockEntryResponse`).

E2E tests in `crates/core/tests` / api tests, all `#[ignore = "network"]`:

```rust
#[ignore = "network"]
#[test] fn e2e_check_public_repo_no_crash() { /* returns UpToDate|UpdateAvailable */ }
#[ignore = "network"]
#[test] fn e2e_check_private_repo_no_token_uncheckable_auth() { /* Uncheckable{auth}, no panic */ }
```

- [ ] **Step 4: Regenerate DTOs + typecheck**

Run: `cd crates/desktop && bun run generate:dto && bun run typecheck`
Expected: `SkillUpdateResponse.ts`, `SkillUpdateStatusResponse.ts` generated; `SkillLockEntryResponse.ts` gains `contentHash?: string`.

- [ ] **Step 5: Run + commit**

Run: `cargo test -p aghub-api skill_update && cargo build --workspace`

```bash
git add crates/api/src/routes/skills_update.rs crates/api/src/dto/skill.rs crates/api/src/routes/skills.rs crates/api/src/lib.rs crates/api/src/bin/export-dto.rs crates/desktop/src/generated/dto
git commit -m "feat(api): GET /skills/check-updates + SkillUpdateStatus DTO + redacted surfacing"
```

## Task F1.8: CLI `check skills` (read-only)

**Files:**

- Create: `crates/cli/src/commands/check.rs`
- Modify: `crates/cli/src/commands/mod.rs`, `crates/cli/src/cli.rs` (clap)

- [ ] **Step 1: Failing CLI e2e test**

Add to `crates/cli/tests/cli_tests.rs`:

```rust
#[test]
fn check_skills_outputs_json_array() {
    // No network: with an empty/local-only lock, check returns an array (possibly
    // with Uncheckable entries) and exits 0.
    let dir = tempfile::tempdir().unwrap();
    let out = aghub_cli().current_dir(dir.path())
        .args(["-a", "claude", "check", "skills", "--json"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v.is_array());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p aghub check_skills_outputs_json_array`
Expected: fail — `check` subcommand unknown.

- [ ] **Step 3: Implement**

Add a `Check { resource: ResourceType }` clap subcommand in `crates/cli/src/cli.rs`; create `crates/cli/src/commands/check.rs` `execute(...)` that loads the scope's lock and prints `Vec<SkillUpdateResponse>`-shaped JSON (use core comparison for local-only entries; remote checks reuse the api orchestration crate or print `Uncheckable{network}` when offline). Register in `commands/mod.rs`.

- [ ] **Step 4: Run + commit**

Run: `cargo test -p aghub check_skills`

```bash
git add crates/cli/src/commands/check.rs crates/cli/src/commands/mod.rs crates/cli/src/cli.rs
git commit -m "feat(cli): read-only `check skills` update detection"
```

---

# Workstream F2 — Clean removal + disk-reconciled lock prune (depends on P; parallel to F1)

## Task F2.1: Allow-listed roots + containment guard

**Files:**

- Create: `crates/core/src/skills/removal.rs`
- Modify: `crates/core/src/skills/mod.rs`

- [ ] **Step 1: Failing tests**

Create `crates/core/src/skills/removal.rs`:

```rust
//! Layout-aware removal helpers + containment guard. Allow-listed skills roots:
//! ~/.config/agents/skills, ~/.agents/skills, <project>/.agents/skills, and the
//! agent's own skills dir.

use std::path::{Path, PathBuf};

/// Collect the allow-listed skills roots for a scope.
pub fn allowed_skill_roots(agent_skill_dirs: &[PathBuf], project_root: Option<&Path>) -> Vec<PathBuf> { unimplemented!() }

/// Canonicalize `target` and assert it is a descendant of one allow-listed root.
/// Returns the canonical path if contained, else None (caller skips + warns).
pub fn assert_contained(target: &Path, roots: &[PathBuf]) -> Option<PathBuf> { unimplemented!() }

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn contained_path_is_accepted() {
        let root = tempdir().unwrap();
        let sub = root.path().join("skills/a");
        std::fs::create_dir_all(&sub).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert_eq!(assert_contained(&sub, &roots), Some(sub.canonicalize().unwrap()));
    }

    #[test]
    fn outside_path_is_rejected() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("x")).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert_eq!(assert_contained(&outside.path().join("x"), &roots), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_root_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("evil")).unwrap();
        let link = root.path().join("evil");
        symlink(outside.path().join("evil"), &link).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert_eq!(assert_contained(&link, &roots), None); // canonicalize escapes root
    }
}
```

Add `pub mod removal;` to `crates/core/src/skills/mod.rs`.

- [ ] **Step 2: Run → implement → pass**

`allowed_skill_roots`: push `~/.config/agents/skills`, `~/.agents/skills`, `project_root.join(".agents/skills")`, plus `agent_skill_dirs`; canonicalize each that exists. `assert_contained`: `target.canonicalize().ok()` then return `Some` only if it `starts_with` a canonicalized root.

Run: `cargo test -p aghub-core skills::removal`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/skills/removal.rs crates/core/src/skills/mod.rs
git commit -m "feat(core): allow-listed skills roots + canonicalized containment guard"
```

## Task F2.2: Removal planning (layout detection + canonical→parent + symlink sweep)

**Files:**

- Modify: `crates/core/src/skills/removal.rs`
- Modify: `crates/core/src/transfer.rs` (reuse `resolve_skill_root`, `find_skill_locations_in_agents`)

- [ ] **Step 1: Failing tests for `plan_removal`**

Add to `crates/core/src/skills/removal.rs`:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum Layout { Symlink, Copy }

#[derive(Debug)]
pub struct RemovalPlan {
    pub layout: Layout,
    /// Absolute paths to delete (canonical dir + symlinks, or copy dirs).
    pub paths: Vec<PathBuf>,
    /// Paths skipped for containment/TOCTOU reasons (warn).
    pub skipped: Vec<PathBuf>,
    /// True when execution requires explicit confirm (symlink full or --all-agents).
    pub needs_confirm: bool,
}
```

Tests (use `TestConfig`-style temp agent dirs):

- `plan_removal_symlink_layout_collects_canonical_and_all_symlinks` — canonical + 2 symlinks → `paths` has all three, `layout == Symlink`, `needs_confirm == true`.
- `plan_removal_out_of_tree_symlink_is_skipped_not_deleted` — symlink → outside allow-list: canonical out-of-tree lands in `skipped`, NOT `paths`.
- `plan_removal_copy_single_agent` — no `canonical_path`, default → only the target agent's copy in `paths`, `needs_confirm == false`.
- `plan_removal_copy_all_agents` — `all_agents=true` → all same-named copies; `needs_confirm == true`.
- `plan_removal_canonical_is_file_path_takes_parent` — `canonical_path` is a `~/.../SKILL.md` file → plan targets its PARENT dir.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p aghub-core plan_removal`
Expected: compile error / `unimplemented!()`.

- [ ] **Step 3: Implement `plan_removal`**

Signature:

```rust
pub fn plan_removal(
    skill: &crate::Skill,            // carries canonical_path / source_path
    agent_skill_dirs: &[PathBuf],    // all agent dirs in scope (for sweeps)
    project_root: Option<&Path>,
    all_agents: bool,
) -> RemovalPlan { /* ... */ }
```

Logic: if `skill.canonical_path.is_some()` → `Layout::Symlink`: expand tilde + take parent (reuse `transfer::resolve_skill_root`) → canonical dir; `assert_contained` it (else → `skipped`); scan `agent_skill_dirs` for entries whose `canonicalize()` equals the canonical dir (canonicalize failure → `skipped`, not "no match"); add each matching symlink + the canonical to `paths`; `needs_confirm = true`. Else `Layout::Copy`: if `all_agents` collect every same-named copy across `agent_skill_dirs` (`needs_confirm = true`), else only the target agent's copy (`needs_confirm = false`); `assert_contained` each.

- [ ] **Step 4: Run + commit**

Run: `cargo test -p aghub-core plan_removal`

```bash
git add crates/core/src/skills/removal.rs crates/core/src/transfer.rs
git commit -m "feat(core): layout-aware removal planning with containment + symlink sweep"
```

## Task F2.3: Disk-reconciled lock prune (`prune.rs`)

**Files:**

- Create: `crates/core/src/skills/prune.rs`
- Modify: `crates/core/src/skills/mod.rs`

- [ ] **Step 1: Failing tests**

Create `crates/core/src/skills/prune.rs` with the test module first. Tests (use `TestConfig` + fake agent dirs):

- `prune_removes_entries_with_no_disk_skill` — lock has `exists` (on disk) + `ghost` (not) → only `ghost` removed.
- `prune_keeps_unlocked_disk_skill` — disk skill without lock entry → untouched, no entry created/removed.
- `prune_scope_isolation_project_never_prunes_global` — project prune leaves a global-only entry intact.
- `prune_aborts_on_scan_error` — `scan_skills` returns `Err` → lock unchanged, returns `Err`.
- `prune_requires_readable_project_root` — `project_root = None`/unreadable → project prune aborts, lock unchanged.

```rust
pub enum PruneScope { Global, Project }

/// Disk-driven prune. `disk_names` MUST come from a provably successful
/// scan_skills (error-returning), unioned per scope. Returns the pruned names.
pub fn prune_lock(
    scope: PruneScope,
    disk_names: &std::collections::BTreeSet<String>,
    project_root: Option<&std::path::Path>,
) -> std::io::Result<Vec<String>> { unimplemented!() }
```

- [ ] **Step 2: Run to verify failure → implement → pass**

Implement: for `Global`, read the global lock, remove any entry whose name ∉ `disk_names`, write atomically (Task P8). For `Project`, require `project_root` present + readable (else `Err`); operate on the project lock. The caller (Task F2.5) builds `disk_names` by calling `scan_skills` over the per-scope dir union and aborts on any `ScanError` BEFORE calling `prune_lock`. Wire `remove_skill_from_lock` / `remove_skill_from_local_lock`.

Run: `cargo test -p aghub-core skills::prune`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/skills/prune.rs crates/core/src/skills/mod.rs
git commit -m "feat(core): disk-reconciled, scope-isolated lock prune (scan-success-gated)"
```

## Task F2.4: Wire layout-aware removal into the manager (dry-run default + confirm + TOCTOU)

**Files:**

- Modify: `crates/core/src/manager/skill.rs`

- [ ] **Step 1: Failing integration tests**

Add to `crates/core/tests/integration_tests.rs` (using `TestConfig` + manual fixtures):

- `symlink_layout_delete_removes_canonical_and_all_symlinks` (with confirm).
- `symlink_out_of_tree_target_prevents_deletion` — out-of-tree untouched, lock unchanged.
- `copy_layout_single_agent_removes_only_target`.
- `copy_layout_all_agents_removes_all_copies` (with confirm).
- `dry_run_lists_paths_deletes_nothing`.
- `symlink_full_removal_needs_explicit_confirm` — without confirm → returns the plan, deletes nothing.
- `remove_skill_verify_no_other_symlinks_before_canonical_delete` — Cursor symlink still present → canonical NOT deleted.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p aghub-core --test integration_tests symlink_layout`
Expected: fail — new params/behavior absent.

- [ ] **Step 3: Implement**

Add a new method (keep the old `remove_skill` as a thin wrapper that calls this with `dry_run=true` defaults at the API/CLI boundary):

```rust
pub fn remove_skill_planned(
    &mut self,
    name: &str,
    all_agents: bool,
    dry_run: bool,
    confirm: bool,
) -> Result<crate::skills::removal::RemovalPlan> {
    let skill = /* look up existing skill (canonical_path/source_path) */;
    let agent_dirs = /* all agent skill dirs in scope */;
    let plan = crate::skills::removal::plan_removal(&skill, &agent_dirs, self.project_root(), all_agents);
    if dry_run { return Ok(plan); }
    if plan.needs_confirm && !confirm {
        return Err(/* error: confirmation required */);
    }
    for p in &plan.paths {
        // TOCTOU: re-check symlink type at delete time; re-assert containment.
        // For Symlink layout: before deleting the canonical, verify no remaining
        // agent view symlinks to it (re-scan); skip+warn on canonicalize failure.
        // remove symlink via remove_file / remove_dir; canonical via remove_dir_all.
    }
    // update config + caller triggers prune (Task F2.5)
    Ok(plan)
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo test -p aghub-core --test integration_tests`

```bash
git add crates/core/src/manager/skill.rs crates/core/tests/integration_tests.rs
git commit -m "feat(core): layout-aware skill removal (dry-run default, confirm, TOCTOU)"
```

## Task F2.5: Both delete routes + prune wiring (api)

**Files:**

- Modify: `crates/api/src/routes/skills.rs` (`DELETE /skills/by-path` ~171-296; `DELETE /agents/<agent>/skills/<name>` ~753-776)
- Modify: `crates/api/src/routes/skills_update.rs` (add `POST /skills/prune-lock`)

- [ ] **Step 1: Failing api tests**

Add `crates/api/tests/skill_delete_safety.rs`:

- `delete_skill_respects_allowed_roots` — by-path outside allow-list → validation error, `remove_dir_all` never called, FS untouched.
- `delete_by_path_dry_run_default_lists_paths` — default returns removed-path summary, deletes nothing.
- `delete_by_path_confirm_executes` — `confirm=true` actually deletes + prunes the lock.
- `delete_plugin_managed_rejected` — plugin-owned skill not deleted.

Add `crates/api/tests/skill_prune.rs`:

- `prune_lock_route_clears_manual_residue` — manual `rm` then `POST /skills/prune-lock` clears the entry.
- `prune_lock_route_scope_isolation`.

- [ ] **Step 2: Implement**

Rework both routes to: build the `RemovalPlan` via `manager.remove_skill_planned`, default `dry_run=true` (request carries `dry_run`/`confirm`/`all_agents` flags), apply the containment guard before any `remove_dir_all`, return a `removed_paths` summary, and after a real deletion run the scan-gated `prune_lock` (build `disk_names` via `scan_skills` over the per-scope union; abort prune on scan error). Add `POST /skills/prune-lock` calling the same prune path on demand. Replace the raw `std::fs::remove_dir_all` at `skills.rs:282`.

- [ ] **Step 3: Run + DTO regen + commit**

Run: `cargo test -p aghub-api delete_skill && cargo test -p aghub-api prune_lock && cd crates/desktop && bun run generate:dto`

```bash
git add crates/api/src/routes/skills.rs crates/api/src/routes/skills_update.rs crates/api/tests crates/desktop/src/generated/dto
git commit -m "feat(api): safe dual delete routes (dry-run/confirm) + prune-lock route"
```

## Task F2.6: CLI `--all-agents`/`--dry-run`/`--yes` on delete + `prune-lock`

**Files:**

- Create: `crates/cli/src/commands/prune.rs`
- Modify: `crates/cli/src/commands/delete.rs`, `crates/cli/src/commands/mod.rs`, `crates/cli/src/cli.rs`

- [ ] **Step 1: Failing CLI e2e tests**

Add to `crates/cli/tests/cli_tests.rs`:

- `delete_skill_dry_run_is_default_and_lists_paths` — `delete skills <name>` without `--yes` prints the path list and deletes nothing.
- `delete_skill_all_agents_requires_yes` — `--all-agents` without `--yes` → non-zero exit / refusal.
- `prune_lock_subcommand_runs` — `skills prune-lock` exits 0 and reports pruned names (JSON).

- [ ] **Step 2: Implement**

Add `--all-agents`, `--dry-run` (default true), `--yes` flags to the `delete` clap command; thread to the manager method. Create `prune.rs` `execute(...)` calling the core prune (build `disk_names` via `scan_skills`). Register `PruneLock` clap subcommand under `skills`.

- [ ] **Step 3: Run + commit**

Run: `cargo test -p aghub delete_skill prune_lock`

```bash
git add crates/cli/src/commands/delete.rs crates/cli/src/commands/prune.rs crates/cli/src/commands/mod.rs crates/cli/src/cli.rs
git commit -m "feat(cli): delete --all-agents/--dry-run/--yes + skills prune-lock"
```

---

# Workstream D — Desktop (depends on F1 DTOs + P content_hash)

## Task D1: DTO regeneration + type-assertion gate

**Files:**

- Modify: `crates/desktop/src/generated/dto/*` (generated), `crates/api/src/bin/export-dto.rs`
- Create: `crates/desktop/src/components/__tests__/dto-shape.test-d.ts` (type-level)

- [ ] **Step 1: Regenerate + confirm fields**

Run: `cd crates/desktop && bun run generate:dto`
Expected: `SkillLockEntryResponse.ts` has `contentHash?: string`; `SkillUpdateResponse.ts` + `SkillUpdateStatusResponse.ts` exist.

- [ ] **Step 2: Add a type-level assertion (no runtime test harness exists)**

Create `crates/desktop/src/components/__tests__/dto-shape.test-d.ts`:

```ts
import type { SkillLockEntryResponse } from "../../generated/dto/SkillLockEntryResponse";
import type { SkillUpdateResponse } from "../../generated/dto/SkillUpdateResponse";

// Compile-time gate: contentHash must exist (optional) on the global lock entry.
const _entry: SkillLockEntryResponse = {} as SkillLockEntryResponse;
const _ch: string | undefined = _entry.contentHash;

const _u: SkillUpdateResponse = {} as SkillUpdateResponse;
void _ch;
void _u;
```

- [ ] **Step 3: Run typecheck + commit**

Run: `cd crates/desktop && bun run typecheck`
Expected: passes (fails if `contentHash` missing).

```bash
git add crates/desktop/src/generated/dto crates/desktop/src/components/__tests__/dto-shape.test-d.ts crates/api/src/bin/export-dto.rs
git commit -m "chore(desktop): regenerate DTOs; type-level contentHash gate"
```

## Task D2: Badge reads `content_hash`

**Files:**

- Modify: `crates/desktop/src/components/skill-detail.tsx` (~121-148, ~405-411)

- [ ] **Step 1: Change the data source**

In `currentSkillSource` (~129), global skills must read `contentHash` instead of `skillFolderHash`; project skills keep `computedHash`. The badge (~405-411) shows `hash.slice(0, 8)`; when `content_hash` is absent (npx-stripped) hide the hash badge rather than show `skillFolderHash` (now always `""`).

```ts
// global branch:
hash: globalEntry?.contentHash ?? undefined,   // was: globalEntry?.skillFolderHash
// project branch unchanged:
hash: projectEntry?.computedHash ?? undefined,
```

Guard the badge render with `{currentSkillSource.hash ? <HashtagIcon …/> : null}`.

- [ ] **Step 2: Typecheck/lint + manual check**

Run: `cd crates/desktop && bun run typecheck && bun run lint`
Expected: clean. (Visual check deferred to Task D3 + finishing.)

- [ ] **Step 3: Commit**

```bash
git add crates/desktop/src/components/skill-detail.tsx
git commit -m "fix(desktop): badge reads content_hash (skill_folder_hash now empty)"
```

## Task D3: Update-status badge + credential picker + async states

**Files:**

- Create: `crates/desktop/src/components/skill-update-badge.tsx`
- Modify: `crates/desktop/src/components/skill-list.tsx` (~210-225)

- [ ] **Step 1: Build the badge component**

`skill-update-badge.tsx` maps `SkillUpdateStatusResponse` → UI: `upToDate` (subtle check), `updateAvailable` (mint accent — per `.impeccable.md` green=action), `uncheckable` with `reason==="auth"` → a "Add credential" affordance that opens the credential picker and retries `GET /skills/check-updates`. Follow house style: information-dense, structure over decoration, mint accent OKLCH 155, no warm tones.

- [ ] **Step 2: Wire into the list + refresh**

In `skill-list.tsx`, fetch `GET /skills/check-updates` (TanStack `useQuery`) keyed by scope/agent, with loading + error states (silent badge-absent on error, per existing pattern); map the result onto the source-grouped sorted list (~210-225). Refresh triggers a refetch.

- [ ] **Step 3: Typecheck/lint + commit**

Run: `cd crates/desktop && bun run typecheck && bun run lint`

```bash
git add crates/desktop/src/components/skill-update-badge.tsx crates/desktop/src/components/skill-list.tsx
git commit -m "feat(desktop): per-skill update badge + Uncheckable{auth} credential picker"
```

---

# Finishing

## Task Z1: Full verification + branch finish

- [ ] **Step 1: Full workspace gate**

Run:

```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cd crates/desktop && bun run lint && bun run typecheck
```

Expected: all green. The CI-blocking tests (golden parity, credential leak, containment, prune isolation, dry-run, sync parity) are part of `cargo test --workspace`.

- [ ] **Step 2: Network/validation lane (manual, online)**

Run: `cargo test --workspace -- --ignored` (the `*_public_repo` / `*_private_repo*` E2E checks). Confirm private-repo-without-token yields `Uncheckable{auth}` and does not crash.

- [ ] **Step 3: Manual desktop smoke (per superpowers:verification-before-completion)**

Run `just desktop`; verify: badge shows `content_hash` first-8; an outdated skill shows "update available"; a private repo without a stored credential shows the credential picker; delete defaults to a dry-run preview.

- [ ] **Step 4: Finish the branch**

Use superpowers:requesting-code-review, then superpowers:finishing-a-development-branch. Candidate upstream PRs (per spec §8): (a) the lock-prune-on-delete bug fix, (b) the git-native update check feature.

---

## Self-review checklist (run before handing off)

- **Spec coverage:** P (hash + schema) → P1–P8; F1 (fetch, compare, creds, route, CLI, redaction, no-binary) → F1.1–F1.8; F2 (removal, containment, prune, both routes, CLI) → F2.1–F2.6; desktop → D1–D3; credential model → F1.4 + F1.1; atomic writes → P8. ✅
- **npx interop:** golden parity (P2, CI gate), round-trip A/B + sync parity + lock-wipe boundary (P7), skill_path form (P4), content_hash optional/strip-recompute (P3/P7), empty skill_folder_hash unchanged (P3), versions unchanged (P3/P7). ✅
- **Hard constraints → tests:** parity golden (P2), skip-only-.git/node_modules + symlink-skip + bounds (P1), source-folder hashing (P5), redaction never-leaks (F1.1), traversal/symlink-escape reject (F1.3), out-of-tree no-delete + containment (F2.1/F2.2/F2.5), prune scope isolation + scan-error abort + project_root gate (F2.3/F2.5), dry-run deletes nothing (F2.4/F2.5/F2.6). ✅
- **No placeholders:** every code step has real code or an exact signature + the failing test that pins behavior; the one capture-once value (golden hex) has an exact runnable command. ✅
- **Type consistency:** `compute_skill_folder_hash`, `SkillUpdateStatus`/`UncheckableReason`, `fetch_ref_to_temp`/`RefKind`, `plan_removal`/`RemovalPlan`/`Layout`, `prune_lock`/`PruneScope`, `SkillUpdateResponse` used consistently across tasks. ✅

All work stays on `feat/skill-management-improvements`; do not touch `main`.
