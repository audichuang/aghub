---
name: npx-skills-contract
description: The interop contract aghub must preserve to stay round-trip compatible with the upstream npx `skills` package (v1.5.x) — lock-file schemas, the .agents Master + symlink layout, the folder-content hash algorithm, and where source normalization lives. Use when changing anything that reads or writes a skill lock file, the universal/.agents install layout, the folder hash, or skillPath, so an aghub-written artifact stays readable by `npx skills` and vice-versa.
---

# npx `skills` interop contract

aghub must round-trip with the upstream `skills` CLI (source of truth:
`/home/audichuang/research/skills/src`). Keep these identical; additions are only
safe when they are ignored by the other tool. Layout/removal already match — see
[aghub-skills](../aghub-skills/SKILL.md) for the aghub-side map.

## Frozen — never change

- **Lock versions**: global `.skill-lock.json` = v3, project `skills-lock.json` =
  v1. NEVER bump — npx resets a lock to empty on a version it considers older,
  silently wiping cross-tool state.
- **Master location & name**: `<home if global | project-root if project>/.agents/skills/<sanitized-name>`.
  `sanitize_name` is a direct port of upstream `sanitizeName` (lowercase, runs of
  `[^a-z0-9._]+` → `-`, trim `.-`, 255-cap, `unnamed-skill` fallback).
- **Layout**: one physical copy at the Master + per-agent **symlink** Referrers
  pointing at it, with copy-on-symlink-failure fallback. Removal is
  reference-counted: delete the Master only when no other installed agent
  references it.
- **Folder hash**: SHA-256 → lowercase hex over a stream of, per file,
  `relativePath` (UTF-8, `\`→`/`) immediately followed by raw file bytes, NO
  delimiters; recurse skipping dirs literally named `.git` / `node_modules`;
  `lstat`-skip symlinks. Files sorted by **JS `localeCompare` (ICU)**.
- **skillPath**: repo-relative POSIX to SKILL.md (`SKILL.md` at root, else
  `<dir>/SKILL.md`); omit when absent. Locks are keyed by the **raw** frontmatter
  `name`, not the sanitized dir name.

## Additive rule (how aghub extends safely)

Any aghub-only lock key MUST be `#[serde(skip_serializing_if = "Option::is_none", default)]`
so npx-written entries deserialize cleanly and npx ignores aghub's extra keys.
Live extensions that round-trip (keep): global `contentHash` (real Source hash,
with `skillFolderHash` left empty), project `computedHash`, the transactional
rename, apply-update, the upstream-rename guard, atomic+locked lock writes, prune,
and the TOCTOU/containment-hardened removal.

## ⚠ Known divergences (align here, or document the gap)

- **Master materialization excludes** (real interop bug): upstream excludes
  `metadata.json` + `.git`/`__pycache__`/`__pypackages__`, dereferences symlinks,
  and skips broken ones when copying a source into the Master. aghub's
  `copy_dir_recursive` (`crates/core/src/skills/install_layout.rs`) does a plain
  copy with NO excludes → the Master can carry junk and hash differently from npx.
  Mirror the upstream excludes + symlink handling.
- **Hash sort collator** (parity gap): upstream sorts by `localeCompare` (ICU);
  aghub uses feruca UCA (`crates/skill/src/hash.rs`). For case-colliding / numeric
  / accented filenames the file order — and thus the hash — differs. Only the
  shared project `computedHash` is exposed → at worst a spurious "update
  available", never data loss. Treat a mismatching hash as recompute, never wipe.

## Cosmetic (round-trips; fix only for byte-diff minimization)

- Global symlink target is absolute in aghub vs always-relative upstream.
- Global lock: aghub adds a trailing newline + emits `contentHash` mid-struct;
  upstream has no trailing newline. Both parse fine.

Source-URL normalization (`getOwnerRepo`, SSH preservation, `SOURCE_ALIASES`,
`parseSource`) lives OUTSIDE `crates/skill` — in the `crates/api` install routes +
`aghub-git::resolve_remote_source`. Verifying hash parity changes? Use the
[testing-fs-failures](../testing-fs-failures/SKILL.md) techniques and re-pin
`crates/skill/tests/hash_parity_golden.rs` against real npx output.
