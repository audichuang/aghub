# Where the skill security audit gates, and how it is overridden

> **Superseded 2026-09-22 — the gate and the `skill-audit` crate were removed
> entirely.** Nothing audits fetched skill content now; `--force-unsafe` and
> `ResyncError::Audit` are gone with it.
>
> Why, in one line: a byte-pattern rule set cannot separate an ordinary install
> instruction from an attack. `curl … | sh` appears verbatim in vendors' own
> documentation, an `os.environ.get("…API_KEY")` beside an `import urllib` was
> enough for `Critical` with no request anywhere in the file, and a `Critical`
> finding was wired straight through to a refusal — so the gate blocked real
> updates that were not malicious. Meanwhile reading the same secret through a
> variable alias dropped to `Suspicious` and installed. Widening the rules costs
> more false refusals; narrowing them lets the equivalent attack through. That
> trade is inherent to matching text, not a tuning problem.
>
> The decision below is kept for its reasoning and its rejected alternatives —
> they are the argument against re-porting upstream's `1b510d77`, not a
> description of current behaviour (`UPSTREAM.md`, ⏭️ Skipped).

`skill-audit` runs on exactly two paths, both in `aghub-core`:
`install_fetched_skill_and_lock` and `resync_installed_skill`, through the one
entry point `skills::audit::guard_fetched_source`. A `Malicious` verdict is a
refusal (`ValidationFailed` / `VALIDATION_FAILED`); `Suspicious` installs with
every finding logged at warn level; an audit that cannot RUN is logged and
treated as "not audited", never as a refusal.

## Considered Options

- **Gate the install only.** Rejected. Publish something benign, wait for
  installs, then push a malicious update is the shape the bundled cisco rules
  are written for, and that update reaches `resync_installed_skill` without ever
  touching the install path. One gate would have looked complete and covered
  half the threat.
- **Gate every skill-writing path, including `add --from <dir>` and
  `transfer`/`reconcile`.** Rejected. `--from` is a directory the user pointed
  at themselves, and `transfer` copies content already granted to another agent,
  so both would re-audit bytes that already passed on the way in — and every
  agent-to-agent copy would pay for it. The threat model is content we FETCHED.
- **Gate at `skills::linker::install_universal` (the shared materializer).**
  Rejected: it has no request context to carry an override, and it is reached by
  the local paths above as well.
- **Override via an environment variable (`AGHUB_SKILL_AUDIT=off`).** Rejected
  even though it needed no signature changes. A security bypass in the
  environment is exported once and then permanently global, and Tauri does not
  reliably inherit a shell environment — the CLI and the desktop would disagree
  about whether the gate is on. The override is a request field, surfaced as
  `--force-unsafe` on `source sync` and `apply-update`.
- **Fail closed when the audit cannot run.** Rejected: a rule set that fails to
  compile would break every install, and the tree is about to be read by the
  install itself, which reports its own IO errors with far better context.

## Consequences

- `add` has NO `--force-unsafe`: its paths are not gated, so the flag would be
  dead. Do not "add it for consistency".
- The API always passes `force_unsafe: false`. The desktop has no "install
  anyway" affordance yet, so until it does, a reviewed false positive is
  installable only from the CLI. Upstream's `4aff485a` is the UI that closes
  this; it was not ported.
- `AuditInput` is built from `skill::collect_skill_files`, the folder-hash's own
  traversal. The bounds and the skip-symlinks rule therefore have one
  definition — do not give the auditor a second opinion about what a skill
  folder contains.
- `yara-x` is a git-rev dependency that pulls in wasmtime + cranelift. It is on
  the critical path of all three CI platforms.
