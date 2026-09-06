# Where the skill security audit gates, and how it is overridden

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
