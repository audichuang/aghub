---
name: verify
description: Runtime-verify aghub's skill fetch/install/update chain end-to-end — build the CLI + API binaries, run them under an isolated $HOME, install from a real GitHub repo (REST fast-path) and from a local git daemon (gix shallow fallback), and observe lock/symlink/disk state. Use when verifying changes to skill install, source sync, check, or apply-update.
---

# Verifying aghub skill fetch/install end-to-end

## Build + launch

```bash
cargo build -p aghub-cli -p aghub-api --bin aghub-cli --bin aghub-api
VHOME=$(mktemp -d)   # NEVER run against real $HOME — global installs write ~/.agents + ~/.claude
HOME=$VHOME GITHUB_TOKEN=$(gh auth token) target/debug/aghub-api --port 18877 &
```

Gotchas:

- bash prefix assignments apply left-to-right: in `HOME=$VHOME GITHUB_TOKEN=$(gh auth token) cmd`
  the `gh` call runs with the isolated HOME and returns an EMPTY token. `export GITHUB_TOKEN` first.
- Global CLI flags go BEFORE the subcommand: `aghub-cli -g check skills`, not `check skills -g`.
- CWD matters: project-scope commands read the lock of the repo you're standing in (real data).
  Stick to `-g` (global = isolated VHOME) or cd to a temp dir.

## REST fast-path (github.com)

```bash
curl -s -X POST http://127.0.0.1:18877/api/v1/skills/install -H 'Content-Type: application/json' \
  -d '{"source":"https://github.com/vercel-labs/skills.git","agents":["claude"],"skills":["find-skills"],"scope":"global","project_path":null,"install_all":false}'
```

Observe: `$VHOME/.agents/.skill-lock.json` (v3; `refCommit` must equal
`git ls-remote <url> HEAD`; `contentHash` set, `skillFolderHash` empty per npx contract),
master dir `$VHOME/.agents/skills/<name>/`, symlink `$VHOME/.claude/skills/<name>`.

## gix shallow fallback + update chain (controllable upstream)

A local `git daemon` gives a non-github host AND lets you push v2 to drive the update chain:

```bash
S=$(mktemp -d); mkdir -p $S/srv/testrepo/skills/hello-world
# write SKILL.md with YAML frontmatter (name/description), git init -b main, commit
git daemon --base-path=$S/srv --export-all --port=9419 &
# non-bare works fine (skill_repository.rs's daemon tests serve a worktree);
# the URL path must match the dir name EXACTLY — /testrepo, not /testrepo.git
# install with source "git://127.0.0.1:9419/testrepo" → GixShallow path
# commit v2 in the served worktree, then:
HOME=$VHOME aghub-cli -g apply-update skills hello-world        # dry-run: refuses without --yes
HOME=$VHOME aghub-cli -g apply-update skills hello-world --yes  # disk v1→v2, lock refCommit advances
```

## Expected non-obvious behavior (NOT bugs)

- `check skills` is **offline by default** → remote sources report `uncheckable/network`.
  Pass `--online` for a real answer.
- `check --online` on a `git://` source → `uncheckable/unsupportedScheme` (https-only path, by spec).
- Re-installing an already-installed skill: response is `success:false` with all per-agent rows
  `success:true` (aggregate = `any_installed && all rows ok`; idempotent no-op sets no
  `installed` flag). Pre-existing on main.
- Missing token: `check` degrades to `uncheckable/network`; `source diff` says
  "needs a credential… GIT_PASSWORD/GITHUB_TOKEN".
