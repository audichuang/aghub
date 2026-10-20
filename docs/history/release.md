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

**Moving to real Apple certs + notarization.** Once an Apple Developer account
exists, add these env entries to the step and DROP the literal
`APPLE_SIGNING_IDENTITY: "-"` (the cert's own identity replaces it):

```yaml
APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
APPLE_ID: ${{ secrets.APPLE_ID }}
APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}
APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
```

## The tag followed a moving HEAD

`release.sh` read `HEAD_SHA` once for the CI wait, then ran `git push fork
main` and `git tag "$TAG"` against whatever `HEAD` was by then. A release waits
up to 40 minutes for CI, and on 2026-10-21 another session in the same clone
committed three local, un-pushed, un-CI'd commits onto main during the v2.41.6
wait. The tag would have landed on them, and `verify-ci` would have refused
the release. The commits were moved off main by hand in time. The push, the
"commits to ship" log and the tag now all use the captured `HEAD_SHA`.
