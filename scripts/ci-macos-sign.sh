#!/usr/bin/env bash
# Sign a standalone Mach-O with the release Developer ID (hardened runtime +
# secure timestamp) and, with --notarize, submit it to Apple's notary service.
# Used by release.yml for what tauri does not sign itself: the aghub-api
# sidecar (shipped as a bundle resource) and the aghub-cli binary.
# --staple-dmg notarizes and staples a DMG tauri already signed but does not
# notarize (Apple: notarize and staple the outermost container).
# Why: docs/history/release.md#macos-developer-id-signing
#
# Usage: scripts/ci-macos-sign.sh [--notarize] <binary>
#        scripts/ci-macos-sign.sh --staple-dmg <dmg>
# Env: APPLE_CERTIFICATE (base64 p12), APPLE_CERTIFICATE_PASSWORD;
#      with --notarize also APPLE_API_KEY_P8, APPLE_API_KEY, APPLE_API_ISSUER;
#      --staple-dmg needs only the APPLE_API_* three.
set -euo pipefail

IDENTITY="Developer ID Application: Yung Chun Chuang (37HFR5V2JJ)"
TEAM_ID="37HFR5V2JJ"

notarize=false
staple_dmg=false
case "${1:-}" in
    --notarize) notarize=true; shift ;;
    --staple-dmg) staple_dmg=true; shift ;;
esac
BIN="${1:?usage: ci-macos-sign.sh [--notarize|--staple-dmg] <file>}"
test -f "$BIN" || { echo "no such file: $BIN" >&2; exit 1; }

need() { [ -n "${!1:-}" ] || { echo "$1 is not set" >&2; exit 1; }; }
if $notarize || $staple_dmg; then
    need APPLE_API_KEY_P8
    need APPLE_API_KEY
    need APPLE_API_ISSUER
fi

WORK="$(mktemp -d "${RUNNER_TEMP:-/tmp}/macos-sign.XXXXXX")"

# `|| true`: a non-zero exit must still reach the status check and the log below.
submit() {
    printf '%s\n' "$APPLE_API_KEY_P8" > "$WORK/AuthKey.p8"
    chmod 600 "$WORK/AuthKey.p8"
    out="$(xcrun notarytool submit "$1" --wait --timeout 30m --output-format json \
        --key "$WORK/AuthKey.p8" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER")" || true
    echo "$out"
    status="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))' <<< "$out" 2>/dev/null)" || true
    if [ "$status" != "Accepted" ]; then
        id="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))' <<< "$out" 2>/dev/null)" || true
        [ -n "$id" ] && xcrun notarytool log "$id" \
            --key "$WORK/AuthKey.p8" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" || true
        echo "$BIN: notarization status '$status'" >&2
        exit 1
    fi
}

if $staple_dmg; then
    trap 'rm -rf "$WORK"' EXIT
    codesign --verify --strict --verbose=2 "$BIN"
    grep -q "^TeamIdentifier=${TEAM_ID}$" <<< "$(codesign --display --verbose=2 "$BIN" 2>&1)" \
        || { echo "$BIN: not signed by team $TEAM_ID" >&2; exit 1; }
    submit "$BIN"
    xcrun stapler staple "$BIN"
    xcrun stapler validate "$BIN"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$BIN"
    exit 0
fi

need APPLE_CERTIFICATE
need APPLE_CERTIFICATE_PASSWORD

KC="$WORK/signing.keychain-db"
ORIG_KEYCHAINS=()
while IFS= read -r line; do
    ORIG_KEYCHAINS+=("$(echo "$line" | tr -d ' "')")
done < <(security list-keychains -d user)
cleanup() {
    security list-keychains -d user -s "${ORIG_KEYCHAINS[@]}" || true
    security delete-keychain "$KC" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

KC_PASS="$(uuidgen)"
printf '%s' "$APPLE_CERTIFICATE" | base64 --decode > "$WORK/cert.p12"
security create-keychain -p "$KC_PASS" "$KC"
security set-keychain-settings -lut 3600 "$KC"
security unlock-keychain -p "$KC_PASS" "$KC"
security import "$WORK/cert.p12" -k "$KC" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
rm -f "$WORK/cert.p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KC_PASS" "$KC" > /dev/null
# The chain to Apple's root resolves through the search list, not --keychain.
security list-keychains -d user -s "$KC" "${ORIG_KEYCHAINS[@]}"

codesign --force --options runtime --timestamp --keychain "$KC" -s "$IDENTITY" "$BIN"
codesign --verify --strict --verbose=2 "$BIN"
info="$(codesign --display --verbose=2 "$BIN" 2>&1)"
grep -q "^TeamIdentifier=${TEAM_ID}$" <<< "$info" \
    || { echo "$BIN: TeamIdentifier is not $TEAM_ID" >&2; echo "$info" >&2; exit 1; }
grep -q '^Timestamp=' <<< "$info" \
    || { echo "$BIN: signature has no secure timestamp" >&2; exit 1; }

if $notarize; then
    # A bare Mach-O cannot carry a stapled ticket; Gatekeeper fetches it online.
    ditto -c -k --keepParent "$BIN" "$WORK/submit.zip"
    submit "$WORK/submit.zip"
fi
