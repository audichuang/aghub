# INFERENCE CRATE KNOWLEDGE BASE

**Crate**: `aghub-inference` — Inference-provider management (LLM endpoints/keys
configured into agents).

## OVERVIEW

SQLite metadata (`store.rs`) + platform keyring for API keys (`credentials.rs`)
— never secrets in the DB. Per-agent adapters (`claude/`, `codex/`, `opencode/`)
map the normalized model into each agent's config format.

## WHERE TO LOOK

| Task                         | Location                                       |
| ---------------------------- | ---------------------------------------------- |
| Add/list providers, bindings | `store.rs`                                     |
| API key get/set              | `credentials.rs`                               |
| Provider delete teardown     | `cascade.rs` (shared by API + CLI; never fork) |
| Per-agent config write       | `<agent>/files.rs`                             |
| Normalize ↔ agent schema     | `<agent>/mapping.rs` (Claude: `claude/mod.rs`) |

## GOTCHAS / ANTI-PATTERNS

- **Secrets in keyring, metadata in SQLite** — never persist API keys in the DB
- **「Active」provider is not an SQLite binding column** (dropped in
  `0006_drop_binding_is_active`). Selection lives in per-agent adapters, not
  inventory rows
- This crate's `CredentialStore` is **distinct** from git/source credentials in
  `crates/api` — don't conflate them
- Each agent maps differently; model changes must update every agent's mapping
  (`codex`/`opencode`: `mapping.rs`; Claude: `claude/mod.rs` — no `mapping.rs`)

```bash
cargo test -p aghub-inference
```
