# DeepSeek Harness (`dsh`) descriptor notes

Research behind `crates/agents/src/agents/dsh.rs`. Product identity: repo
`deepseek-ai/deepseek-harness`, npm package `@deepseek-ai/dsh`, binary `dsh`.
It is NOT `lessweb/deepcode-cli` ("Deep Code", `./.deepcode/skills`) and NOT
DeepSeek-TUI (`~/.deepseek/mcp.json`); both are different products with
different layouts.

## Skill discovery roots

From the upstream README's table. Lower rank wins a duplicate name:

| Rank | Root           | Path                           |
| ---- | -------------- | ------------------------------ |
| 100  | project-dsh    | `<projectRoot>/.dsh/skills`    |
| 200  | project-agents | `<projectRoot>/.agents/skills` |
| 400  | user-dsh       | `<dshHome>/skills`             |
| 500  | user-agents    | `<agentsHome>/skills`          |

The PRIVATE `.dsh` slot outranks the shared `.agents` one at BOTH scopes, so a
grant written there wins discovery. That is also aghub's own convention
(`load_skills_from_dirs` is first-dir-wins and the winner becomes
`source_path`, the path `remove_skill` deletes and `check` hashes), so the
write dir goes first in every read list.

## Why `universal: false`

dsh's shared root is `$DSH_AGENTS_HOME`, default `~/.agents` — NOT
`$XDG_CONFIG_HOME/agents`, which is what the `universal` flag appends. Source:
`packages/skill/skill-filesystem/src/index.ts` L168,
`agentsHome = $DSH_AGENTS_HOME ?? ~/.agents`. The shared slot is spelled out in
the read lists instead, so dsh joins `.agents/skills` co-readership without
joining the XDG group.

## Skill shape

- A `<name>/SKILL.md` directory bundle, or a flat `<name>.md` at the top level
  of a root. Nested `**/SKILL.md` is deliberately not discovered.
- The frontmatter `name:` decides the skill's name — the directory name is only
  the discovery key, and a mismatch is silently tolerated.
- Names must match `^[a-z0-9]+(?:-[a-z0-9]+)*$`; anything else is dropped with
  a warning.
- Required frontmatter is `name` + `description`; `whenToUse`, `metadata`,
  `disable-model-invocation` and `user-invocable` are optional. No documented
  description-length or body-size limit.

## Symlinks

Followed, and the shape is exactly aghub's Master/Referrer model: upstream's
own test stages a symlinked directory and a symlinked flat `.md` under
`~/.dsh/skills`, loads both, and reports the realpathed target as `path` while
`resourceBase.path` stays the LINK. Broken links and `/dev/null` links are
ignored. The symlink-only install needs nothing special.

## Reserved `.system`

`.system` is reserved under the GLOBAL dsh root only — dsh skips that child.
`skill::sanitize_name` strips leading dots (`trim_start_matches(['.', '-'])`),
so a skill called `system` — or `.system` — lands at `<dshHome>/skills/system`
and cannot shadow the reserved child. There is nothing to guard, only something
not to "fix" later by letting a sanitized name keep its leading dot.

## Per-skill enable/disable

Frontmatter-only (`disable-model-invocation`, `user-invocable`), with no
external state file. A dsh "disable" therefore edits the shared Master and is
NOT per-agent — and it dirties the comparison hash `check` / `source diff` use,
so the skill reports `update-available` until it is pushed. aghub models no
skill toggle, so this is a note, not a capability.

## MCP: deliberately not supported

`capabilities.mcp` is all false. Reasons, so nobody re-opens it by accident:

- There is no `mcpServers` map and no `mcp.json`. An MCP server is a plugin row
  in a Cordis YAML COMPOSITION LIST —
  `~/.dsh/profiles/<profile>/cordis.patch.yml` plus a profile-independent
  `$DSH_HOME/cordis.patch.yml`.
- User scope only (no project-level MCP file is documented anywhere) and PER
  PROFILE, so a server added to `web` is invisible to `headless`. aghub's scope
  model has nowhere to put that.
- Transports are `stdio` and `streamable-http` ONLY. There is no `sse`, so
  `McpTransport::Sse` would have to be refused.
- The official examples interpolate env/headers with the non-standard `!!js`
  YAML tag (`GITHUB_TOKEN: !!js process.env.GITHUB_TOKEN`). A plain YAML
  serializer cannot round-trip that, so a rewrite would destroy a user's config
  — and root AGENTS.md is explicit that a value the model cannot hold must be
  refused, not approximated.
- Adding a row needs the `- insert:` verb; a bare `- id:` row is the OVERRIDE
  verb and silently no-ops (warning only) when the id matches nothing.

Supporting it needs its own format module and its own round-trip policy.

## Sub-agents

Not researched, so off at both scopes with the no-op I/O, rather than guessing
a path aghub would write into and dsh would never read.

## Project root

dsh defines it as the nearest ancestor holding `.git`, falling back to cwd.
aghub walks for agent markers, and root AGENTS.md is explicit that `.git` alone
is not enough. Where the two disagree, dsh scans `<gitRoot>/.dsh/skills` while
aghub writes `<aghubRoot>/.dsh/skills`. `.dsh` is a project marker so an
aghub-managed project agrees with dsh once a grant exists; aghub's root
detection is deliberately left alone.
