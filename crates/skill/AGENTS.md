# SKILL CRATE KNOWLEDGE BASE

**Crate**: `skill` — pack/unpack/parse/validate skill packages (`.skill` zip) and
npx-compatible lock files with content hashes. Extends external `skills-ref`.

## WHERE TO LOOK

| Task                     | Location                 | Notes                                                                                                                                |
| ------------------------ | ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------ |
| Pack / unpack `.skill`   | `src/package.rs`         | Excludes any-level `__pycache__`/`node_modules`/…; root-only `tests`/`evals`                                                         |
| Parse any on-disk form   | `src/parser.rs`          | Dir / `.skill` / `.zip` / `.md`                                                                                                      |
| Sanitize skill name      | `src/sanitize.rs`        | `"My Skill!"` → `my-skill`                                                                                                           |
| Folder content hash      | `src/hash.rs`            | npx-compatible SHA-256 (golden: `tests/hash_parity_golden.rs`)                                                                       |
| Repo-relative skill path | `src/skill_path.rs`      | `SkillPath` newtype — path-traversal guard for git installs                                                                          |
| Repo skill discovery     | `src/scan.rs`            | `discover_from_entries` pure policy; gitignore-aware (`tests/discovery_policy.rs`)                                                   |
| Repo install helpers     | `src/install.rs`         | `RepoDiscoveredSkill`, empty-lock digest                                                                                             |
| Global lock              | `src/lock/global.rs`     | Entry CRUD (v3)                                                                                                                      |
| Lock path + atomic IO    | `src/lock/io.rs`         | `$XDG_STATE_HOME/skills/.skill-lock.json`, else `~/.agents/.skill-lock.json` (or `./.agents` if `home_dir()` is None); temp + rename |
| Project lock             | `src/lock/local.rs`      | `<project>/skills-lock.json` (v1)                                                                                                    |
| Lock test isolation      | `src/lock/test_utils.rs` | `TestLockGuard::new()` — mutex + `XDG_STATE_HOME`                                                                                    |

## NPX LOCK CONTRACT (do not break)

The `npx skills` round-trip contract — frozen versions (global v3 / project
v1), empty global `skill_folder_hash`, atomic writes — is owned by the project
skill **`npx-skills-contract`**; read it before touching lock read/write.

## ANTI-PATTERNS

- **NEVER** use non-string frontmatter for `name`/`description` (parser rejects)
- **NEVER** allow `..` in resource paths (`validate_skill_structure`); repo-relative skill paths go through the `SkillPath` newtype, never raw strings
- **NEVER** write lock tests without `TestLockGuard::new()` (serializes + isolates state home)
- **NEVER** pack root `tests/` / `evals/` (intentionally excluded from packages)
