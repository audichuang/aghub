#!/usr/bin/env bash
#
# Verify one release's artifacts. The ONE definition of "this release is
# shippable", used in two places so they cannot drift:
#
#   --pre-publish   release.yml's `publish` job, against the still-DRAFT
#                   release, before anything is visible to users. A failure
#                   here leaves the release a draft: the updater keeps
#                   serving the previous version instead of a 404 or a
#                   half-populated latest.json.
#   (default)       scripts/release.sh after the run, against the published
#                   release: everything above, plus "is it really live" —
#                   releases/latest, the updater endpoint and the Homebrew tap.
#
# Reads everything through the authenticated API (works on drafts; a draft's
# public download URLs 404). Checksums come from GitHub's own per-asset
# `digest`, computed server-side from the uploaded bytes — no downloads.
#
# Usage: scripts/verify-release.sh vX.Y.Z[-rc.N] [--pre-publish]
set -euo pipefail

REPO="${GITHUB_REPOSITORY:-audichuang/aghub}"
TAP="audichuang/homebrew-tap"
TAG="${1:?usage: verify-release.sh vX.Y.Z [--pre-publish]}"
MODE="${2:-}"
TAG="v${TAG#v}"
VERSION="${TAG#v}"
# A pre-release (vX.Y.Z-rc.N) ships NSIS only on Windows (WiX rejects the
# identifier), is never "latest", and never touches Homebrew.
STABLE=1
case "$VERSION" in *-*) STABLE=0 ;; esac
# sha256 of zero bytes: what a failed download hashes to. It has shipped to the
# tap once already (v2.3.0), so it is refused by name, not just by shape.
EMPTY_SHA="e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"

FAILED=0
fail() {
	printf '\033[31m✗ %s\033[0m\n' "$*" >&2
	FAILED=1
}
ok() { printf '\033[32m✓ %s\033[0m\n' "$*"; }

# /releases/tags/<tag> for a published release; it does not return drafts,
# so fall back to the newest releases (a draft being verified is always one).
REL="$(gh api "repos/$REPO/releases/tags/$TAG" 2>/dev/null ||
	gh api "repos/$REPO/releases?per_page=30" \
		--jq "[.[] | select(.tag_name == \"$TAG\")] | first")"
if [ -z "$REL" ] || [ "$REL" = "null" ]; then
	fail "no release for $TAG in $REPO"
	exit 1
fi
ASSETS="$(jq -r '.assets[].name' <<<"$REL")"

# digest_of <asset name> -> bare hex sha256, or "" when absent.
digest_of() {
	jq -r --arg n "$1" \
		'.assets[] | select(.name == $n) | (.digest // "") | sub("^sha256:"; "")' <<<"$REL"
}

# ── assets ───────────────────────────────────────────────────────────────────
need=(
	latest.json
	"aghub_${VERSION}_aarch64.dmg"
	"aghub_${VERSION}_x64.dmg"
	aghub-cli-aarch64-apple-darwin.tar.gz
	aghub-cli-x86_64-apple-darwin.tar.gz
	aghub-cli-x86_64-unknown-linux-gnu.tar.gz
	aghub-cli-x86_64-pc-windows-msvc.zip
	aghub-api-x86_64-unknown-linux-gnu.tar.gz
)
for name in "${need[@]}"; do
	grep -qxF "$name" <<<"$ASSETS" || fail "missing asset $name"
done
suffixes=('\.AppImage$' 'setup\.exe$')
[ "$STABLE" = 1 ] && suffixes+=('\.msi$')
for re in "${suffixes[@]}"; do
	grep -qE "$re" <<<"$ASSETS" || fail "missing asset matching $re"
done
[ "$FAILED" = 0 ] && ok "all required assets present ($(wc -l <<<"$ASSETS") total)"

# ── latest.json: what every installed app's updater reads ───────────────────
LJ_ID="$(jq -r '.assets[] | select(.name == "latest.json") | .id' <<<"$REL")"
if [ -n "$LJ_ID" ]; then
	LJ="$(gh api -H 'Accept: application/octet-stream' "repos/$REPO/releases/assets/$LJ_ID")"
	jq -e --arg v "$VERSION" '.version == $v' >/dev/null <<<"$LJ" ||
		fail "latest.json version is $(jq -r .version <<<"$LJ"), want $VERSION"
	# The four desktop jobs each read-modify-write this one file; two that
	# interleave drop a platform with every job still green. A missing key is
	# an updater told "no update" forever, so it blocks the publish.
	for plat in darwin-aarch64 darwin-x86_64 linux-x86_64 windows-x86_64; do
		jq -e --arg p "$plat" '(.platforms[$p].signature // "") | length > 0' \
			>/dev/null <<<"$LJ" ||
			fail "latest.json has no signed '$plat' entry — rerun that platform's Build Desktop job"
	done
	# Every URL must name THIS tag: a draft's `untagged-…` URL, another
	# repo, or another version would break the download after publishing.
	bad="$(jq -r --arg p "https://github.com/$REPO/releases/download/$TAG/" \
		'.platforms[].url | select(startswith($p) | not)' <<<"$LJ")"
	[ -z "$bad" ] || fail "latest.json URLs outside $TAG: $bad"
	[ "$FAILED" = 0 ] && ok "latest.json: $VERSION, 4 platforms signed, URLs on $TAG"
fi

if [ "$MODE" = "--pre-publish" ]; then
	[ "$FAILED" = 0 ] || exit 1
	ok "draft $TAG is ready to publish"
	exit 0
fi

# ── published state ──────────────────────────────────────────────────────────
[ "$(jq -r .draft <<<"$REL")" = "false" ] || fail "$TAG is still a draft"
LATEST_TAG="$(gh api "repos/$REPO/releases/latest" --jq .tag_name 2>/dev/null || true)"
if [ "$STABLE" = 1 ]; then
	[ "$LATEST_TAG" = "$TAG" ] || fail "releases/latest is ${LATEST_TAG:-none}, want $TAG"
	# The exact URL the updater requests (tauri.conf.json endpoints).
	LIVE="$(curl -fsSL "https://github.com/$REPO/releases/latest/download/latest.json" |
		jq -r .version 2>/dev/null || true)"
	[ "$LIVE" = "$VERSION" ] || fail "updater endpoint serves ${LIVE:-nothing}, want $VERSION"
else
	[ "$LATEST_TAG" != "$TAG" ] || fail "pre-release $TAG is releases/latest — stable users would get it"
	[ "$FAILED" = 0 ] || exit 1
	ok "pre-release $TAG published (not latest)"
	exit 0
fi

# ── Homebrew: version AND checksum must match THIS release ──────────────────
# Checking only that "some sha256 is there" passes on the previous release's
# cask too, i.e. when the tap was never updated at all.
tap_file() {
	gh api "repos/$TAP/contents/$1" --jq .content | base64 -d
}
# check_sha <file text> <label> <asset name>
check_sha() {
	local want
	want="$(digest_of "$3")"
	if [ -z "$want" ] || [ "$want" = "$EMPTY_SHA" ]; then
		fail "no usable digest for $3"
	elif ! grep -q "\"$want\"" <<<"$1"; then
		fail "$2 does not carry the sha256 of $3 ($want)"
	fi
}
CASK="$(tap_file Casks/aghub.rb)"
FORMULA="$(tap_file Formula/aghub-cli.rb)"
# No `${var,,}`: this also runs under macOS's /bin/bash 3.2.
for label in cask formula; do
	if [ "$label" = cask ]; then text="$CASK"; else text="$FORMULA"; fi
	grep -qE "^ *version \"$VERSION\"" <<<"$text" || fail "tap $label is not version $VERSION"
	if grep -q "$EMPTY_SHA" <<<"$text"; then fail "tap $label carries the empty-file sha256"; fi
done
check_sha "$CASK" cask "aghub_${VERSION}_aarch64.dmg"
check_sha "$CASK" cask "aghub_${VERSION}_x64.dmg"
for t in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu; do
	check_sha "$FORMULA" formula "aghub-cli-$t.tar.gz"
done

[ "$FAILED" = 0 ] || exit 1
ok "$TAG is live: releases/latest, updater endpoint and Homebrew tap all at $VERSION"
