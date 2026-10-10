# One update verdict owns the local baseline

`crates/skill-update/src/verdict.rs` is the one place that decides a locked
skill's local baseline and the verdict against an upstream folder. It owns the
baseline discovery (one scan of the managed agents, plus the withheld-Master
fallback), the placeholder rule, the one-walk raw and comparison digests, and the
precedence between a local copy and the lock's stored hash. `check` maps the
verdict to `SkillUpdateStatus`: `Ambiguous` and `Uncheckable` both become
`Uncheckable{local}`. `source diff` maps the same verdict in its own vocabulary
(#46). Both decisions below were made by the maintainer on 2026-10-07 (#27).

## Decisions

- **Decision 1 (unknown or placeholder lock hash).** The lock offers no baseline.
  With a readable local copy (an agent copy or a withheld Master), the verdict
  compares that copy against upstream, and check heals the lock with the raw
  local hash as before. With no readable copy, or with disagreeing copies, the
  result is `Uncheckable(Local)`, never "current". The sources test
  `classify_unknown_lock_hash_as_current` contradicts this and is rewritten in
  #46, not here.
- **Decision 2 (disabled agents).** Only managed (enabled) agents' copies form the
  baseline and the ambiguity check. A Master that no managed agent links, including
  one only a disabled agent still reads, is hashed from the store. `apply-update`
  replaces that Master, so it is the installed copy the user is acting on.

## Considered Options

- **Two baselines, one for check and one for diff.** Rejected: check and diff
  disagreed on the same skill (#23).
- **Count disabled agents in the baseline.** Rejected: a copy the user chose not
  to manage would make rows uncheckable or ambiguous.
- **Treat a placeholder lock hash as uncheckable even with a readable copy.**
  Rejected: check would stop healing placeholder hashes, a behaviour change nobody
  asked for.

## Consequences

- A reader who sees a disabled agent's copy ignored by check should not "fix" that;
  it is decision 2.
- The verdict is pinned by `crates/skill-update/tests/verdict_table.rs`, which runs
  each row through both `judge` and the check adapter. Reverting the withheld-Master
  fallback, the missing-comparison-hash veto, or the managed-agent filter turns
  specific rows red.
