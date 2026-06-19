# JSON CRATE KNOWLEDGE BASE

**Crate**: `aghub-json` — JSON / JSONC parsing + editing helpers\
**Role in monorepo**: Leaf utility. Lets callers update managed config fields
while **preserving comments and formatting** around untouched fields (agent
config files are JSONC with user comments we must not clobber).

## OVERVIEW

Built on `jsonc-parser`'s CST API. The point of this crate over plain
`serde_json` is comment/formatting preservation on round-trip edits.

## PUBLIC API (`src/lib.rs`)

| Item                        | Purpose                                                           |
| --------------------------- | ----------------------------------------------------------------- |
| `parse_jsonc_opt::<T>()`    | Parse JSONC into `Option<T>` (tolerates comments / trailing JSON) |
| `patch_jsonc_object::<T>()` | Merge `T`'s fields into an object CST, preserving untouched bits  |
| `JsonError`                 | `Parse` / `Serialize` / `ExpectedObject` (thiserror)              |

## CONVENTIONS

- Use `patch_jsonc_object` (not re-serialize-from-scratch) when editing an
  existing config file, so user comments and formatting survive.
- Root must be an object for object patching → `JsonError::ExpectedObject`.

## DEPENDENTS

`aghub-inference` (SQLite/keyring-backed provider config). Keep the API minimal
and general — this is a shared low-level helper, not inference-specific.

## ANTI-PATTERNS

- **NEVER** round-trip a config file through plain `serde_json` for edits — it
  drops comments and reflows formatting. Use the CST patch path here.
