# release history

Incidents behind the release pipeline, `.github/workflows/release.yml`.

## macOS ad-hoc codesign

`.github/workflows/release.yml` "Build Tauri" step — commit `5968b82f`.

**Why a literal `-`, not a secret.** An unset secret expands to the empty
string, and tauri-cli reads it with `var_os(...)` -> `Some("")`, which becomes
`codesign -s ""`, which fails. That is a different failure from the one the
commented-out `APPLE_*` block used to cause: there, unset `APPLE_CERTIFICATE`
and `APPLE_CERTIFICATE_PASSWORD` became `Some("")`, so tauri ran
`security import` on an empty cert (the releasing-aghub skill's
`security import: failed to import keychain certificate` row).

**Why sign at all.** Without any identity tauri does not run codesign, and the
shipped `.app` was only linker-signed: `codesign --verify --deep --strict`
failed with "code has no resources but signature indicates they must be
present", because nothing sealed the bundle's resources. Ad-hoc signing seals
them. It is NOT notarization and NOT a Developer ID: Gatekeeper still treats
the app as unidentified on a clean Mac. It only makes the bundle internally
consistent, so tampering after download invalidates it instead of going
unnoticed.

**Local network identity.** Ad-hoc signing also makes identity tracking less
reliable for macOS local network privacy. Apple recommends an Apple-issued
signing identity; a self-signed certificate is not an equivalent guarantee.
See [the SSH incident](remote.md#macos-local-network-privacy). For distribution
outside the App Store, use `Developer ID Application` and notarization as
described in [Tauri's signing guide](https://v2.tauri.app/distribute/sign/macos/).
The updater signing key is separate from this macOS identity.

**No cert import happens.** tauri only calls `security import` when
`APPLE_CERTIFICATE` _and_ `APPLE_CERTIFICATE_PASSWORD` are both set
(tauri-bundler `macos/sign.rs::keychain`). With only an identity it takes the
`with_signing_identity` branch and runs `codesign --force -s - <target>` over
each nested binary and framework, inner to outer.

**Superseded** by Developer ID signing, below. Kept for why the literal `-`
existed and why empty `APPLE_*` secrets break the import.

## macOS Developer ID signing

`.github/workflows/release.yml` "Stage notarization API key (macOS)" and
"Build Tauri" steps — 2026-10, once the Apple Developer account existed.

**Why.** Every ad-hoc build had a different code signature, so macOS treated
each update as a new, unidentified app: the cask had to `xattr -cr` it past
Gatekeeper, and every update re-asked for the login password before the app
could read its keychain items (inference keys via the Apple keyring store).
A Developer ID signature gives a stable designated requirement (team
`37HFR5V2JJ` + bundle id), so the keychain ACL survives updates. Users coming
from an ad-hoc build get ONE more prompt ("Always Allow"), then none — seen on
v2.43.3 → v2.43.4: the upgraded app read `aghub`/`credentials` in 40 ms with
no `SecurityAgent` dialog. The ACL trusts each identity separately, so
`aghub-cli` (identifier `aghub-cli`) asks once on its own first read.

**Secrets** (repo `audichuang/aghub`): `APPLE_CERTIFICATE` (base64 `.p12`:
Developer ID Application cert + the Developer ID G2 intermediate + key),
`APPLE_CERTIFICATE_PASSWORD`, and notarization through an App Store Connect
API key (role Developer) — `APPLE_API_KEY` (key id), `APPLE_API_ISSUER`,
`APPLE_API_KEY_P8` (the `.p8` content; the staging step writes it to a file and
exports `APPLE_API_KEY_PATH`, which is what tauri reads). An API key, not
`APPLE_ID` + app-specific password: it does not depend on the Apple ID's
password or 2FA. `APPLE_TEAM_ID` is set too but tauri does not need it with an
API key. Backups of the `.p12`, its password and the `.p8` are in 1Password
(vault `Agent`, items titled `aghub …`). The cert expires **2027-11-23**:
renew it (new CSR, same steps) and replace `APPLE_CERTIFICATE` and
`APPLE_CERTIFICATE_PASSWORD` before then.

**The sidecar is signed by hand.** `aghub-api` ships through
`tauri.bundle.conf.json` `resources`, not `externalBin`, and tauri signs only
the latter. The first signed release (v2.43.1) failed notarization on it: "not
signed with a valid Developer ID certificate", no secure timestamp, no hardened
runtime. "Sign staged aghub-api (macOS)" signs it with `--options runtime
--timestamp` before "Build Tauri" seals the bundle, through
`scripts/ci-macos-sign.sh`.

**The CLI is signed and notarized too.** `aghub-cli` shares keychain items
with the desktop (inference keys). Ad-hoc, its identity changed every build,
so each `brew upgrade` re-prompted. The same script signs it and submits it
to the notary service (a bare Mach-O cannot be stapled; Gatekeeper fetches
the ticket online).

**The DMG is notarized and stapled too.** tauri notarizes and staples the
`.app`, then builds and signs the DMG without notarizing it (`spctl --type
open` on the DMG said `Unnotarized Developer ID`, v2.43.1–v2.43.3). Apple's
rule is to notarize and staple the outermost container, so a Mac can validate
the download offline. "Notarize and staple DMG (macOS)" runs
`scripts/ci-macos-sign.sh --staple-dmg` on it and replaces the draft's asset
before the generic `aghub_mac_arm.dmg` copy is made; the cask's sha comes
from the server-side digest, so it follows.

**No `APPLE_SIGNING_IDENTITY`.** tauri derives the identity from the imported
cert; leaving the old literal `-` would override it and ship ad-hoc again.

**The gate.** "Verify macOS bundle signature…" requires
`Authority=Developer ID Application:`, `TeamIdentifier=37HFR5V2JJ`, a stapled
ticket (`xcrun stapler validate`) and `spctl --assess`. A lost or expired secret
fails the release instead of silently shipping an app that prompts again. The
staging step also fails by name when `APPLE_CERTIFICATE` / `APPLE_API_KEY_P8`
are empty (the empty-string trap above).

**Cask.** The `postflight` `xattr -cr` and the "not notarized" caveat were
dropped: a notarized app needs neither.

## The tag followed a moving HEAD

`release.sh` read `HEAD_SHA` once for the CI wait, but pushed `main` by name
(seconds later, a small window) and ran the "commits to ship" log and
`git tag "$TAG"` against whatever `HEAD` was after the wait. A release waits
up to 40 minutes for CI, and on 2026-10-21 another session in the same clone
committed three local, un-pushed, un-CI'd commits onto main during the v2.41.6
wait. The tag would have landed on them, and `verify-ci` would have refused
the release. The commits were moved off main by hand in time. The push, the
"commits to ship" log and the tag now all use the captured `HEAD_SHA`.
