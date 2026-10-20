#!/usr/bin/env bash
#
# Lint: a literal TAB after non-blank text on a line. Rust here is
# tab-indented, so such a tab is almost always a removed `\`+newline
# continuation that left the next line's indentation inside one string.
# See docs/history/tooling.md#literal-tabs-inside-a-string-literal
set -uo pipefail

cd "$(dirname "$0")/.."
tab="$(printf '\t')"
# POSIX ERE, not `grep -P`: BSD grep on macOS has no -P.
hits="$(grep -rn "[^[:space:]] *${tab}" --include='*.rs' crates)"
case $? in
0)
	echo "✗ literal tab after code (a removed \`\\\` line continuation in a string?):" >&2
	echo "$hits" >&2
	exit 1
	;;
1) ;;
*)
	echo "check-string-tabs: grep failed" >&2
	exit 2
	;;
esac
