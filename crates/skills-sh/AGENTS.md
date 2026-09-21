# SKILLS-SH CRATE KNOWLEDGE BASE

**Crate**: `skills-sh` — HTTP client for the skills.sh registry (search only).

Thin reqwest wrapper: `Client` / `ClientBuilder` + search DTOs. Almost everything
about it is visible from `src/` or enforced by the dependency graph; the one
thing that is not:

- The base URL is overridable for tests — `ClientBuilder::api_url(...)` or
  `Client::from_env()` (`SKILLS_API_URL`). That variable is **process-wide** and
  cargo runs tests as threads of one process, so two tests driving it must share
  a lock (`ENV_LOCK` in `client.rs`), not just set and reset it.
