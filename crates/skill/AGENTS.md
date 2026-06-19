# SKILL CRATE KNOWLEDGE BASE

**Crate**: skill — Skill packaging library for `.skill` (zip) format

## OVERVIEW

Pack, unpack, parse, and validate AI agent skill packages. Extends skills-ref with zip-based packaging and lock file management for tracking skill dependencies with content hashes.

## STRUCTURE

```
src/
├── lib.rs           # Public exports (Skill, SkillError, pack, unpack, parse, validate)
├── error.rs         # SkillError enum with thiserror
├── model.rs         # Skill struct, SkillSource enum
├── package.rs       # pack(), unpack(), read_skill_md() — zip I/O
├── parser.rs        # parse() auto-detects format; parse_skill_md, parse_skill_dir, parse_skill_file
├── validator.rs     # validate() with path traversal protection
├── sanitize.rs      # sanitize_name() for safe directory names
├── hash.rs          # compute_skill_folder_hash() — npx-compatible SHA-256 of the SOURCE folder
└── lock/            # Lock file management (npx `skills`-compatible)
    ├── io.rs        # global path: $XDG_STATE_HOME/skills/.skill-lock.json → ~/.agents/.skill-lock.json (v3); atomic temp+rename under WRITE_LOCK
    ├── types.rs     # SkillLockEntry (contentHash) / LocalSkillLockEntry (computedHash, skillPath)
    ├── global.rs    # global-lock entry ops (add/remove/retain_locked_skills)
    ├── local.rs     # project lock: <project>/skills-lock.json (v1)
    └── test_utils.rs # Mutex-based test isolation
```

## WHERE TO LOOK

| Task                 | File                 | Notes                                                    |
| -------------------- | -------------------- | -------------------------------------------------------- |
| Pack skill to .skill | `src/package.rs`     | Excludes **pycache**, node_modules, .git, tests/ at root |
| Parse any format     | `src/parser.rs`      | Auto-detects directory, .skill, .zip, .md                |
| Validate skill       | `src/validator.rs`   | Checks for path traversal (`..`) in resources            |
| Sanitize name        | `src/sanitize.rs`    | Converts "My Skill!" → "my-skill"                        |
| Global lock ops      | `src/lock/global.rs` | Per-user skill registry                                  |
| Local lock ops       | `src/lock/local.rs`  | Per-project skill registry                               |

## COMMANDS

```bash
# Build this crate only
cargo build -p skill

# Test with test isolation (uses mutex for lock file tests)
cargo test -p skill
```

## CONVENTIONS

- **Skill name rules**: lowercase, hyphens not spaces, no `..` in paths
- **Required field**: `name` and `description` in SKILL.md frontmatter (rejected if non-string)
- **Source path**: `~` prefix for home-relative paths
- **Lock entries**: Track `content_hash` (SHA-256) for integrity
- **npx-compatible locks**: global `.skill-lock.json` (v3) + project `skills-lock.json` (v1) round-trip with `npx skills`; keep global `skill_folder_hash` EMPTY and store the real SHA in the additive `content_hash`/`computed_hash`; never bump the versions. `hash.rs` mirrors npx `computeSkillFolderHash` (parity is fixture-pinned, see `tests/hash_parity_golden.rs`)

## ANTI-PATTERNS

- **NEVER** use non-string frontmatter values for `name`/`description` (rejected by parser)
- **NEVER** allow `..` in resource paths (validated in `validate_skill_structure`)
- **NEVER** write lock tests without `with_test_lock()` mutex guard (prevents test flakiness)
- **NEVER** pack `tests/` or `evals/` at skill root (intentionally excluded)
