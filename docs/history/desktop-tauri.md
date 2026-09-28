# desktop-tauri history

Incidents behind the Tauri shell commands in `crates/desktop/src-tauri`.

## Legacy inference db migration removed

Before every surface resolved one app data root, the desktop wrote inference
providers into Tauri's identifier-scoped dir (`<data>/com.akrc.aghub`), and
aghub used to copy that db across to the shared root on startup. The migration
was removed on purpose: the shared db has writers the desktop cannot
coordinate with — `aghub-cli inference …`, a standalone `aghub-api`, a second
desktop — so any automatic publish had a real interleaving that dropped a
provider committed by one of them (the desktop's own lock never bound them,
and a lock timeout started the API anyway). The split itself is fixed by every
surface resolving ONE root; carrying the old bytes over was a convenience, and
the convenience was the entire risk.

`legacy_inference_db_hint` now only formats a message. It keys on the legacy
file EXISTING, with no marker, so copying the file across does not silence it —
only renaming the original aside does. A notice that still fires after a copy
invites the copy a second time, which would restore the pre-upgrade list over
everything added since; the message therefore tells the user to rename.
Knowledge page `推論供應商與 app data root`. Commit `29d30bbb`.
