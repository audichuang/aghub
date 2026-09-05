# Skill store decoupling: `.aghub` as the Master, `.agents/skills` as a Referrer

Status: spec, revision 2 — not implemented

Evidence in this directory:

- `discovery-findings.json` — 11-agent sweep. 178 impact sites, 103 affected tests.
  **Its prescriptions say `.agent-hub`; the store is now named `.aghub` (D1). Read
  the directory name from this spec, never from that file.**
- `attack-findings.json` — 16-agent adversarial pass over spec revision 1.
  24 findings survived independent refutation, 21 empirically verified, 5 blockers.
  Revision 2 exists because **three load-bearing claims in revision 1 were false**;
  each is recorded below as a rejected alternative so it is not reinvented.

## Problem

`.agents/skills` is simultaneously two things:

1. **aghub's Master** — the single physical copy of every installed skill
   (`universal_canonical_dir`, `crates/core/src/skills/linker/mod.rs:15-27`).
2. **A live read path for a large minority of agents** — they scan it because
   their own vendors said to, not because aghub put anything there.

Verified against the descriptors (`crates/agents/src/agents/`):

| scope   | agents natively reading `.agents/skills`                                                | of which have **no** private skills dir |
| ------- | --------------------------------------------------------------------------------------- | --------------------------------------- |
| global  | 5 / 24 — codex, cursor, opencode, cline, warp                                           | 2 — cline, warp                         |
| project | 10 / 24 — codex, cline, warp, antigravity, copilot, gemini, amp, kimi, cursor, opencode | 8 — all but cursor, opencode            |

Because storage **is** a read path, installing is granting. `aghub skill add foo -a grok`
must materialize `~/.agents/skills/foo` for grok's Referrer to point at, and that
act alone hands the skill to codex, cursor, opencode, cline and warp. There is no
way to express "installed, granted to nobody" or "granted only to grok".

The cost is not only tokens. A harness loads every visible skill's `description`
into **every** request, so an irrelevant skill both bills per-turn and competes
for trigger matching against the skills that should fire.

## Decisions (locked)

| #   | Decision                                                                                                                                                         | Rationale                                                                                                                                                                                    |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | Master moves to **`.aghub/<sanitized-name>`** — `~/.aghub` (global), `<root>/.aghub` (project). No agent reads it.                                               | `agent-hub` already names an unrelated upstream UI branch (`UPSTREAM.md:29,45,183`).                                                                                                         |
| D2  | `.agents/skills/<sanitized-name>` is demoted to **an ordinary Referrer symlink**, created only when a targeted agent actually reads it.                          | Splits storage from authorization — the point of the change.                                                                                                                                 |
| D3  | **Per-agent Referrers wherever an agent has a private dir.** The shared `.agents/skills` slot is used ONLY for agents with no alternative.                       | codex / cursor / opencode at global become individually protectable — the primary use case. A single shared Referrer would leave exactly those agents all-or-nothing.                        |
| D4  | npx `skills` coexistence is **detect + repair, never defend**.                                                                                                   | Defending means fighting npx's writes forever.                                                                                                                                               |
| D5  | Migration's **skill** worklist is the lock. Real directories under `.agents/skills` that no lock entry names are left untouched and reported.                    | aghub must not relocate content it did not install.                                                                                                                                          |
| D6  | Migration's **Referrer** worklist is derived, not recorded: for each migrated skill, every agent that can read it today. No agent loses a skill it can read now. | Revision 1 said "every agent the lock records". **Nothing records that** — see Rejected alternative 1.                                                                                       |
| D7  | **This repo does not migrate.** Its 40 tracked files under `.agents/skills` are not lock entries, so D5 already excludes them.                                   | Committed symlinks break Windows clones (`core.symlinks=false` yields text files); a migrated layout leaves 22 dangling two-hop links per clone plus a lock the first `delete --yes` prunes. |

## Naming

The lock is keyed by the **raw frontmatter `name`**; the on-disk directory is
`sanitize_name(name)` (upstream `sanitizeName`, frozen by the npx contract).
Wherever this spec writes `<name>` for a path it means the sanitized form, and
every Referrer/Master path pair must be derived through the same sanitizer.
Revision 1 conflated the two.

## Contract

### Referrer set (derived, never recorded)

Neither lock records a per-skill agent set. `SkillLockEntry`
(`crates/skill/src/lock/types.rs:9-57`) has eleven fields, none naming an agent;
`lastSelectedAgents` (`types.rs:78-84`) is **file-level UI state**, not per-skill;
`LocalSkillLockEntry` (`crates/skill/src/lock/local.rs:26-92`) has nothing either.

The Referrer set for a skill at a scope is therefore **derived**, from two existing
seams, and their union is the authoritative set:

- **Held links** — `removal::agent_skill_dirs_in_scope` feeding
  `universal_relink_referrers` (`crates/core/src/manager/skill.rs:1449`, called at
  `:1581`): agent dirs whose `<dir>/<name>` is a link resolving to the Master.
- **Native readers** — agents whose descriptor read paths include `.agents/skills`
  at that scope, via `classify_agent` / `agent_link_need`
  (`crates/core/src/skills/linker/classify.rs`). These hold no link today and are
  exactly the agents D6 promises not to break.

### The four shapes

For a skill at a scope, with `M = <store>/.aghub/<name>` and each derived Referrer
path `R`:

| shape              | test                                                                                          | meaning                                                                                                                  |
| ------------------ | --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| **conformant**     | `is_link(R)` ∧ `canonicalize(R) == canonicalize(M)` ∧ the one-hop target is not itself a link | healthy                                                                                                                  |
| **absent**         | `symlink_metadata(R)` is `Err`                                                                | not granted to that agent — legal, and **indistinguishable from a migration that died before linking**                   |
| **legacy**         | lock names it, `.agents/skills/<name>` is a real directory, `M` does not exist                | un-migrated. **Never a violation.** Read paths (`check`, `doctor`, `list`) must tolerate it on an un-migrated host       |
| **aliased-master** | `canonicalize(R) == canonicalize(M)` but `is_link(R)` is false                                | a **parent** of `R` is a symlink, so `R` and `M` are the same inode. **Refuse, name the symlinked parent, never delete** |

Everything else — a real directory that is not `M`, a link resolving elsewhere, a
dangling link, a link chain — is a violation.

**Delete revision 1's sentence "One `symlink_metadata` per Referrer decides it.
There are no heuristics and no unclassifiable states." It is false in both halves.**
`symlink_metadata` is `lstat`: it returns a file type and nothing about the target,
so a healthy Referrer, a two-hop chain, and a dangling link are indistinguishable
by it. Deciding conformance requires:

```
conformant(R, M) :=
     Linker::is_link(R)
  && canonicalize(R) == canonicalize(M)                                  // endpoint
  && !symlink_metadata(R.parent().join(read_link(R)))?.is_symlink()      // one hop, not a chain
```

The OS resolves the intermediate `..` inside that `join`, so a symlinked `.agents`
parent is handled by the filesystem — **no lexical `..` walk**, and AGENTS.md's ban
on hand-rolled path normalization is not touched. `resolve_existing` is _not_ a
substitute for the third clause: it canonicalizes, so a chain resolves through and
certifies as healthy. The third clause exists because npx's `createSymlink`
(`installer.ts:224-234`) repoints agent Referrers at `.agents/skills/<name>`,
producing exactly that chain.

### Detection is bounded — say so

The shared `.agents/skills/<name>` slot has a **fixed path per scope**, so its shape
is decidable with no record. A **missing per-agent Referrer is not detectable**:
"never granted" and "the link was removed" are the same observation. Policy 1 is
scoped to what is decidable; anything stronger needs authorization persisted
somewhere, which is deliberately left open below.

### Identity before hash, always

Any comparison that can lead to a deletion runs the identity check first: resolve
both sides (`skill::lock::resolve_existing`, or `dev`+`ino` on unix). If they are
the same object, the shape is `aliased-master` — refuse.

This is not theoretical. With `.agents/skills` a symlink to `.aghub` (stow, or a
user hand-fixing their own layout), `symlink_metadata(.agents/skills/<name>)`
reports `is_symlink = false, is_dir = true` — revision 1's "real directory =
violation" — and the two paths are one inode, so any hash comparison is trivially
equal and the "safe" repair branch `remove_dir_all`s the Master. Verified with a
compiled Rust program: after `fs::remove_dir_all(referrer)`, the Master was gone.

`Linker::is_link` (`linker/mod.rs:303-318`) has the same leaf-only blind spot, and
`install_fetched.rs:437` already copies that pattern. Both inherit the bug unless
the identity check is added at the shared helper, not per caller.

The reverse variant — a user creates `.aghub -> .agents/skills` expecting a free
migration, migration finds `.aghub/<name>` present and hash-equal, and replaces
`.agents/skills/<name>` with a symlink to itself — is blocked by the same check.

### Ordering

- Never create a Referrer before its Master exists.
- Remove a Referrer before its Master. npx's `computeSkillFolderHash` throws ENOENT
  on a dangling link and `sync.ts:203` does not wrap the call, so a dangling
  Referrer breaks the other tool.

This ordering is **unobservable on the success path** — both orders leave the same
final disk state. Assert it on `RemovalPlan.paths`, whose documented contract is
that `execute_removal` follows the vector order:
`paths.position(referrer) < paths.position(master)`. Reversing the sort turns that
line red with no filesystem failure injection.

### Shared slots are first-class

Ten agent/scope combinations resolve their write dir to a directory shared with up
to seven others. `LinkNeed::NeedsLink { agent_skills_dir }` documents that path as
"private"; that is false after this change, and every consumer keyed on it
(install result attribution, doctor rows, `transfer.rs:1739`'s protect set, the
desktop bucketing) conflates those agents into one identity today.

Sharing is computed **once**, in `classify`, and carried on the plan. Granting to
one member grants to all, and that must reach the user before the write.

Constraint on any slot-dedup: `materialize_universal_master` must keep emitting
exactly one `AgentInstallResult` per requested agent — both API install routes zip
the requested list **positionally** against `report.agent_results`. Dedup belongs on
the linker's private input list only, with an
`agent_results.len() == target_agents.len()` assertion.

### Removal

`removal::read_effect_after` stays the single verdict, with two changes under it:

- Removing the shared Referrer takes the skill from every agent sharing the slot,
  most unnamed in the command. The user must be told which, before the write.
- A Master with zero Referrers is the **normal** "installed, granted to nobody"
  state, not an orphan. `doctor`'s `orphanMaster` must be redefined or it reports
  the feature's happy path as a fault.

### Discovery must stop hiding the shared Referrer

`load_skills_from_dirs` (`crates/core/src/skills/discovery.rs:109-129`) keeps only
the first directory's entry per name, while `read_effect_after` walks per-directory
without dedup. The desktop shows one location, the user unchecks it, and core
refuses citing a second location **no surface ever displayed**. Either the location
view drops dedup, or the refusal names the shadowed path.

## Where the shape check lives

**Not on either mutation guard.** Revision 1 said "all mutating flows funnel through
the one interprocess mutation guard; the check goes there". That is wrong three ways:

1. `skill::lock::guard::mutation_guard` (`crates/skill/src/lock/guard.rs:267`)
   **cannot** run it — `crates/skill` has no dependency on `aghub-agents` or
   `aghub-core`, and the dependency runs the other way
   (`crates/core/Cargo.toml:17`). It cannot compute a Referrer path.
2. Both guards are **scope-granular**, never skill-granular (`MutationScope` is
   `Global | Project(PathBuf)`, `guard.rs:76-81`; `guard.rs:71-74` states per-skill
   locks are deliberately absent). One npx-clobbered skill would refuse `delete B`,
   `prune-lock`, `source sync`, `transfer`, `reconcile` and every unrelated install
   at that scope — **including the commands that would repair it**.
3. **The migration would refuse itself.** Its input state _is_ the violation, and
   revision 1 put both at the same acquisition point. On an api-only host that
   means the server does not come up.

Instead: an explicit **`verify_shape(scope, &names_this_command_touches)`** in core.

- Called by each mutating flow **at the point it decides what to do**
  (`plan_removal` / `skill_for_planned_removal`), not at guard acquisition — so
  **preview and commit run the identical check**. `remove_skill_planned` takes no
  guard when `dry_run`, and the API delete takes none unless `req.confirm`, so a
  guard-hosted check would let a preview report `outcome: removed` for a shape the
  same command with `--yes` then refuses.
- Refuses **only the skills the command names**.
- The **migration and the Policy-2 repair do not call it** — they run outside the
  invariant they establish. State this; it is unstatable from revision 1's text.
- Both lock-write floor acquisitions (`lock/io.rs:13`, `lock/local.rs:17`) stay
  check-free: they are primitives, not flow entry points.
- `verify_shape` must be **idempotent and cheap** — `mutation_guard` is reentrant
  and genuinely nests (`transfer.rs:1851` outer, `:1906` per copy row).
- **Read verbs never refuse.** `GET /skills/check-updates` is not read-only — it
  calls `write_auto_healed_hashes` (`crates/api/src/routes/skills_update.rs:180`,
  invoked at `:562`), whose lock writes take the skill-level guard. A refusal there
  maps into `ApiError::new(Status::InternalServerError, ...)` at `:243-250` /
  `:275-282` — an **intermittent 500**, raised only when a heal is pending, which
  no smoke test would catch. Report shape as per-skill status in the existing
  `SkillUpdateStatusResponse`; never a 500, never a silent heal.
- Owe a test per mutating flow that it calls `verify_shape`, in the shape of the
  existing `crates/core/tests/mutation_lock_flows.rs` guard-taking tests.

Rejected: an exemption flag on the guard acquisition. That is per-call-site policy
hand-mirrored across surfaces, which AGENTS.md bans outright.

## Lock reads: fail closed

Both the invariant check and the migration take the lock as their worklist, so both
use the **fail-closed** readers (`read_global_lock_checked` /
`read_local_lock_checked`). The fail-open readers return an empty lock on
unparseable JSON — and `skills-lock.json` is a committed file, so one unresolved
merge conflict yields zero entries, which makes the migration silently move nothing
(D5: "no lock entry names them") while reporting success, and makes the invariant
vacuously true exactly when the disk is most suspect.

Root AGENTS.md already covers this shape: "An unreadable lock fails the commands
that report it." Migration and `verify_shape` report lock contents as their answer.

Not adopted: erroring on an old lock version. For the global lock that would diverge
from npx's v2-wipe behavior with no recoverable loss.

## npx `skills` coexistence (D4, specified)

Verified against `skills` v1.5.19 (`/home/audichuang/research/vercel_npx_skill`)
with live fixtures on Node v26.8.1.

| npx verb                                                        | Behavior against the new layout                                                                 | Consequence                                                          |
| --------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| `list`, folder hash                                             | Follows the symlink; digest byte-identical                                                      | Safe                                                                 |
| `experimental_sync`                                             | Discovers from `node_modules`, never rebuilds from lock                                         | Safe except on a name collision                                      |
| `remove`                                                        | `scanDir` (`remove.ts:45`) filters on `isDirectory()`, false for symlinks                       | **Silent no-op**, exit 0, "No skills found to remove"                |
| `add` / `install` / `update` / `check` / `experimental_install` | `cleanAndCreateDirectory` (`installer.ts:359`) unlinks the Referrer and writes a real directory | **Cancels the withhold and forks the content**, no error either side |

`check` is an alias for `update`, not a read. `experimental_install` is the
lock-driven wholesale rebuild.

**No direct data loss from npx**: `fs.rm` dispatches on `lstat`, so it unlinks the
Referrer without descending into `.aghub`. The loss risk is ours.

Also note: npx's `addSkillToLock` (`skill-lock.ts:205-221`) writes
`lock.skills[name] = { ...entry, installedAt, updatedAt }` — a whole-entry
overwrite. **Any npx write already wipes aghub's `contentHash` and `refCommit`.**
That is pre-existing, and it is why no aghub key can be added to the npx lock.

### Policy

1. **`verify_shape` at the decision point, per skill** (above). Without it,
   `apply-update` / `source sync` write to `.aghub/<name>` that the shared-slot
   readers no longer read and **report success with a fresh lock hash while
   delivering nothing to any agent**. That failure cannot happen today, because
   what npx overwrites _is_ the Master.

2. **Repair shape only, never content, and never by deletion.**
   The comparison is **tri-state**: `Equal | Diverged | Undecidable(HashError)`.
   `Undecidable` routes to the same refuse-and-disclose path as `Diverged` — two
   `Err`s must never compare equal.

    Hash equality is **not** content equality. `compute_skill_folder_hash`
    (`crates/skill/src/hash.rs`) is npx-parity by design: it skips symlinks
    (`:120-122`), skips `.git` and `node_modules` (`:124-126`), never records empty
    directories, and hashes no mode or permission data (`:61-69`). An npx fork can
    hash equal while exclusively holding a user's `.git/`, a `node_modules/` tree,
    symlinks, or empty dirs.

    So the `Equal` branch **quarantines by rename**, never `remove_dir_all`:
    `.aghub/.quarantine/<name>-<stamp>/`. That target is structurally invisible to
    the store scan — `top_level_skill_dirs` (`crates/core/src/skills/prune.rs:464-481`)
    is one level deep and tests `entry.path().join("SKILL.md").is_file()`, and
    `.quarantine` has no `SKILL.md` at its own root. Keep both constraints: the
    quarantine dir is dot-prefixed with no root `SKILL.md`, and store scans stay one
    level deep.

3. `~/.agents/` **is never removed.** The shared global lock lives at
   `~/.agents/.skill-lock.json` (or `$XDG_STATE_HOME/skills/.skill-lock.json`) —
   `crates/skill/src/lock/io.rs:20-31`, upstream `skill-lock.ts:67-73`. Deleting a
   seemingly-empty `~/.agents` wipes it, and upstream `readSkillLock` returns an
   empty lock instead of erroring, so the loss presents as "no skills tracked".
   The `npx-skills-contract` skill records this path wrongly as `~/.skill-lock.json`
   and must be corrected.

## Non-goals — the floor this cannot lift

For agents whose only skills directory **is** `.agents/skills`, per-agent targeting
is impossible: their directory is the shared one. `.aghub` buys "withhold from all
of them", never "give to one".

- global: cline, warp (2)
- project: codex, cline, warp, antigravity, copilot, gemini, amp, kimi (8)

Also unchanged: amp and kimi share `~/.config/agents/skills` at global — a second
shared slot with the same property.

This is an agent-side constraint. Do not design around it; disclose it.

## Migration

Skill worklist from the lock (D5), Referrer worklist derived (D6), and:

- **Transactional.** Copy-on-symlink-failure is banned (`linker/mod.rs:1-4`), so
  there is no degraded mode. If a Referrer cannot be created, the Master must not be
  moved out from under the real directory currently serving it. Today a symlink
  failure for a native reader was harmless; after this change it is total skill loss
  for that agent.
- **Runs outside `verify_shape`** — its input state is `legacy` by construction.
- **Refuses to touch foreign real directories** (D5). Report them; never move or
  delete.
- Emits a **receipt** recording what it linked. Without it, "withheld on purpose"
  and "migration died before linking" are the same observation (both are `absent`),
  which is the one gap the derived Referrer set cannot close.

### Where it runs

**Delete revision 1's "called thinly from both startups". Neither surface has such
a startup.** The CLI's `main()` is `Cli::parse()` then `run(cli)`
(`crates/cli/src/main.rs:694-706`); a call at the top of `run` makes `check`,
`doctor`, `get` and `coverage` take the interprocess mutation lock and write —
contradicting AGENTS.md ("read paths are deliberately unlocked"), `check.rs:496-500`'s
own read-only premise for the `--write-result` guard, and doctor's read-only doc.

The seam is the **`core::skills::lock::mutation_guard` call sites**
(`crates/core/src/skills/lock.rs:58`) — verified as `install_fetched.rs:402`,
`rename.rs:295`, `prune.rs:134`, `resync.rs:125`, `transfer.rs:1851`/`:1906`,
`api/routes/skills.rs:351`/`:1131`. Not `guard_and_reload`: that covers only
`ConfigManager` mutations and misses fetched install plus two API routes.

For an api-only host (`crates/remote`'s VM, any standalone `aghub-api`), the hook
is `aghub_api::start()` (`crates/api/src/lib.rs:366`), before `rocket.launch()`.

**Version skew is unresolved.** A migrated Mac driving an un-migrated VM whose
`aghub-api` is older (`bringup.rs`'s `is_version_compatible`) has no defined
behavior. Decide before shipping remote support.

### Link target

The project-scope shared Referrer `<root>/.agents/skills/<n> -> ../../.aghub/<n>`
is the first link aghub creates pointing **out** of `.agents/skills`.
`relative_path` (`linker/mod.rs:106-135`) is purely lexical, so a symlinked
`.agents` parent dangles it — the failure AGENTS.md bans hand-rolled normalization
for, now on the **write** path.

Resolve the `from` directory before computing the relative target, or use
`LinkTarget::Absolute`. Windows junctions resolve absolute regardless.

### `.gitignore`

`.aghub` is a new top-level directory in every user project that gets a
project-scope install. Decide whether install writes a `.gitignore` entry; a silent
new dot-directory in someone's repo root is a support ticket. This repo's
`.gitignore` carries only `.agents/.aghub-mutation.lock`.

Note that `.agents` itself cannot be used as a "was anything granted" assertion
target — the project mutation lock creates it.

## Acceptance

The spec's central promise gets one test, in `crates/cli/tests/cli_tests.rs`, using
the existing `isolated_cli(home, state)` harness.

1. `aghub -g skills add <local path> -a grok`
2. `home/.aghub/<n>/SKILL.md` exists and `symlink_metadata` says real dir
3. `home/.grok/skills/<n>` is a symlink whose `canonicalize` equals
   `canonicalize(home/.aghub/<n>)`
4. **`home/.agents/skills/<n>` does not exist** — assert via
   `symlink_metadata().is_err()`, never `exists()`, which returns false for a
   dangling symlink and would pass a broken link as success
5. codex / cursor / opencode / cline / warp global read dirs contain no `<n>`

**Line 4 is the proof.** Revert the change and it goes red — today's
`install_universal` always materializes `~/.agents/skills/<n>`. Lines 2, 3 and 5
can be green before the change and prove nothing on their own.

Use the CLI harness, not in-process. `isolated_cli` already sets
HOME/USERPROFILE/APPDATA/XDG_STATE_HOME and calls `clear_agent_home_overrides`
(11 agent-specific vars). Revision 1's "we need a hook for `set_skills_path_override`"
is wrong twice: it is a thread_local holding **one** `(agent_id, path)` pair, so it
could never isolate the six agents this assertion observes. If an in-process test is
unavoidable, lift `clear_agent_home_overrides`' list to a shared constant and clear
it via `EnvVarGuard` under the same binary's env lock.

One residue that cannot be isolated: codex's global read path hard-codes
`/etc/codex/skills`. Use a fixture skill name that cannot collide
(e.g. `aghub-withhold-fixture`); no further mechanism is needed.

Second required test — preview/commit parity: seed a lock entry `alpha`, create
`~/.aghub/alpha/SKILL.md`, then make `~/.agents/skills/alpha` a **real directory**
(the npx-`add` shape). Run `aghub -g --json delete skills alpha` **without**
`--yes`. Assert stdout JSON is the shape-refusal `error.code` and exit 1. Moving the
check back onto the guard turns this red — the preview reverts to exit 0 with
`outcome: "preview"`.

For genuine mid-flight failure coverage use the repo's existing
`testing-fs-failures` ENOTDIR technique (make an agent's skills dir a file), which
is root-safe. Do not use `chmod`.

## Blast radius

178 impact sites (52 blocker / 88 required / 38 cosmetic); **104 fail silently**.
103 affected tests, **65 becoming false greens**. Inventory in
`discovery-findings.json`.

The two invisible to the compiler:

- `LinkNeed::NativeReader` becomes **unreachable** — nothing reads `.aghub`. It is
  still constructed in source, so there is no dead-code warning and all eight
  `matches!` arms silently die. **Delete the variant** so the compiler enumerates
  every call site as a decision.
- `reads_master` / `writes_master` / `auto_covered` become permanently `false` on the
  coverage wire, so the desktop's `autoCovered` bucket
  (`agent-capabilities.ts:74-100`) renders three constants as facts. Remove or
  redefine against the Referrer dir; regenerate the ts-rs DTO.

Any test asserting a `NativeReader` outcome is a false green after the change.

## Rejected alternatives (do not reinvent)

1. **Recording the granted agent set in the npx lock.** `addSkillToLock` overwrites
   the whole entry, so npx erases it on any write; and bumping the lock version makes
   npx's `readSkillLock` (`skill-lock.ts:93-98`) return `createEmptyLockFile()` —
   every user entry vanishes at once.
2. **Hosting the shape check on either mutation guard.** Wrong crate, wrong
   granularity, self-refusing migration, and it skips every preview path.
3. **An exemption flag on guard acquisition.** Hand-mirrored per-call-site policy.
4. **`resolve_existing` as the chain test.** It canonicalizes, so a two-hop chain
   resolves through and certifies as healthy.
5. **`remove_dir_all` on a hash-equal fork.** Hash equality is not content equality,
   and under aliasing the two paths are one inode.

## Still open

- **Authorization persistence.** A sidecar would make "granted to nobody" and
  "Referrer lost" distinguishable. If added, it goes under `.aghub/` — never an
  app-data root (root AGENTS.md's three-roots trap: CLI, aghub-api and desktop
  resolve three different roots) — and never inside `.aghub/<name>/`, which npx
  folder-hashes through the symlink. Decide together with the migration receipt and
  doctor's third resting state; they are the same question.
- **Shared-slot representation** — carried on `LinkNeed::NeedsLink` as `shared_with`,
  or a `classify::shared_slots()` helper. Every consumer depends on the shape.
- **Shared-slot removal UX** — refuse pending explicit acknowledgement of the
  co-affected agents, or proceed with disclosure?
- **`capabilities.skills.universal`** — its only remaining effect is appending a
  Referrer read path. Keep, redefine, or delete?
- **Remote version skew** (above).
