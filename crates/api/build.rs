//! Stamp the `aghub-api` binary with a git-derived version so a from-source
//! build reports a real version (e.g. `2.7.2-3-gabc1234` or `2.7.2-dev`)
//! instead of the workspace manifest's placeholder `version`. Mirrors
//! `crates/cli/build.rs` exactly — keep the two in sync.
//!
//! Resolution order:
//!   1. `AGHUB_RELEASE_VERSION` env — set by the release CI. Without it, CI's
//!      version-sync `sed` dirties the tree and `git describe --dirty=-dev`
//!      stamps every RELEASE binary `X.Y.Z-dev` (shipped that way up to
//!      v2.5.4).
//!   2. `git describe --tags --dirty=-dev` (leading `v` stripped) — needs tags
//!      reachable, so it works in a full local checkout.
//!   3. fallback to `CARGO_PKG_VERSION` — what a tag-less / tarball build
//!      sees. `--always` is deliberately NOT used: a bare commit SHA must
//!      never shadow the release version.
//!
//! No `cargo:rerun-if-changed`/`cargo:rerun-if-env-changed` directives here:
//! emitting ANY such directive opts cargo OUT of its default whole-package
//! change detection (which re-runs `build.rs` on any tracked-file edit,
//! including an unstaged one) and IN to only the directives listed. A prior
//! version watched `../../.git/{HEAD,index,packed-refs}` plus the env var,
//! which missed unstaged source edits between commits — a `-dev` build stayed
//! stamped with a stale version until the next commit touched one of those
//! paths. Falling back to the default scan catches edits WITHIN THIS
//! PACKAGE — it does not catch a bare commit with no working-tree change, an
//! env-only change, or an edit confined to another workspace crate, so the
//! stamp can stay stale until something in this package changes. Release CI
//! is unaffected either way: it always builds clean with an explicit
//! `AGHUB_RELEASE_VERSION` plus a downstream exact-version smoke check.

use std::process::Command;

fn main() {
	let version = std::env::var("AGHUB_RELEASE_VERSION")
		.ok()
		.filter(|v| !v.is_empty())
		.or_else(git_describe)
		.unwrap_or_else(|| {
			std::env::var("CARGO_PKG_VERSION")
				.unwrap_or_else(|_| "0.0.0".into())
		});
	println!("cargo:rustc-env=AGHUB_API_VERSION={version}");
}

fn git_describe() -> Option<String> {
	let output = Command::new("git")
		.args(["describe", "--tags", "--dirty=-dev"])
		.output()
		.ok()?;
	if !output.status.success() {
		return None;
	}
	let described = String::from_utf8(output.stdout).ok()?;
	let described = described.trim();
	if described.is_empty() {
		return None;
	}
	Some(described.strip_prefix('v').unwrap_or(described).to_string())
}
