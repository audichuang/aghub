# SKILLS-SH CRATE KNOWLEDGE BASE

**Crate**: `skills-sh` — HTTP client for the skills.sh registry (search only).

**Used by**: `aghub-api` market route only — **not** `aghub-core`.

## ROLE

Thin reqwest wrapper: `Client` / `ClientBuilder` + search DTOs. Override base URL
with `ClientBuilder::api_url(...)` or `Client::from_env()` (`SKILLS_API_URL`).

## ANTI-PATTERNS

- NEVER call skills.sh HTTP directly from other crates — go through `Client`
- NEVER hardcode the registry URL in callers — use env / builder for tests
