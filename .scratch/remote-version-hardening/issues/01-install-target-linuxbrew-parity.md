# 01 — Install-target resolution lacks the probe's linuxbrew fallback

**Status:** deferred

**Where:** `crates/remote/src/ssh.rs`, `default_install_target_script`
(compare `default_api_path_script` just above it).

## The asymmetry

`default_api_path_script` (used to PROBE for / find the running `aghub-api`)
checks, in order: `command -v aghub-api` -> `$HOME/.cargo/bin/aghub-api` ->
`$HOME/.local/bin/aghub-api` -> `/home/linuxbrew/.linuxbrew/bin/aghub-api` ->
`$HOME/.linuxbrew/bin/aghub-api` -> bare `aghub-api`.

`default_install_target_script` (used to decide WHERE an install writes) only
checks: `command -v aghub-api` -> `$HOME/.cargo/bin/aghub-api` ->
`$HOME/.local/bin/aghub-api`, falling back to a concrete
`$HOME/.local/bin/aghub-api` for a fresh install. It has no linuxbrew
branches at all.

Consequence: if the PROBE resolved an existing `aghub-api` from a linuxbrew
prefix (e.g. `/home/linuxbrew/.linuxbrew/bin/aghub-api` is on `PATH` and
`command -v aghub-api` finds it — the install-target script's FIRST branch
also matches `command -v`, so this specific case is actually fine), the
install-target script is only asymmetric when `command -v` does NOT resolve
it (e.g. a non-login shell where linuxbrew's `PATH` export never ran) but the
probe's explicit linuxbrew-prefix checks still found the binary directly. In
that narrow case: the probe reports "present", but an upgrade install writes
to `$HOME/.local/bin/aghub-api` instead of overwriting the linuxbrew copy —
the old linuxbrew-resolved binary is left on disk, now SHADOWED by the new
`~/.local/bin` entry (which typically sorts earlier on `PATH` than a
linuxbrew prefix). The user ends up with two copies: a stale one that no
longer runs (shadowed), and the freshly installed one that does.

## Why this is deferred, not fixed now

This round's changes (Fix A/B/C/D) all harden the install SCRIPTS
themselves — shell-injection/TOCTOU/atomicity/tag-precedence — without
touching WHERE any install writes. Extending
`default_install_target_script` to also probe linuxbrew paths changes
install-target resolution, which is a materially riskier change than a shell
hygiene fix: it affects every existing linuxbrew-based deployment's upgrade
path, and any bug there risks writing an install to a location the running
process doesn't expect (or duplicating installs) rather than merely
mishandling an edge-case failure. That risk is out of proportion to the bug
it fixes — a shadowed, no-longer-executed stale binary sitting on disk is a
cosmetic disk-usage papercut, not a data-safety or functional regression (the
freshly installed binary still resolves and runs correctly via `PATH`/probe).

## Suggested fix (when picked up)

Mirror the two linuxbrew branches from `default_api_path_script` into
`default_install_target_script`, in the same order, so an existing
linuxbrew-resolved binary is upgraded in place instead of shadowed. Add a
test asserting `default_install_target_script`'s branch set is a superset
of (or identical to, minus the final generic fallback)
`default_api_path_script`'s, so future drift between the two is caught
immediately rather than rediscovered as another asymmetry.
