#!/usr/bin/env bash
#
# Cut a release of this aghub fork. You pick the version; everything else is
# automated. Encodes the project-specific gotchas so they can't be fumbled:
#   - pushes to `fork` (origin = upstream AkaraChen/aghub — NEVER release there)
#   - refuses to tag unless the HEAD commit's ci.yml is green on the fork
#   - auto-reruns once on a transient CI dispatch flake (jobs stuck "queued")
#   - verifies the published artifacts after the build goes green
#
# Usage:
#   scripts/release.sh <X.Y.Z> [--yes]      # or: just release X.Y.Z [--yes]
#
# The version + go/no-go are deliberately a human decision (a tag triggers a
# real public release: Homebrew + the auto-update endpoint). This script only
# automates the mechanical steps *after* you've decided. See the releasing-aghub
# skill for the full reference and the re-release-a-botched-tag flow.
set -euo pipefail

REPO="audichuang/aghub" # the fork; every gh call targets it explicitly
REMOTE="fork"           # push tags here — origin is upstream, do NOT push there

err() { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; }
ok() { printf '\033[32m✓ %s\033[0m\n' "$*"; }
info() { printf '\033[36m• %s\033[0m\n' "$*"; }

# Watch a workflow run to completion, then succeed/fail on its REAL conclusion.
# `gh run watch --exit-status` can exit non-zero on a transient API hiccup (seen
# in the wild: HTTP 401 fetching annotations) while the run is perfectly fine —
# so its exit code is NOT a reliable "did the run fail" signal. Instead: let
# watch block, but after it returns, re-check the run's actual status; if it is
# not terminal yet (watch bailed early), wait and re-watch. Only the run's own
# `conclusion` decides the return value. seq is a generous backstop — normally
# `gh run watch` blocks for the whole run and this loops just once.
watch_run() {
	run_id="$1"
	for _ in $(seq 1 120); do
		gh run watch "$run_id" --repo "$REPO" >/dev/null 2>&1 || true
		st="$(gh run view "$run_id" --repo "$REPO" --json status --jq .status 2>/dev/null || echo "")"
		if [ "$st" = "completed" ]; then
			cc="$(gh run view "$run_id" --repo "$REPO" --json conclusion --jq .conclusion 2>/dev/null || echo "")"
			if [ "$cc" = "success" ]; then return 0; else return 1; fi
		fi
		sleep 15
	done
	err "watch_run: timed out waiting for run $run_id to reach a terminal state"
	return 1
}

VERSION="${1:-}"
ASSUME_YES=0
[ "${2:-}" = "--yes" ] && ASSUME_YES=1

# 1. validate the version ----------------------------------------------------
[ -n "$VERSION" ] || {
	err "usage: scripts/release.sh <X.Y.Z> [--yes]"
	exit 1
}
VERSION="${VERSION#v}" # tolerate a leading v
echo "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || {
	err "version must be X.Y.Z with no leading zeros (got: $VERSION)"
	exit 1
}
TAG="v$VERSION"

# the fork remote must really be the fork, not the upstream
REMOTE_URL="$(git remote get-url "$REMOTE" 2>/dev/null || true)"
echo "$REMOTE_URL" | grep -q "audichuang/aghub" || {
	err "remote '$REMOTE' is not audichuang/aghub (got: ${REMOTE_URL:-missing}). Refusing."
	exit 1
}

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
[ "$BRANCH" = "main" ] || {
	err "not on main (on: $BRANCH)"
	exit 1
}
git diff --quiet && git diff --cached --quiet || {
	err "working tree is dirty — commit or stash first"
	exit 1
}

# tag must not already exist (locally or on the fork)
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null 2>&1 ||
	git ls-remote --tags "$REMOTE" "$TAG" 2>/dev/null | grep -q "$TAG"; then
	err "tag $TAG already exists. To re-release a botched tag, delete it first (see releasing-aghub)."
	exit 1
fi

# must be strictly newer than the latest existing tag
LATEST="$(git tag --list 'v*' --sort=-v:refname | head -1)"
if [ -n "$LATEST" ]; then
	HIGHEST="$(printf '%s\n%s\n' "${LATEST#v}" "$VERSION" | sort -V | tail -1)"
	if [ "$VERSION" = "${LATEST#v}" ] || [ "$HIGHEST" != "$VERSION" ]; then
		err "$TAG is not newer than the latest tag $LATEST"
		exit 1
	fi
fi
ok "version $TAG validated (latest was ${LATEST:-none})"

# 2. push HEAD and wait for ci.yml to be green -------------------------------
HEAD_SHA="$(git rev-parse HEAD)"
info "ensuring HEAD ($(git rev-parse --short HEAD)) is on $REMOTE/main..."
git push "$REMOTE" main

info "waiting for ci.yml on this commit to finish (must be green before tagging)..."
# Match the fork's main *push* run for THIS sha — not a PR run or an older run
# that happens to share the commit. CI_OK is asserted after the loop so a run
# that never registers can't fall through and let us tag without a green CI.
CI_OK=0
for _ in $(seq 1 60); do
	RUN="$(gh run list --repo "$REPO" --workflow ci.yml --limit 30 \
		--json headSha,headBranch,event,status,conclusion \
		--jq "[.[] | select(.headSha==\"$HEAD_SHA\" and .headBranch==\"main\" and .event==\"push\")] | first" 2>/dev/null || echo "")"
	if [ -n "$RUN" ] && [ "$RUN" != "null" ]; then
		STATUS="$(echo "$RUN" | jq -r .status)"
		CONCL="$(echo "$RUN" | jq -r .conclusion)"
		if [ "$STATUS" = "completed" ]; then
			[ "$CONCL" = "success" ] || {
				err "ci.yml for HEAD concluded '$CONCL' — fix it before releasing"
				exit 1
			}
			CI_OK=1
			break
		fi
		info "  ci.yml: $STATUS ..."
	else
		info "  no ci.yml run registered yet; waiting..."
	fi
	sleep 20
done
[ "$CI_OK" = "1" ] || {
	err "timed out waiting for ci.yml to go green on HEAD"
	exit 1
}
ok "ci.yml is green for HEAD"

# 3. confirm -----------------------------------------------------------------
echo
info "Commits to ship in $TAG (since ${LATEST:-start}):"
git log --oneline "${LATEST:+$LATEST..}HEAD"
echo
if [ "$ASSUME_YES" != "1" ]; then
	[ -t 0 ] || {
		err "non-interactive shell — re-run with --yes to confirm automatically"
		exit 1
	}
	printf "Tag and release %s? [y/N] " "$TAG"
	read -r ANS
	case "$ANS" in
	y | Y | yes | YES) ;;
	*)
		err "aborted"
		exit 1
		;;
	esac
fi

# 4. tag + push to the fork --------------------------------------------------
# Record the highest existing release-run id BEFORE pushing the tag. databaseId
# is monotonic, so the run our push triggers is the first one with a strictly
# greater id. Without this, a re-release (delete tag + re-push) races: a stale
# run for the same tag/sha still lingers in the list and `first` locks onto it
# before the fresh run registers — which is exactly how an earlier test
# mistakenly watched (and tried to rerun) a cancelled previous run.
PRIOR_RUN_ID="$(gh run list --repo "$REPO" --workflow release.yml --limit 1 \
	--json databaseId --jq '.[0].databaseId // 0' 2>/dev/null || echo 0)"

git tag "$TAG"
git push "$REMOTE" "$TAG"
ok "pushed $TAG to $REMOTE — release.yml triggered"

# 5. watch release.yml; auto-rerun once on a transient dispatch flake --------
# Poll for the run THIS push triggered: same head sha + push event + a newer id
# than any that existed before the push. Actions can take a while to register
# under load, so keep polling rather than accepting the first stale match.
RUN_ID=""
for _ in $(seq 1 20); do
	RUN_ID="$(gh run list --repo "$REPO" --workflow release.yml --limit 15 \
		--json databaseId,headSha,event \
		--jq "[.[] | select(.headSha==\"$HEAD_SHA\" and .event==\"push\" and (.databaseId > $PRIOR_RUN_ID))] | sort_by(.databaseId) | last | .databaseId" 2>/dev/null || echo "")"
	[ -n "$RUN_ID" ] && [ "$RUN_ID" != "null" ] && break
	info "  waiting for release.yml run to register..."
	sleep 6
done
[ -n "$RUN_ID" ] && [ "$RUN_ID" != "null" ] || {
	err "could not find the release run for $TAG after waiting"
	exit 1
}
info "release run: $RUN_ID — watching to completion..."

if ! watch_run "$RUN_ID"; then
	# The RUN itself reached a non-success terminal state (watch_run already
	# confirmed it is terminal — a transient watch API hiccup alone cannot land
	# us here). Distinguish a transient dispatch flake (jobs stuck
	# "queued"/cancelled, nothing concluded "failure") from a real test/build
	# failure (a job whose conclusion is "failure"). Only the former is worth an
	# auto-rerun — rerunning a real failure just wastes ~20 min reproducing it.
	PUBLISHED="$(gh release view "$TAG" --repo "$REPO" --json url --jq .url 2>/dev/null || true)"
	REAL_FAIL="$(gh run view "$RUN_ID" --repo "$REPO" --json jobs \
		--jq '[.jobs[] | select(.conclusion=="failure")] | length' 2>/dev/null || echo 0)"
	if [ -n "$PUBLISHED" ]; then
		err "release run failed but a partial release exists — see the re-release flow in releasing-aghub."
		exit 1
	elif [ "${REAL_FAIL:-0}" != "0" ]; then
		err "release run failed: a job actually failed (not a dispatch flake)."
		err "Inspect: gh run view $RUN_ID --repo $REPO --log-failed"
		exit 1
	else
		err "release run failed with no job concluding 'failure' (transient dispatch flake) — rerunning once..."
		gh run rerun "$RUN_ID" --repo "$REPO" || {
			err "could not rerun $RUN_ID — re-check manually: gh run view $RUN_ID --repo $REPO"
			exit 1
		}
		sleep 8
		if ! watch_run "$RUN_ID"; then
			err "rerun failed too. Inspect: gh run view $RUN_ID --repo $REPO --log-failed"
			exit 1
		fi
	fi
fi
ok "release.yml succeeded"

# 6. verify the artifacts ----------------------------------------------------
info "verifying artifacts for $TAG..."
VERIFY_FAILED=0
ASSETS="$(gh release view "$TAG" --repo "$REPO" --json assets --jq '.assets[].name')"
echo "$ASSETS" | sed 's/^/    /'
for must in latest.json .dmg .AppImage setup.exe; do
	echo "$ASSETS" | grep -q -- "$must" || {
		err "missing asset matching '$must'"
		VERIFY_FAILED=1
	}
done
CLI_COUNT="$(echo "$ASSETS" | grep -cE 'aghub-cli|\.tar\.gz$|\.zip$' || true)"
info "CLI/archive assets: $CLI_COUNT (expect ~4)"

# latest.json must reference this repo (auto-update endpoint integrity)
LJ="$(gh release download "$TAG" --repo "$REPO" --pattern latest.json --output - 2>/dev/null || true)"
if echo "$LJ" | grep -q "audichuang/aghub"; then
	ok "latest.json points at $REPO"
else
	err "latest.json does not reference $REPO"
	VERIFY_FAILED=1
fi

# Homebrew cask sha256 — informational only (the tap push can lag a moment).
CASK="$(gh api "repos/audichuang/homebrew-tap/contents/Casks/aghub.rb" --jq .content 2>/dev/null | base64 -d 2>/dev/null || true)"
if echo "$CASK" | grep -qE 'sha256 "[0-9a-f]{64}"'; then
	ok "Homebrew cask sha256 present"
else
	info "(could not confirm Homebrew cask sha256 — check manually)"
fi

if [ "$VERIFY_FAILED" != "0" ]; then
	err "Release $TAG built, but artifact verification found problems (see above)."
	exit 1
fi

echo
ok "Release $TAG complete."
echo "  brew install --cask audichuang/tap/aghub        # desktop"
echo "  brew install audichuang/tap/aghub-cli           # CLI"
