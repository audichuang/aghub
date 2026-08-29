# v2.15.0 — CLI agent-operability

A **minor**, not a patch: several parts of the CLI contract changed.

## Contract changes — read this if you script against aghub

- **`delete` now exits non-zero when it did not finish.** `RemovalKind::Partial`
  (the removal ran and at least one path could not be deleted) used to report
  `"success": true` and exit 0 with the skill still on disk. It now reports
  `success: false` and exits 1, on both the single-agent and the `-a a,b`
  fan-out paths. **A script that treated exit 0 as "it is gone" was wrong
  before and is right now — but a script that ignored the exit code and parsed
  `success` will start seeing failures it used to miss.**
- **`delete --json` carries `outcome`**: `preview` | `removed` | `absent` |
  `partial` | `kept`. Read that, not `dry_run`/`executed`. `kept` means the
  shared `.agents/skills` master was left because another agent still reads it
  — `success: true` AND THE SKILL IS STILL THERE. A preview also carries
  `would_prune_lock_entries`, separate from the committed `pruned_lock_entries`.
- **`check` defaults to BOTH scopes**, like `doctor` / `source list` /
  `source diff`. It used to follow the plain global default and answer "up to
  date" from the global lock alone when run inside a project.
- **Narrowed resource arguments**: `check` / `apply-update` take `skills` only;
  `enable` / `disable` take `mcps` only. Previously-accepted spellings now fail
  as a clap parse error naming the valid values (exit 2).
- **`--json` failures are JSON too**: `{"error":{code,message,retryable}}` on
  stdout, exit 1, with `code` drawn from the same vocabulary the HTTP API sends.
- **`get skills` on an unreadable skills directory now errors** instead of
  printing `[]` on exit 0. "Absent" and "unreadable" are different answers.

## Data loss fixed

`reconcile` copies a resource to one agent and removes it from another, staged
so the removal only runs if the copy succeeded — but the gate only asked whether
the copy returned an error. Six rounds of independent adversarial review found
**five distinct paths** where a copy returned success having written nothing (or
having written something else), the paired removal ran anyway, and the resource
was gone from every agent while the command printed "N succeeded, 0 failed" and
exited 0.

- A removal may not take the resource from a copy target **or from the source**.
  Membership is a property of the FILE, so this compares `(device, inode)` —
  not agent ids, and not resolved path strings, which miss hard links. Checked
  once before any write and again at removal time, because some backing paths
  only settle after the copies have run.
- A skill copy must PROVE the source content landed before a paired removal.
  The proof refuses rather than certifies when it cannot see the whole tree
  (symlinks and `.git`/`node_modules` are invisible to the npx-compatible hash).
- The two universal install entry points now answer "already installed?" the
  same way, from the resolved path rather than the name.
- An agent whose config could not be READ is no longer counted as one that
  holds nothing when deciding whether a shared master still has readers.
- A refused copy rolls back the referrer it created, under the mutation lock.

## Honest reporting

- `add` reports the skill as it exists ON DISK. The materializer preserves a
  pre-existing master rather than overwriting it, so re-adding an edited source
  for a second agent wrote nothing — and the response echoed the source's
  description and version back. `add` said one thing and `get` said another,
  seconds apart, for the same agent.
- An existing master that does not parse is an error, not a reason to report the
  caller's own input back as if it were installed.
- The reconcile PREVIEW raises the shared-backing refusal the commit raises.
- `doctor --fail-on-issues` gates on `orphan-lock` and `invalid-skill` only.
  `untracked` (a skill written by hand into `.agents/skills` — this repo's own
  layout) and `master-is-symlink` are supported resting states; failing CI over
  them only teaches you to append `|| true`.

## Known and NOT fixed in this release

Full list with reproductions: `.scratch/cli-agent-ergonomics/issues/`.

**One deserves naming here.** This release makes the sub-agent
`transfer`/`reconcile` flows safe against losing the whole FILE. They still lose
FIELDS: `crates/agents/src/sub_agents.rs` models only `name` and `description`,
and the save-all loop rewrites every sub-agent in the target directory — so a
transfer naming one sub-agent strips `tools`, `model`, `color` and anything else
aghub does not model, from all of its siblings. Back up a sub-agent directory
that carries those before transferring into it.

The rest are pre-existing and out of this branch's scope: sub-agent rename
leaving the old file, the API skill-import lock overwrite, the API duplicate-add
answering 201, `add --from --name` dropping the source tree, npx-hash blind
spots, `doctor` UX debts, and `RemovalOutcome::commit()` not asserting the
mutation lock itself.
