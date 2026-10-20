#!/usr/bin/env bash
#
# Lint: a literal TAB after non-blank text on a line. Rust here is
# tab-indented, so such a tab is a multi-line string literal whose `\`
# continuation is missing (the next line's indentation lands in the string).
# See docs/history/tooling.md#literal-tabs-inside-a-string-literal
set -euo pipefail

cd "$(dirname "$0")/.."
hits="$(grep -rnP '\S *\t' --include='*.rs' crates || true)"
if [ -n "$hits" ]; then
	echo "✗ literal tab after code (a missing \`\\\` line continuation in a string?):" >&2
	echo "$hits" >&2
	exit 1
fi
