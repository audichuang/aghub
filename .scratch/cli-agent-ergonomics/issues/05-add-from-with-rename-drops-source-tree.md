# 05 — `add skills --from <dir> --name <new>` drops the whole source tree

**Status:** open · pre-existing

`crates/cli/src/commands/add.rs:91`: when `--name` renames the skill, the flow
falls back to writing a stub from the parsed struct instead of copying the source
folder, so `assets/`, `scripts/` and `references/` never arrive — and the
original skill is left installed beside the stub.

**Fix direction:** rename is a folder copy plus a frontmatter edit, not a
re-serialization. Reuse the same materializer the un-renamed path uses.

**Found by:** round-4 workflow (HIGH, confirmed).
