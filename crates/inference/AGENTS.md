# INFERENCE CRATE KNOWLEDGE BASE

**Crate**: `aghub-inference` — Inference-provider management (the LLM endpoints/keys aghub configures into agents).

## OVERVIEW

Stores provider inventory + bindings as **SQLite metadata** (`store.rs`) and keeps secret API keys in the **platform keyring** (`credentials.rs`) — never in the DB. Adapts a provider into each agent's own config format: Claude, Codex, OpenCode (one subdir each, with `files.rs` = read/write that agent's config + `mapping.rs` = normalize ↔ that agent's schema).

## STRUCTURE

```
src/
├── lib.rs          # Public exports (re-exports InferenceProviderError)
├── error.rs        # InferenceProviderError enum
├── model.rs        # Provider / binding / capability enums (AgentProvider*)
├── store.rs        # SQLite metadata (inventory, bindings, active provider)
├── credentials.rs  # Platform keyring for API keys (CredentialStore)
├── cascade.rs      # delete_provider_cascade — the SINGLE delete-teardown seam
│                   #   shared by the API route AND CLI `inference delete`;
│                   #   they must not diverge
├── agent.rs        # Cross-agent binding orchestration
├── claude/         # files.rs (config I/O) + mod.rs
├── codex/          # files.rs + mapping.rs + mod.rs  (TOML)
└── opencode/       # files.rs + mapping.rs + schema.rs + mod.rs
```

## WHERE TO LOOK

| Task                         | Location                                           |
| ---------------------------- | -------------------------------------------------- |
| Add/list providers, bindings | `src/store.rs`                                     |
| API key get/set              | `src/credentials.rs`                               |
| Provider delete teardown     | `src/cascade.rs` (shared by API + CLI; never fork) |
| Per-agent config write       | `src/<agent>/files.rs`                             |
| Normalize ↔ agent schema     | `src/<agent>/mapping.rs`                           |

## COMMANDS

```bash
cargo test -p aghub-inference
```

## GOTCHAS / ANTI-PATTERNS

- **Secrets live in the keyring, metadata in SQLite — keep them split.** Never persist an API key in the SQLite store.
- A separate `CredentialStore` trait exists here for provider keys; it is **distinct** from the git/source credentials in `crates/api` — don't conflate them.
- Each agent maps differently (Codex/OpenCode are TOML/JSON with their own keys); changes to the normalized model must update every `<agent>/mapping.rs`.
