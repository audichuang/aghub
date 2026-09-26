#!/usr/bin/env bash
#
# Preflight guard: the Rust tests build their fixture projects under the temp
# dir, and the skill repair flow deliberately refuses a path it believes is
# inside a git repository that `git` cannot answer for. A stray `.git` in ANY
# ancestor of the temp dir (an empty `/tmp/.git` did this on 2026-09-26) makes
# every such fixture look like it lives in a broken repo, and the repair tests
# fail on a clean main for a reason no test output names. Say it up front.
set -euo pipefail

dir="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
while :; do
	if [ -e "$dir/.git" ]; then
		echo "✗ '$dir/.git' exists above the test temp dir (${TMPDIR:-/tmp})." >&2
		echo "  Test fixtures under it would look like they live in a git repo, and the" >&2
		echo "  skill repair tests fail. Remove it if it is stray (\`rmdir '$dir/.git'\` when" >&2
		echo "  empty), or run with TMPDIR pointing elsewhere, e.g. TMPDIR=/var/tmp just preflight." >&2
		exit 1
	fi
	[ "$dir" = "/" ] && break
	dir="$(dirname "$dir")"
done
