# MARKDOWN CRATE KNOWLEDGE BASE

**Crate**: `aghub-markdown` — Generic YAML-frontmatter markdown parser/renderer\
**Role in monorepo**: Leaf utility. Splits a document into typed YAML
frontmatter (between leading `---` lines) and the body, and renders the pair
back. This is the shared primitive behind SKILL.md frontmatter handling.

## PUBLIC API (`src/lib.rs`)

| Item               | Purpose                                                      |
| ------------------ | ------------------------------------------------------------ |
| `parse::<T>()`     | `(meta: T, body: String)` — errors if frontmatter is missing |
| `parse_opt::<T>()` | Like `parse` but tolerates a document with no frontmatter    |
| `render::<T>()`    | Serialize `meta` + body back into a `---`-delimited document |
| `MarkdownError`    | Error enum (thiserror)                                       |

Frontmatter is delimited by `---` lines at the very top of the document.

## DEPENDENTS

`aghub-agents` (SKILL.md frontmatter: `name`, `description`, `author`,
`version`, `tools`). Keep this crate generic over `T` — agent-specific schema
(required fields, string-only validation) lives in the descriptor/parser layers,
**not** here.

## ANTI-PATTERNS

- **NEVER** bake skill-specific field rules into this crate — it stays generic;
  callers supply the `T` they deserialize into and enforce their own invariants.
- **NEVER** assume frontmatter exists — use `parse_opt` when a missing block is valid.
