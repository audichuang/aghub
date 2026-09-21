# SKILL-AUDIT CRATE KNOWLEDGE BASE

**Crate**: `skill-audit` — the install-time security gate. L1 static YARA over
every file of a fetched skill + L2 prompt-injection detection over its markdown.
Consumed by `aghub-core` (`skills::audit::guard_fetched_source`), which is what
install and resync call before writing anything.

## THE ONE THING TO KNOW

**`Critical` is the only severity that REFUSES an install** (`verdict.rs`
`aggregate`: any Critical → `Malicious` → `decide` = `Block`). High / two
Mediums / any injection signal → `Suspicious`, which still installs with a
warning. So the two failure directions are NOT symmetrical:

- a rule that is too WIDE refuses somebody's legitimate skill — a deploy doc
  that writes a `.env` and curls a health endpoint was refused this way;
- a rule that is too NARROW does not lose the finding, it demotes it to a
  warning the user can click past.

Both cost real things. Judge a rule change by running BOTH directions, not just
the sample you are fixing.

## WHERE TO LOOK

| Task                       | Location                                           |
| -------------------------- | -------------------------------------------------- |
| Severity → verdict stance  | `src/verdict.rs` (`aggregate` — the paranoia knob) |
| Verdict → install/block    | `src/policy.rs` (`decide` → `Action`)              |
| Rule set (embedded)        | `rules/` — `cisco/`, `clawhub/`, `aghub/`          |
| aghub's own rules          | `rules/aghub/real_world.yara` + `dataflow.yara`    |
| Rule compile + fingerprint | `src/rules.rs` (`include_str!`, `OnceLock`)        |
| L1 scan                    | `src/engine/yara.rs` (yara-x)                      |
| L2 prompt injection        | `src/engine/injection.rs`                          |
| Report / digest            | `src/report.rs`                                    |
| Real-world detection tests | `tests/real_threats.rs`                            |

## GOTCHAS

- **Rules are `include_str!`'d into the binary.** Editing a `.yara` file needs a
  rebuild of this crate — and when you compare before/after across two git
  worktrees sharing one `CARGO_TARGET_DIR`, put a canary case in the probe whose
  verdict MUST differ; otherwise a reused test binary answers with the old rules
  and the comparison silently proves nothing.
- **`aghub_credential_source_direct` is the only evidence the Critical rule may
  act on**, and it is deliberately narrow: a property access such as
  `process.env` is not a `.env` file, and the ALIASED read (`env = os.environ`
  … `env['TOKEN']`) is uncorrelated in YARA, so it feeds the `low` dataflow
  source instead. Never wire the wide `aghub_credential_source` into a Critical
  rule.
- **A credential read must meet its path in ONE expression** — that is what
  keeps "writes `.env`, then curls something" out of Critical. The ceiling is
  accepted: a two-step read (`p = join(home, '.ssh/id_rsa')` … `open(p)`) falls
  to Suspicious. But the path itself is rarely a literal, so the verbs and the
  argument shape must cover `expanduser` / `Path.home() /` / an f-string /
  `fs.promises.readFile`; narrowing that is how real samples get demoted.
- The dataflow rules (`dataflow.yara`) carry LOW/INFO on their own and only
  matter as a source+sink pair — `engine::run()` correlates them into
  `aghub_dataflow_chain` ACROSS files, which is the multi-file exfil case single
  rules miss.
- An audit that cannot RUN (unreadable tree, rule-compilation failure) is
  **logged and treated as "not audited"**, never as a refusal — a corrupt rule
  set must not break every install.
- Every non-Benign finding is `log::warn!`ed by `guard_fetched_source` with rule
  id, severity and file, which is what makes "check Settings > Logs" a truthful
  thing to tell a user.

## ANTI-PATTERNS

- **NEVER** add a rule at `severity = "critical"` without a case in
  `tests/real_threats.rs` for BOTH a sample it must block and a benign shape it
  must not — Critical is the level that takes the install away.
- **NEVER** widen a Critical rule to catch a sample the Suspicious tier already
  reports; the user still sees it.
- **NEVER** make the rule set's behaviour depend on the file's path or name —
  the audit reads a fetched tree it does not own.
