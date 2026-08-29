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
- **An unreadable config now errors instead of reading as empty.** "Absent"
  and "unreadable" are different answers, and every level of the read now says
  so: the directory, each entry in it, and the FILE itself. `get skills` on an
  unreadable skills dir exits 1 rather than printing `[]` on exit 0.
  **Consequence worth knowing before you hit it:** `load_config` reads an
  agent's mcps, skills and sub-agents as one all-or-nothing unit, so an
  unreadable `~/.claude/skills` also fails that agent's `get mcps`. That
  structure is older than this release (a broken `~/.claude.json` has always
  failed `get skills`); this release widens what can trigger it. Fail-loud and
  safe, but not yet per-resource — issue 10. Commands that read every agent
  (`check`, `doctor`, `coverage`, `skill-usage`, `get -a all`) still fail OPEN.

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

A seventh round — three independent lenses over the six rounds' own output —
found the same sentence one level lower. The fixes had all landed on the
**directory** (`read_dir`, and the stat of each entry in it); the read of the
**file** was still swallowing errors, and so were the sibling sweeps that make
the same "is anyone else still holding this?" decision for `delete`. Four more,
each with a control/fault pair that differs by one permission bit:

- An unreadable `SKILL.md` inside a readable skill directory read as "no skill
  here", because every parse failure was treated as "this is a group directory,
  recurse". The holder went invisible with no warning at all, and in a
  copy layout the resulting exhaustive verdict widened a single-agent removal
  into a name-based sweep that deleted an **untargeted** agent's own skill
  directory. Only a genuinely missing `SKILL.md` recurses now.
- An unreadable sub-agent `.md` read as absent, so `transfer`'s already-exists
  check passed and the write **overwrote it**. The same command refuses with
  "already exists" when the file is readable.
- `delete --all-agents` dropped an agent directory it could not stat out of the
  referrer sweep entirely — neither counted as a holder nor unlinked — and
  still reported `success: true` with that path silently missing from `paths`.
- A live cross-agent symlink in an unreadable directory was invisible to the
  "is anything still pointing at this?" check, so the directory it pointed at
  was `remove_dir_all`'d and the peer left dangling.

Those four are **pre-existing on `main`** — this branch's review process found
them, it did not introduce them. The claim above ("an agent whose config could
not be READ is no longer counted as one that holds nothing") is only true with
them fixed.

I/O errors from these paths now name the path. They used to reach the user as a
bare `Permission denied (os error 13)` — sometimes about a directory the
command was not even asking about.

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

Round 7 added four more records rather than four more fixes, on a deliberate
line — **a reproduced false success on a mutating verb gets fixed; a false
failure or an unreproduced suspicion gets recorded**:

- **10** — `load_config` is all-or-nothing across mcps/skills/sub-agents. The
  only pure false-FAILURE in the set, and that atomicity is what makes the
  `load_failed` guard work at all; per-resource granularity is its own change,
  not a release-eve one.
- **11** — a copy-layout directory kept because another agent's link still
  points at it reports `outcome: "removed"`. Misleading, but `paths: []` and
  `skipped: [...]` carry the truth beside it, and the one-line fix would widen
  a field three decision sites read.
- **12** — file-level `.ok()` swallows located but not reproduced as
  destructive: codex's own sub-agent loader (its write path re-reads and
  refuses, verified), `prune.rs`'s single unguarded stat, `accept_rename`'s
  target set.
- **13** — the two loaders changed in the same commit disagree on "path exists
  but is not a directory"; ELOOP now hard-fails; `load_failed` reaches no DTO,
  so the desktop under-reports silently instead of erroring.

The rest are pre-existing and out of this branch's scope: sub-agent rename
leaving the old file, the API skill-import lock overwrite, the API duplicate-add
answering 201, `add --from --name` dropping the source tree, npx-hash blind
spots, `doctor` UX debts, and `RemovalOutcome::commit()` not asserting the
mutation lock itself.
