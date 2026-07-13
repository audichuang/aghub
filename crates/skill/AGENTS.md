# SKILL CRATE KNOWLEDGE BASE

**Crate**: `skill` — pack/unpack/parse/validate skill packages (`.skill` zip) and
npx-compatible lock files with content hashes. Extends external `skills-ref`.

## WHERE TO LOOK

| Task                   | Location                 | Notes                                                                                                                      |
| ---------------------- | ------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| Pack / unpack `.skill` | `src/package.rs`         | Root excludes `__pycache__`, `node_modules`, …                                                                             |
| Parse any on-disk form | `src/parser.rs`          | Dir / `.skill` / `.zip` / `.md`                                                                                            |
| Sanitize skill name    | `src/sanitize.rs`        | `"My Skill!"` → `my-skill`                                                                                                 |
| Folder content hash    | `src/hash.rs`            | npx-compatible SHA-256 (golden: `tests/hash_parity_golden.rs`)                                                             |
| Global lock            | `src/lock/global.rs`     | `$XDG_STATE_HOME/skills/.skill-lock.json`, else `~/.agents/.skill-lock.json` (or `./.agents` if `home_dir()` is None) (v3) |
| Project lock           | `src/lock/local.rs`      | `<project>/skills-lock.json` (v1)                                                                                          |
| Lock test isolation    | `src/lock/test_utils.rs` | `TestLockGuard::new()` — mutex + `XDG_STATE_HOME`                                                                          |

## NPX LOCK CONTRACT (do not break)

Round-trip with `npx skills` (also documented in skill `npx-skills-contract`):

- Global lock v3 + project lock v1 — **never bump** those versions casually
- Keep global `skill_folder_hash` **empty**; real integrity is
  `content_hash` / `computed_hash` (Source hash)
- Atomic lock writes under the write lock (temp + rename)

## ANTI-PATTERNS

- **NEVER** use non-string frontmatter for `name`/`description` (parser rejects)
- **NEVER** allow `..` in resource paths (`validate_skill_structure`)
- **NEVER** write lock tests without `TestLockGuard::new()` (serializes + isolates state home)
- **NEVER** pack root `tests/` / `evals/` (intentionally excluded from packages)
