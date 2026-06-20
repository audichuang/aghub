//! Stamp the CLI binary with a git-derived version so a from-source build
//! reports a real version (e.g. `2.1.5-3-gabc1234` or `2.1.5-dev`) instead of
//! the workspace manifest's placeholder `version`.
//!
//! Resolution order:
//!   1. `git describe --tags --dirty=-dev` (leading `v` stripped) — needs tags
//!      reachable, so it works in a full local checkout.
//!   2. fallback to `CARGO_PKG_VERSION` — the value the release CI `sed`s the
//!      tag into, and also what a tag-less / tarball build sees. `--always` is
//!      deliberately NOT used: a bare commit SHA must never shadow the
//!      CI-injected release version.

use std::process::Command;

fn main() {
	// Re-stamp when HEAD/index move so the embedded version tracks commits and
	// dirty state without rerunning on every unrelated build. Paths are relative
	// to this crate dir; the workspace `.git` lives two levels up. Missing paths
	// (e.g. a source tarball) are simply never watched — harmless.
	for rel in [".git/HEAD", ".git/index", ".git/packed-refs"] {
		println!("cargo:rerun-if-changed=../../{rel}");
	}

	let version = git_describe().unwrap_or_else(|| {
		std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into())
	});
	println!("cargo:rustc-env=AGHUB_CLI_VERSION={version}");
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
