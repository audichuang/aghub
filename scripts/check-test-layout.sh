#!/usr/bin/env bash
#
# Lint: integration-test layout. Every `crates/*/tests/*.rs` is linked as its
# own binary, and linking dominates the Windows CI test job, so a test that
# never writes process env belongs in its crate's one `tests/it/` binary. A
# test that writes env (or takes an env lock) keeps its own binary: libtest
# runs a binary's tests as threads of one process, so an env writer next to
# an unlocked reader is a race. See
# docs/history/tooling.md#integration-tests-share-one-binary
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
# Anything that writes env, or a lock that exists because something does.
env_writer='set_var|remove_var|set_current_dir|env_lock|Mutex|GlobalLockGuard|GlobalHome|TestEnv|TestLockGuard|isolate_env|with_isolated_state|HomeGuard|CwdGuard|with_pinned_data_dir|isolated_cli'

fail=0
for f in crates/*/tests/it/*.rs; do
	[ -e "$f" ] || continue
	if hits="$(grep -nE "$env_writer" "$f")"; then
		echo "✗ $f writes env or takes an env lock; give it its own crates/<crate>/tests/<name>.rs:" >&2
		echo "$hits" >&2
		fail=1
	fi
done
for f in crates/*/tests/*.rs; do
	[ -e "$f" ] || continue
	if ! grep -qE "$env_writer" "$f"; then
		echo "✗ $f never writes env; move it to $(dirname "$f")/it/ and add \`mod $(basename "$f" .rs);\` to it/main.rs" >&2
		fail=1
	fi
done
for d in crates/*/tests/it; do
	[ -d "$d" ] || continue
	for f in "$d"/*.rs; do
		m="$(basename "$f" .rs)"
		[ "$m" = main ] && continue
		if ! grep -qE "^mod ${m};" "$d/main.rs"; then
			echo "✗ $f is not declared in $d/main.rs (add \`mod $m;\`), so it never runs" >&2
			fail=1
		fi
	done
done
exit "$fail"
