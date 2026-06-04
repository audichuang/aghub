---
name: testing-fs-failures
description: Force filesystem operations to fail deterministically in tests so error, rollback, and cleanup paths get covered without adding fault-injection seams. Use when writing a test that must make an fs op fail (permission denied, ENOTDIR, not-a-symlink), when verifying rollback or partial-failure handling, when a permission-based test must stay safe under root/CI, or for unix-gated filesystem tests in Rust.
---

# Testing filesystem-failure paths

Error/rollback/cleanup code that only runs when an `fs` call fails is usually
untested. You can exercise it with real filesystem state — no `trait`-based
fault-injection seam needed. Pick the technique by what the failing path must
already be.

## Pick a technique

- **The path can be junk** → ENOTDIR trick. Deterministic, root-safe, no `chmod`.
- **The path must first be a valid, discoverable file/symlink, then fail on a
  later mutation** → read-only dir + root probe.

## ENOTDIR trick (preferred — root-safe, no chmod)

Make an ancestor a regular file; any op on `file/child` fails with `NotADirectory`,
deterministically, even as root.

```rust
let root = tempdir().unwrap();
let file = root.path().join("not-a-dir");
std::fs::write(&file, "x").unwrap();
let inaccessible = file.join("subdir"); // ancestor is a file → every op errors
// assert your scanner ABORTS here instead of treating it as "absent"
```

It stands in deterministically for the real-world `EACCES` of an unreadable
parent or a dropped mount.

## Read-only dir + root probe

A `0o555` dir blocks create/remove of entries inside it while still allowing
read + traversal — so a symlink inside stays *detectable* but cannot be
*mutated*. Use when the target must be discovered first (e.g. as a referrer) and
only fail on a later unlink/relink.

```rust
#[cfg(unix)]
fn perms_enforced(under: &Path) -> bool {       // root bypasses perm bits
    use std::os::unix::fs::PermissionsExt;
    let p = under.join(".probe");
    std::fs::create_dir(&p).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o555)).unwrap();
    let blocked = std::fs::write(p.join("x"), b"x").is_err();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_dir_all(&p).ok();
    blocked
}
// in the test:
if !perms_enforced(root) { eprintln!("skip: root"); return; }
let orig = std::fs::metadata(&dir).unwrap().permissions();
std::fs::set_permissions(&dir, Permissions::from_mode(0o555)).unwrap();
let res = thing_under_test();
std::fs::set_permissions(&dir, orig).unwrap();  // RESTORE before asserting
assert!(res.is_err());
```

## Non-negotiables

- [ ] `#[cfg(unix)]` on permission/symlink tests.
- [ ] **Root probe + skip** — CI often runs as root, where `0o555` is ignored.
- [ ] **Restore perms before assertions** — else a failed assert leaks an
      unremovable temp dir.
- [ ] To cover "restore an already-processed item", make a **later** item in the
      deterministic iteration order the one that fails (the earlier items get
      processed first, then rollback must undo them).

## Gotcha: not every "occupied target" is an error

Some APIs return `Ok` with a *conflict* result rather than `Err` when the target
already exists (e.g. a symlink-creator that reports `Conflict` instead of
clobbering). Pre-creating a file there will **not** trip the error path. Read the
API first; only a real `Err` reaches your rollback code.

In this repo: the prune scanner uses the ENOTDIR trick; the universal-rename
rollback tests use the read-only-dir technique (`crates/core/src/manager/skill.rs`).
