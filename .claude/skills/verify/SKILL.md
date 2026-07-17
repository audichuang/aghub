---
name: verify
description: Runtime-verify aghub's skill fetch/install/update chain end-to-end — build the CLI + API binaries, run them under an isolated HOME + XDG environment, install from a real GitHub repo and from a local git daemon (gix shallow path), and observe lock/symlink/disk state. Use when verifying changes to skill install, source sync, check, or apply-update.
---

# Verifying aghub skill fetch/install end-to-end

## Build + launch (isolation is the whole game)

```bash
cargo build -p aghub-cli -p aghub-api --bin aghub-cli --bin aghub-api

# Grab real-credential material BEFORE isolating — afterwards gh/git can no
# longer see their real config (XDG_CONFIG_HOME points into the sandbox):
export GITHUB_TOKEN="${GITHUB_TOKEN:-$(gh auth token)}"

# NEVER run against the real environment. HOME alone is NOT enough: the global
# lock prefers $XDG_STATE_HOME (crates/skill/src/lock/io.rs) and universal-skill
# agents read $XDG_CONFIG_HOME/agents/skills. Clear the whole XDG class (HOME
# fallback then lands every persistent dirs:: path this recipe touches in the
# sandbox), then pin the two this recipe references:
export VHOME=$(mktemp -d)
unset "${!XDG_@}"
export HOME=$VHOME XDG_STATE_HOME=$VHOME/state XDG_CONFIG_HOME=$VHOME/config

# --port 0 + the liftoff port line: readiness comes from OUR child's stdout, so a
# stale aghub-api (running against the real HOME) on a fixed port can never be
# mistaken for the sandboxed one.
target/debug/aghub-api --port 0 > "$VHOME/api.log" 2>&1 & API_PID=$!
trap 'kill $API_PID 2>/dev/null' EXIT
API_PORT=
for _ in $(seq 50); do
  kill -0 $API_PID 2>/dev/null || break                      # died on startup
  API_PORT=$(sed -n 's/^AGHUB_API_PORT=//p' "$VHOME/api.log" | head -1)
  [ -n "$API_PORT" ] && break
  sleep 0.2
done
[ -n "$API_PORT" ] || { echo "aghub-api not ready" >&2; exit 1; }
```

Known isolation limit: the **OS keyring is NOT isolated** by any env var. If the real
keyring holds a credential binding for the source's host, API installs will use it —
state that precondition in your report instead of assuming anonymous.

Token model — know which surface reads what:

- **API `/skills/install` does NOT read `GITHUB_TOKEN` env.** Its tokens come from the
  keyring credential store or the forwarded `X-Aghub-Git-Tokens` header only. With an
  empty keyring, a curl against a public github repo goes ANONYMOUS.
- **CLI** (`source diff/sync`, `check --online`, `apply-update`) DOES read
  `GIT_PASSWORD`/`GITHUB_TOKEN` env — `export` them first (see gotcha below).

Gotchas:

- bash prefix assignments apply left-to-right: in `HOME=$VHOME GITHUB_TOKEN=$(gh auth token) cmd`
  the `gh` call runs with the isolated HOME and returns an EMPTY token — which is why
  the launch block above exports the token BEFORE isolating.
- Global CLI flags go BEFORE the subcommand: `aghub-cli -g check skills`, not `check skills -g`.
- CWD matters: project-scope commands read the lock of the repo you're standing in (real data).
  Stick to `-g` (global = isolated env) or cd to a temp dir.

## API install outcome (github.com source)

What this proves: the install RESULT (lock pin, hash, master dir, symlink) — NOT which
backend served it. A resolve-time REST failure falls back to gix transparently with
byte-identical results (post-resolve REST errors are clean failures, not fallbacks —
but you cannot tell the two backends apart from disk). Backend-level proof lives in
the cargo tests: the `NeverBackend` fake-transport suite, plus the token-gated real
REST E2E — `cargo test --workspace --test skill_repository -- --ignored real_github_rest`
(inherits the `GITHUB_TOKEN` exported before isolation; re-running `gh auth token`
here would read the sandboxed config and return empty).

```bash
curl -fs --max-time 30 -X POST "http://127.0.0.1:$API_PORT/api/v1/skills/install" -H 'Content-Type: application/json' \
  -d '{"source":"https://github.com/vercel-labs/skills.git","agents":["claude"],"skills":["find-skills"],"scope":"global","project_path":null,"install_all":false}'
```

Observe: lock at `$XDG_STATE_HOME/skills/.skill-lock.json` (or `$VHOME/.agents/.skill-lock.json`
when XDG_STATE_HOME is unset) — v3; `refCommit` must equal `git ls-remote <url> HEAD`;
`contentHash` set, `skillFolderHash` empty per npx contract; master dir
`$VHOME/.agents/skills/<name>/`; symlink `$VHOME/.claude/skills/<name>`.

## gix shallow path + update chain (controllable upstream)

A local `git daemon` gives a non-github host AND lets you push v2 to drive the update
chain. This is also automated: `gix_daemon_roundtrip_fetches_content_and_sees_upstream_advance`
in `crates/skill-update/tests/skill_repository.rs` (its `spawn_git_daemon` helper handles
the port race) — prefer extending that test over re-doing this manually.

```bash
S=$(mktemp -d); mkdir -p $S/srv/testrepo/skills/hello-world
# write SKILL.md with YAML frontmatter (name/description), git init -b main, commit
git daemon --base-path=$S/srv --export-all --listen=127.0.0.1 --port=9419 & DAEMON_PID=$!
trap 'kill $API_PID $DAEMON_PID 2>/dev/null' EXIT
ready=0
for _ in $(seq 50); do
  kill -0 $DAEMON_PID 2>/dev/null || break                   # stolen port kills it instantly
  # bound the probe too — git:// has no client-side timeout (GNU timeout; on macOS use gtimeout)
  timeout 2 git ls-remote git://127.0.0.1:9419/testrepo >/dev/null 2>&1 && { ready=1; break; }
  sleep 0.2
done
[ "$ready" = 1 ] || { echo "git daemon not ready (port taken?)" >&2; exit 1; }
# non-bare works fine (the automated tests serve a worktree);
# the URL path must match the dir name EXACTLY — /testrepo, not /testrepo.git
# install with source "git://127.0.0.1:9419/testrepo" → GixShallow path
# commit v2 in the served worktree, then:
aghub-cli -g apply-update skills hello-world        # dry-run: refuses without --yes
aghub-cli -g apply-update skills hello-world --yes  # disk v1→v2, lock refCommit advances
```

## Expected non-obvious behavior (NOT bugs)

- `check skills` is **offline by default** → remote sources report `uncheckable/network`.
  Pass `--online` for a real answer.
- `check --online` on a `git://` source → `uncheckable/unsupportedScheme` (https-only path, by spec).
- Re-installing an already-installed skill: response is `success:false` with all per-agent rows
  `success:true` (aggregate = `any_installed && all rows ok`; idempotent no-op sets no
  `installed` flag). Pre-existing on main.
- Observed without a token on a PUBLIC github source: `source diff` says "needs a
  credential… GIT_PASSWORD/GITHUB_TOKEN". (A private source may classify as
  `uncheckable/auth` instead — `check.rs` supports both reasons; verify before asserting.)
