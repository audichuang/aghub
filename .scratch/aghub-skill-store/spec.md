# Skill store decoupling: `.aghub` as the Master, `.agents/skills` as a Referrer

Status: **revision 3 — implementation-ready**

Evidence in this directory (read the directory name from THIS file — the findings
files predate the `.aghub` rename and say `.agent-hub`):

- `discovery-findings.json` — 11-agent sweep. 178 impact sites, 103 affected tests.
- `attack-findings.json` — 16-agent adversarial pass on revision 1. 24 findings
  survived refutation, 21 empirical, 5 blockers.
- `attack2-findings.json` — 9-agent pass on revision 2's _new_ prescriptions plus a
  3-angle design panel with a judge. 30 findings survived, 18 empirical, 6 blockers.

Revisions 1 and 2 each had load-bearing claims proved false. Every one is recorded
under **Rejected alternatives** so it is not reinvented.

## Problem

`.agents/skills` is simultaneously two things:

1. **aghub's Master** — the single physical copy of every installed skill
   (`universal_canonical_dir`, `crates/core/src/skills/linker/mod.rs:15-27`).
2. **A live read path for a large minority of agents** — they scan it because their
   own vendors said to, not because aghub put anything there.

Verified against the descriptors (`crates/agents/src/agents/`):

| scope   | agents natively reading `.agents/skills`                                                | of which have **no** private skills dir |
| ------- | --------------------------------------------------------------------------------------- | --------------------------------------- |
| global  | 5 / 24 — codex, cursor, opencode, cline, warp                                           | 2 — cline, warp                         |
| project | 10 / 24 — codex, cline, warp, antigravity, copilot, gemini, amp, kimi, cursor, opencode | 8 — all but cursor, opencode            |

Because storage **is** a read path, installing is granting. `aghub skill add foo -a grok`
must materialize `~/.agents/skills/foo` for grok's Referrer to point at, and that act
alone hands the skill to codex, cursor, opencode, cline and warp. There is no way to
express "installed, granted to nobody" or "granted only to grok".

The cost is not only tokens. A harness loads every visible skill's `description` into
**every** request, so an irrelevant skill both bills per-turn and competes for trigger
matching against the skills that should fire.

## Decisions

| #   | Decision                                                                                                                                                                                                                                                                                          |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | Master moves to **`.aghub/<sanitized-name>`** — `~/.aghub` (global), `<root>/.aghub` (project). No agent reads it. (`agent-hub` already names an unrelated upstream UI branch, `UPSTREAM.md:29,45,183`.)                                                                                          |
| D2  | `.agents/skills/<sanitized-name>` is demoted to **an ordinary Referrer symlink**, created only when a targeted agent actually reads it.                                                                                                                                                           |
| D3  | **Per-agent Referrers wherever an agent has a private dir.** The shared `.agents/skills` slot is used ONLY for agents with no alternative. Verified: codex / cursor / opencode write to `~/.codex/skills`, `~/.cursor/skills`, `<config>/opencode/skills` at global — the primary use case works. |
| D4  | npx `skills` coexistence is **detect + repair, never defend**.                                                                                                                                                                                                                                    |
| D5  | Migration's **skill** worklist is the lock. Real directories under `.agents/skills` that no lock entry names are left untouched and reported.                                                                                                                                                     |
| D6  | Migration relinks **every agent that can read the skill today**, so no agent loses a skill it can read now.                                                                                                                                                                                       |
| D7  | **Migration is lazy per skill, plus an explicit bulk command.** A mutating flow that meets a `legacy` skill migrates that one skill inside its own transaction. Nothing migrates in bulk without an explicit command.                                                                             |
| D8  | **No authorization is persisted.** No sidecar, no grants file, no migration receipt. Crash-safety comes from ordering plus an idempotent, re-runnable migration.                                                                                                                                  |

### Why D7 replaced "this repo does not migrate"

Revision 2 claimed this repo is excluded because its `.agents/skills` directories are
not lock entries. **Measured and false**: `jq -r '.skills|keys[]' skills-lock.json`
against `ls .agents/skills` gives 22 of 23 present in the committed lock (only
`project-form-patterns` is absent). They _are_ D5's worklist.

With revision 2's mutation-triggered bulk migration, the first `aghub -p skills add`
run here would convert 22 tracked real directories into symlinks into an untracked
`.aghub/`, and the 22 tracked `.claude/skills/*` symlinks into two-hop links to
nothing on every fresh clone. Bulk-automatic migration also contradicts the project's
own principle that aghub "requires explicit opt-in for changes".

Under D7 this repo's skills stay put — they are hand-edited and never aghub-mutated.
D7 additionally closes a hole revision 2 left open: `apply-update` on a `legacy` skill
would write `.aghub/<n>` while agents still read the real directory — the same
silent-green failure fixed for npx, reappearing on every un-migrated host.

### The cost of D8, accepted

A Referrer a user deletes by hand is forever indistinguishable from one never granted.
There is no restore-my-grants and no audit trail. This buys the absence of a second
source of truth that would drift from disk on every host aghub does not exclusively
own (a VM, a restored backup, a hand-migrated machine), and the absence of a
false-alarming doctor — a doctor that cries wolf is worse than one that stays quiet.

## Naming

The lock is keyed by the **raw frontmatter `name`**; the on-disk directory is
`sanitize_name(name)` (upstream `sanitizeName`, frozen by the npx contract). Wherever
this spec writes `<name>` in a path it means the sanitized form, and every
Referrer/Master path pair must be derived through the same sanitizer.

## Contract

### One derivation, time-explicit

**Steady state.** An agent's candidate Referrer path is the write dir `classify_paths`
returns for that agent **against the new Master** — taken unconditionally, with no
`is_link` prefilter. Verified across all 25 agents: cursor / codex / opencode get their
private dirs, cline / warp get the shared slot. One call answers it.

**Migration time only.** "Which agents can read this skill today" is a _different_
question with a _different_ input: the OLD master path, or equivalently the agents
whose descriptor read paths contain `.agents/skills` at that scope. It is valid only at
the migration instant. `universal_relink_referrers`
(`crates/core/src/manager/skill.rs:1449`) answers the held-link half and carries a
precondition its own doc states — **it must be called BEFORE the master moves**,
because the per-link canonicalize resolves through the still-present master.

Revision 2 fused these into one union and broke both halves; see Rejected alternative 6.

### Shape-test the candidates

For a skill at a scope, `M = <store>/.aghub/<name>`, each candidate Referrer `R`:

| shape              | meaning                                                                                                                                                    |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **conformant**     | `R` is a link, resolves to `M`, and its one-hop target is not itself a link                                                                                |
| **absent**         | `symlink_metadata(R)` is `Err` — not granted. Legal, and **indistinguishable from a lost grant** (D8)                                                      |
| **legacy**         | lock names it, `.agents/skills/<name>` is a real directory, `M` does not exist. **Never a violation** — read paths must tolerate it on an un-migrated host |
| **aliased-master** | `R` resolves to `M` but `R` is not a link — a **parent** is. **Refuse, name the symlinked parent, never delete**                                           |

Everything else is a violation: a real directory that is not `M`, a link resolving
elsewhere, a dangling link, a link chain.

Because candidates are **path-derived, not shape-derived**, a dangling per-agent link,
a foreign target, and an npx-written real directory are all _reported_ rather than
silently filtered out. Revision 2's set was shape-derived and therefore circular — the
membership test was the predicate — so only the chain clause could ever fire.

### The predicate, and its error semantics

```
conformant(R, M) :=
     Linker::is_link(R)
  && canonicalize(R) and canonicalize(M) both Ok and equal
  && the one-hop target R.parent().join(read_link(R)) is not itself a symlink
```

**Two `Err`s must never compare equal.** Written as `canonicalize(R) == canonicalize(M)`
it does not even compile — `io::Error` is not `PartialEq` — and the obvious
`Option`-fold makes `None == None`, so a dangling `R` with a missing `M` certifies as
conformant. That state is reachable: a user deletes `.aghub` after migrating, every
Referrer dangles, the legacy arm needs a real directory so it does not fire, and
`apply-update` proceeds against a store it believes is healthy. Both sides must be
`Ok` before the comparison happens.

The OS resolves the intermediate `..` inside the third clause's `join`, so a symlinked
`.agents` parent is handled by the filesystem — **no lexical `..` walk**, so AGENTS.md's
ban on hand-rolled path normalization is untouched. `resolve_existing` is _not_ a
substitute: it canonicalizes, so a chain resolves through and certifies healthy. The
clause exists because npx's `createSymlink` (`installer.ts:224-234`) repoints agent
Referrers at `.agents/skills/<name>`, producing exactly that chain.

**Pick one normalizer and use it everywhere.** Four exist in the tree
(`canonicalize_lenient`, `Linker::is_link`, `resolve_existing`, bare `fs::canonicalize`)
and they disagree on the exact inputs this spec is about — absent paths especially.
Name the chosen one in the implementation and route every comparison through it.

### Identity before hash, always

Any comparison that can lead to a deletion runs the identity check first: resolve both
sides (`resolve_existing`, or `dev`+`ino` on unix). Same object → `aliased-master` →
refuse.

Verified with a compiled Rust program: with `.agents/skills` a symlink to `.aghub`,
`symlink_metadata(.agents/skills/<name>)` reports `is_symlink = false, is_dir = true` —
revision 1's "real directory = violation" — and the two paths are one inode, so any
hash comparison is trivially equal and the "safe" repair branch `remove_dir_all`s the
Master. After `fs::remove_dir_all(referrer)`, the Master was gone.

`Linker::is_link` (`linker/mod.rs:303-318`) has the same leaf-only blind spot and
`install_fetched.rs:437` already copies it. Fix at the shared helper, not per caller.

### Detection is bounded — say so

The shared `.agents/skills/<name>` slot has a fixed path per scope, so its shape is
decidable with no record. What stays undecidable under D8 is only **"never granted"
versus "grant removed"** — both are `absent`.

### Ordering

- Never create a Referrer before its Master exists.
- Remove a Referrer before its Master. npx's `computeSkillFolderHash` throws ENOENT on
  a dangling link and `sync.ts:203` does not wrap the call.

Unobservable on the success path — both orders leave the same final disk state. Assert
on `RemovalPlan.paths`, whose contract is that `execute_removal` follows the vector
order: `paths.position(referrer) < paths.position(master)`. Reversing the sort turns
that line red with no filesystem failure injection.

### Shared slots (Q2 decided)

No new field on `LinkNeed`, no `shared_slots` table. **One oracle**, already `pub`:
`removal::skill_dir_readers_outside(dir, scope, root, requested)` — its doc already
states the policy (naming every reader is legal; naming only some is the dangerous
case) and the API delete route already uses it.

1. **Delete `LinkNeed::NativeReader`** — unreachable once nothing reads `.aghub`. It is
   still constructed in source, so there is no dead-code warning and all eight
   `matches!` arms silently die. Deleting it makes the compiler enumerate every call
   site. Delete doctor's `AutoCovered` with it.
2. **Rename `NeedsLink`'s `agent_skills_dir` → `referrer_dir`** and rewrite the "private"
   doc comment, which is now a lie. The rename is a free compiler sweep that forces
   every consumer to look at the site once.
3. **Disclosure is computed once**, inside `materialize_universal_master` (it already
   holds the plans and dirs), and carried as `shared_with: Vec<&'static str>` on the
   existing `AgentInstallResult`. An **additive** field does not disturb the positional
   zip; CLI and both API routes read the same value, so nothing is hand-mirrored.
4. **Coverage wire**: drop `reads_master` / `writes_master` / `auto_covered` (constant
   false after the change) and add `shared_with: Vec<String>`, filled only on
   `classify_all`, which already pays full-roster cost.
5. Three verified install fixes: dedup `symlink_dirs`; fold `already_linked` parents
   into the attribution set (`created_referrer_dirs` stays `install.linked` only — it is
   the rollback receipt); `assert!(agent_results.len() == target_agents.len())` before
   returning.

`already_linked` attribution is load-bearing and must land in the **same commit** as
the `NativeReader` deletion. Today it is a cosmetic reinstall bug; the moment
`NativeReader` is gone it becomes a _first_-install bug for the second agent in a
shared slot — `installed: false` with `error: None`.

### Removal from a shared slot (Q3 decided)

**Refuse** — using the existing `kept`/refuse contract. No new flag, no new `outcome`,
no new refusal code.

Placed in `remove_skill_planned` (**not** `plan_removal` — `rename.rs` has two call
sites that must not be blocked), between `plan_removal` and `read_effect_after`, where
scope / project_root / requested are already in hand. New helper in `removal.rs`,
beside `single_agent_keep_reason`.

The criterion is **"who actually loses"**, not "who reads this directory": for each
unnamed agent that `skill_dir_readers_outside` returns, ask `read_effect_after` again
against _that agent's own_ read paths. codex with a private Referrer still reads the
skill after the shared slot goes, so `changed == false` and it is not a victim. The
verdict still has exactly one home.

When there is a victim: move the Referrer path from `plan.paths` into `plan.skipped`
**and set `plan.shared_master_kept = true`**. That flag is load-bearing — without it the
plan falls into `spared_everything` and the preview reports something still on disk as
`removed`/`absent`, the exact false green this spec exists to prevent. With it, the
existing path applies: preview reports `kept`; a `--yes` commit refuses with
`UNSUPPORTED_OPERATION` / HTTP 422 / exit 1, so no script sees exit 0.

Note this is the **third** producer of that flag (existing: `plan_copy_removal`'s
`UniversalMaster`, and `remove_skill_planned`'s `blocks` fold) — annotate it there.

The message must carry three things: the slot path, the unnamed agents that genuinely
lose the skill, and the full `-a` spelling that would be accepted. `reconcile` inherits
the whole-batch refusal for free, since it routes through `remove_skill_planned` and the
refusal precedes the first write.

**Consequence to state in the docs**: for the project-scope eight and global
cline/warp, revoking from one agent is effectively impossible — every co-affected agent
must be named. The message must say plainly that this is an agent-side limitation
(Non-goals), or it becomes a recurring support question.

### Discovery must stop hiding the shared Referrer

`load_skills_from_dirs` (`crates/core/src/skills/discovery.rs:109-129`) keeps only the
first directory's entry per name, while `read_effect_after` walks per-directory without
dedup. The desktop shows one location, the user unchecks it, and core refuses citing a
location **no surface ever displayed**.

This is now **blocking for Q3**: the dialog submits only the agents the location view
lists, and a shared-slot reader deduped away is never listed, so per-location delete
would be refused across the board. Either drop dedup for the location view or make the
refusal name the shadowed path. Needs a test: submitting every reader of a location does
not trigger the refusal.

Also dedup the per-peer `read_effect_after` by canonical directory — at project scope it
would otherwise scan one shared directory eight times.

### Desktop UI

The install panel currently renders the leak as a feature. `partitionByCoverage`
(`lib/agent-capabilities.ts:88-100`) splits installable agents into `linkTargets`
(checkboxes) and `autoCovered` (read-only chips), and the `autoCovered` hint says
verbatim: _"這些 Agent 會直接讀取共用的 .agents 主檔,因此不會建立連結。"_ — which is
precisely "these five get it and you cannot opt out", presented as coverage.

After the change `autoCovered` is **permanently empty** (nothing reads `.aghub`), so the
bucket must be deleted rather than left to render an empty section. Concretely:

- Delete `isAutoCoveredByMaster`, the `autoCovered` half of `partitionByCoverage`, and
  its two render sites (`import-github-skill-panel.tsx:989`, `source-detail.tsx:1494`).
- Retire the `sourceInstallCoveredTitle` / `sourceInstallCoveredHint` /
  `agentCoveredBadge` strings in all three locales; the `linkTargets` hint must stop
  saying "指向共用 .agents 主檔" — the target is now the `.aghub` store.
- codex / cursor / opencode move from read-only chips to ordinary checkboxes. That
  transition **is** the user-visible feature.
- Shared-slot agents (global cline+warp; the project eight) need a grouped control
  driven by `shared_with`: checking one checks the group, and the group is labelled as
  an agent-side limitation, not an aghub choice. Without it a user unchecks codex at
  project scope, sees success, and seven other agents silently keep the skill — or,
  post-Q3, gets a refusal they cannot act on because the dialog never listed the other
  seven.
- `SkillResponse`'s doc comment and `agents.ts:43` both describe the NativeReader
  split; both become false.

### `capabilities.skills.universal` (Q4 decided)

**Delete the flag.** Inline the two paths it appends directly into amp's and kimi's
descriptor read paths (`<home>/.config/agents/skills` and the XDG
`agents/skills`). Write paths are untouched, so behavior is bit-identical — a pure
refactor needing no vendor facts about whether amp/kimi honor XDG.

Then remove: the bool and its two branches in `descriptor.rs`, 27 `universal: false`
lines, the api DTO field and its filler in `routes/agents.rs`, the regenerated ts-rs
type, and the `descriptor_regression` column.

Not in scope: repointing amp/kimi's global **write** path at
`get_universal_skills_path()`. That would relocate the Referrer of every user with
`XDG_CONFIG_HOME` set, on the strength of the same unverified vendor fact. Separate
issue.

## Where the shape check lives

An explicit **`verify_shape(scope, &names)`** in core. **Not on either mutation guard**:

1. `skill::lock::guard::mutation_guard` (`guard.rs:267`) **cannot** run it —
   `crates/skill` has no dependency on `aghub-agents` or `aghub-core`, and the
   dependency runs the other way (`crates/core/Cargo.toml:17`).
2. Both guards are **scope-granular**, never skill-granular (`MutationScope` is
   `Global | Project(PathBuf)`, `guard.rs:76-81`; `guard.rs:71-74` says per-skill locks
   are deliberately absent). One clobbered skill would refuse every unrelated mutation
   at that scope — **including the commands that repair it**.
3. The migration would refuse itself; on an api-only host the server would not start.

### Landing points

**`RemovalOutcome::preview` and `RemovalOutcome::commit`** (`removal.rs:1059` and its
preview sibling) — **not** `plan_removal` / `skill_for_planned_removal`. Both delete
branches and the rename path pass through that pair, so preview and commit share the
check **by construction**, which is the property revision 2 wanted and did not get.

Revision 2's landing point missed a whole branch: `DELETE /skills/by-path` has two, and
when `canonical_layout` is false the route hand-builds
`RemovalPlan { layout: Copy, paths: [skill_dir] }` and calls `RemovalOutcome` directly,
touching neither named function. Worse, `canonical_layout` is
`get_skill(name).canonical_path.is_some() || Linker::is_link(skill_dir)` — and a
zero-Referrer Master is absent from `get_skill` while its path is a real directory, so
this feature's flagship state lands in exactly the branch with no check and gets
`remove_dir_all`'d.

**`skill_for_planned_removal` needs a lock+store fallback.** A correctly placed check is
worthless if the next step cannot find the resource: after D1, a Master with zero
Referrers — the "installed, granted to nobody" state this feature exists to create —
fails lookup and the whole delete returns `RESOURCE_NOT_FOUND`. The fallback consults
the lock and `<store>/.aghub/sanitize_name(name)` directly, the same principle as D5.

Other rules:

- Refuses **only the skills the command names**.
- **Migration and repair do not call it** — they run outside the invariant they
  establish.
- Both lock-write floor acquisitions (`lock/io.rs:13`, `lock/local.rs:17`) stay
  check-free: primitives, not flow entry points.
- **Read verbs never refuse.** `GET /skills/check-updates` is not read-only — it calls
  `write_auto_healed_hashes` (`routes/skills_update.rs:180`, invoked at `:562`), whose
  lock writes take the skill-level guard, and a refusal there maps to
  `InternalServerError` at `:243-250`/`:275-282` — an **intermittent 500** raised only
  when a heal is pending, which no smoke test would catch. Report shape as per-skill
  status in the existing `SkillUpdateStatusResponse`.
- Flows without a single decision point (`install`'s common funnel is already past the
  first write for `add_skill_universal`; `prune-lock` does not know its names at
  decision time; `accept-rename`'s preview never enters core) are called out per-flow in
  the issues, not papered over here.

Verifying that each flow calls it cannot copy `mutation_lock_flows.rs` — those tests
detect lock acquisition by _timing_, and `verify_shape` has no timing signature. Use a
call-count or proof-token instrument on `execute_removal`.

## Lock reads: fail closed

Both the invariant check and the migration take the lock as their worklist, so both use
the **fail-closed** readers (`read_global_lock_checked` / `read_local_lock_checked`).
The fail-open readers return an empty lock on unparseable JSON — and `skills-lock.json`
is committed, so one unresolved merge conflict yields zero entries, making the migration
silently move nothing while reporting success, and making the invariant vacuously true
exactly when the disk is most suspect.

Root AGENTS.md already covers the shape: "An unreadable lock fails the commands that
report it."

Not adopted: erroring on an old lock version — for the global lock that diverges from
npx's v2-wipe with no recoverable loss.

## npx `skills` coexistence

Verified against `skills` v1.5.19 (`/home/audichuang/research/vercel_npx_skill`) with
live fixtures on Node v26.8.1.

| npx verb                                                        | Behavior                                                                                        | Consequence                                                          |
| --------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| `list`, folder hash                                             | Follows the symlink; digest byte-identical                                                      | Safe                                                                 |
| `experimental_sync`                                             | Discovers from `node_modules`, never from the lock                                              | Safe except on a name collision                                      |
| `remove`                                                        | `scanDir` (`remove.ts:45`) filters on `isDirectory()`, false for symlinks                       | **Silent no-op**, exit 0, "No skills found to remove"                |
| `add` / `install` / `update` / `check` / `experimental_install` | `cleanAndCreateDirectory` (`installer.ts:359`) unlinks the Referrer and writes a real directory | **Cancels the withhold and forks the content**, no error either side |

`check` is an alias for `update`, not a read. `experimental_install` is the lock-driven
wholesale rebuild.

**No direct data loss from npx**: `fs.rm` dispatches on `lstat`, so it unlinks the
Referrer without descending into `.aghub`. The loss risk is ours.

npx's `addSkillToLock` (`skill-lock.ts:205-221`) writes
`lock.skills[name] = { ...entry, installedAt, updatedAt }` — a whole-entry overwrite, so
**any npx write already wipes aghub's `contentHash` and `refCommit`**. Pre-existing, and
the reason no aghub key can be added to the npx lock.

### Repair policy

The comparison is **tri-state**: `Equal | Diverged | Undecidable(HashError)`.
`Undecidable` routes to the same refuse-and-disclose path as `Diverged` — two `Err`s
must never compare equal. **Undecidable must have an exit**: some hash failures are
permanent and deterministic (an unreadable file), and a user must not be wedged forever
with no way out. Name the escape in the message.

Hash equality is **not** content equality. `compute_skill_folder_hash` is npx-parity by
design: it skips symlinks (`hash.rs:120-122`), skips `.git` and `node_modules`
(`:124-126`), never records empty directories, and hashes no mode or permission data
(`:61-69`). An npx fork can hash equal while exclusively holding a user's `.git/`, a
`node_modules/` tree, symlinks, or empty dirs.

So the `Equal` branch **quarantines by rename**, never `remove_dir_all` — and does it in
an order that needs no rollback:

1. Create the replacement Referrer at a **dot-prefixed temp name inside the slot
   directory** — this proves link creation works on this filesystem _before_ anything
   destructive. (A symlink, so npx's `scanDir` and agent scanners skip it.)
2. Rename the fork into `.aghub/.quarantine/<name>/<stamp>/`.
3. Rename the temp link over the real name.

Revision 2 had steps 2 and 3 only, leaving a window in which the skill was readable from
nowhere — and anything ending the process there (SIGKILL, ENOSPC, EACCES, a Windows host
where both `symlink_dir` and the `mklink /J` fallback fail, `linker/mod.rs:504-515`)
leaves the _legal_ `absent` shape, so a dead repair became indistinguishable from a
deliberate withhold.

Quarantine constraints: nested `<name>/<stamp>/`, **not** flat `<name>-<stamp>` —
sanitized names contain hyphens, so the flat form cannot be split back, and the nested
form lets doctor tell a died repair (quarantine present, slot absent) from an ordinary
leftover with no sidecar. `<stamp>` must be collision-safe; a same-instant collision is
a hard error mid-repair. Rename can fail cross-filesystem (EXDEV) and on Windows sharing
violations — specify the fallback. Quarantine currently grows without bound and no verb
lists or reaps it; give it one.

**"Structurally invisible to the store scan" is a property of one function, not of the
layout.** `top_level_skill_dirs` (`prune.rs:464-481`) is one level deep and requires a
root `SKILL.md`, so `.quarantine` is invisible _there_ — but doctor lists it as a skill
and permanently reddens `--fail-on-issues`. Audit **every** enumerator of `.aghub`
(prune, discovery, doctor, coverage, `source list`, `GET /skills`) and keep store scans
one level deep.

### `~/.agents/` is never removed

The shared global lock lives at `~/.agents/.skill-lock.json` (or
`$XDG_STATE_HOME/skills/.skill-lock.json`) — `crates/skill/src/lock/io.rs:20-31`,
upstream `skill-lock.ts:67-73`. Deleting a seemingly-empty `~/.agents` wipes it, and
upstream `readSkillLock` returns an empty lock instead of erroring, so the loss presents
as "no skills tracked". The `npx-skills-contract` skill records this path wrongly as
`~/.skill-lock.json` and must be corrected.

## Migration (D7)

Per skill, lazily on mutation, plus an explicit bulk command. Skill worklist from the
lock (D5); "who can read it today" answered at the migration instant against the OLD
master.

### Order (no rollback needed, no receipt needed)

1. Copy out to `.aghub/<n>`.
2. Create the Referrer for every agent with a private dir.
3. Swap the shared slot last: create the temp link `.agents/skills/.<n>.aghub-migrating`,
   rename the old real directory into `.aghub/.quarantine/<n>/<stamp>/`, rename the temp
   link over `<n>`. POSIX forbids renaming a symlink over a non-empty directory, so the
   temp name is the only way to shrink the window to zero; it is transient and a resume
   unlinks it.

Any crash leaves exactly one of two states: the old real directory still serving
(**legacy** — nobody lost anything), or `M` present with `R` a real directory — which is
precisely npx's shape and is absorbed by the existing repair policy. **No fifth shape is
needed, and no migration receipt.**

The temp link is picked up by discovery as a duplicate entry pointing at the same Master
(names come from frontmatter, not the folder), harmless after identity resolution — but
resume must unlink it and no scan may go deeper than one level.

Creating Referrers must be **idempotent against an existing link or an existing chain**:
after a crashed repair, private Referrers may already point at `.agents/skills/<n>`, and
re-running turns them into chains that clause 3 rejects, wedging that skill until
migration reruns.

Migration runs **outside `verify_shape`** — its input is `legacy` by construction.

### Where the bulk command runs

**There is no CLI or API "startup".** The CLI's `main()` is `Cli::parse()` then
`run(cli)` (`crates/cli/src/main.rs:694-706`); a call at the top of `run` would make
`check`, `doctor`, `get` and `coverage` take the interprocess mutation lock and write —
contradicting AGENTS.md ("read paths are deliberately unlocked"), `check.rs:496-500`'s
own read-only premise, and doctor's read-only doc.

The bulk command is an ordinary CLI verb, an API route, **and a desktop entry point**.
For an api-only host (`crates/remote`'s VM, any standalone `aghub-api`) the operator
invokes the route; under D7 nothing migrates behind their back, which also **dissolves
the remote version-skew question** revision 2 left open.

### Migration expands implicit reads into explicit grants — this is the point

Moving the Master alone is worthless. Today codex, cursor and opencode read
`~/.agents/skills` **implicitly**, holding no link of their own; if migration only
recreates the shared `.agents/skills` Referrer, those three stay fused to cline and warp
and revoking one of them is still impossible. The feature would be un-delivered.

So migration step 2 (private-dir Referrers) is load-bearing, not an optimization: it
converts every implicit read into an explicit, individually revocable link.

```
before                                    after
~/.agents/skills/foo/       (Master)      ~/.aghub/foo/                      (Master)
  implicitly read by:                     ~/.agents/skills/foo -> Master     (cline, warp)
    codex cursor opencode cline warp      ~/.codex/skills/foo  -> Master     NEW
~/.claude/skills/foo -> Master            ~/.cursor/skills/foo -> Master     NEW
                                          ~/.config/opencode/skills/foo -> Master  NEW
                                          ~/.claude/skills/foo -> Master     repointed
```

`delete foo -a codex` is impossible before and correct after. The acceptance suite must
pin this: migrate a fixture, then revoke codex, and assert cursor / opencode / cline /
warp still resolve the skill.

### Desktop entry point

The desktop must expose the bulk migration, because "one click" is the whole value for a
GUI user, and because the desktop cannot reach core directly (it depends on `aghub-api`,
not `aghub-core`) — the route is its only path.

Requirements:

- A per-scope banner when any lock entry is `legacy`, stating how many skills are
  un-migrated and what migrating will do — not a silent nag.
- The action shows a **preview first**: which Masters move, which new per-agent
  Referrers appear, which agents remain fused to the shared slot afterwards. A user who
  cannot see that codex is about to become individually revocable does not know what
  the button bought them.
- Progress is per skill, and a partial run is resumable: D7 migration is idempotent, so
  re-running after a failure is safe and must be offered rather than requiring a reset.
- The banner must not appear for a host with nothing to migrate, and must not appear at
  all in a project whose `legacy` directories are unlocked (D5 leaves those alone).

### Link target

`<root>/.agents/skills/<n> -> ../../.aghub/<n>` is the first link aghub creates pointing
**out** of `.agents/skills`. `relative_path` (`linker/mod.rs:106-135`) is purely lexical,
so a symlinked `.agents` parent dangles it — the failure AGENTS.md bans hand-rolled
normalization for, now on the **write** path. Resolve the `from` directory first, or use
`LinkTarget::Absolute`. Windows junctions resolve absolute regardless.

### `.gitignore`

`.aghub` is a new top-level directory in every project that gets a project-scope
install. Decide whether install writes a `.gitignore` entry. Note `.agents` cannot serve
as a "was anything granted" assertion target — the project mutation lock creates it.

## Day-one hazards (must ship in the same commit as the change they guard)

1. **`removal::universal_master_roots` must gain `.aghub`'s two scope paths**, renamed
   `skill_store_roots`. It feeds `allowed_skill_roots` (the deletion allowlist — without
   it every Master delete is refused as out-of-tree) **and**
   `is_universal_master` / `single_agent_keep_reason` (without it a single-agent delete
   `remove_dir_all`s the Master). Both consumers, one commit.
2. **`doctor`'s `master_state` needs a `legacy` fallback.** `health_of` maps
   `(true, MasterState::Missing)` to `orphan-lock`, and `master_state` only inspects the
   master passed in. The moment the master points at `.aghub`, every un-migrated tracked
   skill is reported `orphan-lock` **with a note telling the user to run `prune-lock`** —
   which would cut live lock entries.
3. **`already_linked` attribution** with the `NativeReader` deletion (above).
4. **`plan.shared_master_kept = true`** with the Q3 helper (above).
5. **Discovery dedup** removed (or the refusal made to name the shadowed path) with Q3.
6. **Wire breakage lands together**: `SkillCapabilitiesDto.universal` and the coverage
   view's `reads_master`/`writes_master`/`auto_covered` disappear while
   `AgentInstallResult` gains `shared_with`. Regenerate ts-rs and the desktop DTOs in the
   same commit or the mismatch only surfaces at typecheck.

## Non-goals — the floor this cannot lift

For agents whose only skills directory **is** `.agents/skills`, per-agent targeting is
impossible: their directory is the shared one. `.aghub` buys "withhold from all of
them", never "give to one".

- global: cline, warp (2)
- project: codex, cline, warp, antigravity, copilot, gemini, amp, kimi (8)

amp and kimi also share `~/.config/agents/skills` at global — a second shared slot with
the same property.

Agent-side constraint. Do not design around it; disclose it.

## Acceptance

**Test 1 — the central promise.** `crates/cli/tests/cli_tests.rs`, using the existing
`isolated_cli(home, state)`:

1. `aghub -g skills add <local path> -a grok`
2. `home/.aghub/<n>/SKILL.md` exists, `symlink_metadata` says real dir
3. `home/.grok/skills/<n>` is a symlink whose canonicalize equals that of
   `home/.aghub/<n>`
4. **`home/.agents/skills/<n>` does not exist** — assert `symlink_metadata().is_err()`,
   never `exists()`, which returns false for a dangling symlink and would pass a broken
   link as success
5. codex / cursor / opencode / cline / warp global read dirs contain no `<n>`

**Line 4 is the proof.** Revert the change and it goes red — today's `install_universal`
always materializes `~/.agents/skills/<n>`. Lines 2, 3 and 5 can be green beforehand.

**Test 2 — preview/commit parity.** Seed lock entry `alpha`, create
`~/.aghub/alpha/SKILL.md`, make `~/.agents/skills/alpha` a **real directory** (npx's
shape). Run `aghub -g --json delete skills alpha` **without** `--yes`. Assert the JSON
carries the shape-refusal `error.code` and exit 1. Moving the check off
`RemovalOutcome` turns this red — the preview reverts to exit 0 with `outcome: "preview"`.

**Test 3 — Q3.** Preview returns `kept` and names the leftover; `--yes` returns
`unsupported_operation` and exit 1. Removing the `shared_master_kept = true` line turns
the preview into `removed`/`absent`.

**Test 4 — Q3 does not block legitimate deletes.** Submitting every reader of a location
does not trigger the refusal.

Use the CLI harness, not in-process. `isolated_cli` already sets
HOME/USERPROFILE/APPDATA/XDG_STATE_HOME and calls `clear_agent_home_overrides` (11
agent-specific vars). `set_skills_path_override` is a thread_local holding **one**
`(agent_id, path)` pair and could never isolate the six agents test 1 observes. If an
in-process test is unavoidable, lift `clear_agent_home_overrides`' list to a shared
constant and clear it via `EnvVarGuard` under the same binary's env lock.

codex's global read path hard-codes `/etc/codex/skills` and cannot be isolated — use a
fixture name that cannot collide (`aghub-withhold-fixture`).

For mid-flight failure coverage use the repo's `testing-fs-failures` ENOTDIR technique
(make an agent's skills dir a file), which is root-safe. Do not use `chmod`.

## Implementation order

1. **Core spine, serially**, until `cargo check -p aghub-core` is green: split
   `universal_canonical_dir`; add `.aghub` to `skill_store_roots`; delete
   `LinkNeed::NativeReader`; rename `agent_skills_dir` → `referrer_dir`; the single
   derivation; `verify_shape` on `RemovalOutcome`; the Q3 helper; migration.
2. **Fan out** cli / api / desktop / docs against that frozen API.
3. **The 65 false-green tests** as their own phase with its own proof obligation:
   revert the fix, watch the assertion go red, restore. This is the phase that gets
   skipped.
4. `just preflight` (does **not** run prettier/eslint — the pre-push hook does, from the
   repo root).

## Rejected alternatives (do not reinvent)

1. **Recording the granted agent set in the npx lock.** `addSkillToLock` overwrites the
   whole entry; bumping the version makes npx's `readSkillLock` (`skill-lock.ts:93-98`)
   return `createEmptyLockFile()` — every user entry vanishes at once.
2. **An aghub-side grants sidecar (`.aghub/.grants.json`).** A second source of truth
   that drifts from disk on any host aghub does not exclusively own; false-alarms
   whenever a user tidies their own links; lands in the user's repo root at project
   scope; and buys only the ability to distinguish a hand-deleted link from a
   never-granted one, which is not an input to any command.
3. **A `.aghub/.migrating/<n>` marker and a fifth `Interrupted` shape.** Upgrades a
   window that ordering alone eliminates into a persistent file with no TTL that wedges
   a skill after SIGKILL until a human intervenes.
4. **Hosting the shape check on either mutation guard.** Wrong crate, wrong granularity,
   self-refusing migration, skips every preview path.
5. **An exemption flag on guard acquisition.** Hand-mirrored per-call-site policy.
6. **The held-links ∪ native-readers union.** Both arms fail: `NativeReader` is a unit
   variant carrying no path, so cursor/opencode/codex lose their private dirs exactly
   where D3 needs them; and `universal_relink_referrers`' filter admits only paths that
   are already conformant, so `absent`, `legacy`, `aliased-master` and npx's real
   directory are filtered out before the predicate runs.
7. **`resolve_existing` as the chain test.** It canonicalizes, so a two-hop chain
   resolves through and certifies healthy.
8. **`remove_dir_all` on a hash-equal fork.** Hash equality is not content equality, and
   under aliasing the two paths are one inode.
9. **A `Slot::Private | Slot::Shared` enum on `LinkNeed`.** Moves full-roster path
   resolution into `agent_link_need`, which is documented to stay cheap and is called
   once per copy row in `transfer.rs`; and duplicates a judgment
   `skill_dir_readers_outside` already owns.
10. **"Proceed with disclosure" for shared-slot removal.** Reproduces on a new surface
    the `reconcile mcp` gap AGENTS.md already records as "verified, unfixed".
11. **Repointing amp/kimi's global write path at `get_universal_skills_path()`.** Would
    relocate the Referrer of every user with `XDG_CONFIG_HOME` set, on an unverified
    vendor fact, inside an already-large refactor. Separate issue.
