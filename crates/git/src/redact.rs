//! Strip URL userinfo (`user:token@`) from any string before it becomes an error,
//! a log line, an Uncheckable reason, or a persisted URL.

use std::path::Path;
use url::Url;

/// Replace every `scheme://user:secret@host` occurrence's userinfo with `***`.
///
/// Scans for `://`, then looks for an `@` userinfo terminator that appears
/// before the next path/query/fragment/whitespace boundary. When found, the
/// userinfo segment is replaced with `***`.
pub fn redact_url_userinfo(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	let mut rest = s;

	while let Some(rel) = rest.find("://") {
		// Emit everything up to and including the "://".
		out.push_str(&rest[..rel + 3]);
		rest = &rest[rel + 3..];

		let whitespace = rest.find(char::is_whitespace).unwrap_or(rest.len());
		let token = &rest[..whitespace];
		let authority_end = token.find(['/', '?', '#']).unwrap_or(token.len());
		let authority = &token[..authority_end];

		let at = authority.rfind('@').or_else(|| {
			let last_at = token.rfind('@')?;
			let before_at = &token[..last_at];
			let prefix_end =
				before_at.find(['/', '?', '#']).unwrap_or(before_at.len());
			let authority_prefix = &before_at[..prefix_end];
			authority_prefix.contains(':').then_some(last_at)
		});

		if let Some(at) = at {
			out.push_str("***@");
			rest = &rest[at + 1..];
		}
	}

	out.push_str(rest);
	out
}

/// Strip credential userinfo from a git repo's on-disk config, best-effort.
///
/// gix's `PrepareFetch` persists the fetch URL **losslessly** (`save_to` →
/// `Url::to_bstring()`, which keeps `user:token@`) into the temp repo's local
/// config *before* connecting — so a token-bearing clone/fetch leaves the token
/// on disk. `redact_url_userinfo` only sanitizes strings, not this file. Call
/// this right after a clone/fetch to rewrite the config with the userinfo
/// stripped (`***@host`).
///
/// Safe because nothing re-fetches through the persisted remote afterwards: the
/// treeless path only reads objects, and git-scan re-clones a fresh dir to
/// switch branches — so the `***@host` placeholder is never used to connect.
///
/// Best-effort: a rewrite failure is logged at warn (never silently swallowed —
/// it means the token is still on disk) but does not fail the surrounding
/// operation; the temp dir is `0700` and short-lived. `config_path` is the git
/// config file (`<dir>/config` for a bare repo, `<dir>/.git/config` for a
/// worktree). A missing file or a config with no userinfo is a no-op.
pub(crate) fn scrub_config_userinfo(config_path: &Path) {
	let text = match std::fs::read_to_string(config_path) {
		Ok(text) => text,
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
		Err(e) => {
			log::warn!(
				"could not read git config to scrub credentials at {}: {e}",
				config_path.display()
			);
			return;
		}
	};
	let mut changed = false;
	let mut out = String::with_capacity(text.len());
	for (i, line) in text.lines().enumerate() {
		if i > 0 {
			out.push('\n');
		}
		out.push_str(&scrub_config_url_line(line, &mut changed));
	}
	if text.ends_with('\n') {
		out.push('\n');
	}
	if !changed {
		return; // no credentialed remote url in this config
	}
	if let Err(e) = std::fs::write(config_path, out) {
		log::warn!(
			"failed to scrub credentials from git config at {}: {e}",
			config_path.display()
		);
	}
}

/// Rewrite a single git-config line: if it is a `url = <value>` whose value is a
/// URL carrying userinfo, return the line with the userinfo stripped and set
/// `*changed`. Any other line (or a URL with no userinfo, or an unparsable
/// value) is returned unchanged. Using real URL parsing — NOT string
/// redaction — means a port colon or an `@` in the PATH is never mistaken for
/// credentials (e.g. `https://host:8443/a/b@v2.git` is left intact).
fn scrub_config_url_line(line: &str, changed: &mut bool) -> String {
	let trimmed = line.trim_start();
	// Match the `url` key only (case-insensitive), then `=`. `urlbase = …`,
	// section headers, and `insteadOf` lines all fall through untouched.
	let Some(rest) = trimmed
		.get(..3)
		.filter(|k| k.eq_ignore_ascii_case("url"))
		.map(|_| &trimmed[3..])
	else {
		return line.to_string();
	};
	let Some(value) = rest.trim_start().strip_prefix('=') else {
		return line.to_string();
	};
	// ponytail: strip a single layer of surrounding quotes; fully-escaped
	// git-config quoting is not handled (a token needing it is rare).
	let value = value.trim();
	let value = value
		.strip_prefix('"')
		.and_then(|v| v.strip_suffix('"'))
		.unwrap_or(value);
	let Ok(mut url) = Url::parse(value) else {
		return line.to_string();
	};
	if url.username().is_empty() && url.password().is_none() {
		return line.to_string();
	}
	let _ = url.set_username("");
	let _ = url.set_password(None);
	*changed = true;
	let indent = &line[..line.len() - trimmed.len()];
	format!("{indent}url = {url}")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn strips_pat_userinfo() {
		let s = "fatal: Authentication failed for 'https://user:ghp_SECRET@github.com/o/r.git'";
		let r = redact_url_userinfo(s);
		assert!(!r.contains("ghp_SECRET"));
		assert!(!r.contains("user:"));
		assert!(r.contains("https://***@github.com/o/r.git"));
	}

	#[test]
	fn leaves_clean_url_untouched() {
		let s = "https://github.com/o/r.git";
		assert_eq!(redact_url_userinfo(s), s);
	}

	#[test]
	fn handles_multiple_urls() {
		let s = "a https://u:p@h/x b https://v:q@h/y";
		let r = redact_url_userinfo(s);
		assert!(!r.contains("u:p") && !r.contains("v:q"));
	}

	#[test]
	fn redacts_to_last_at_in_authority() {
		let r = redact_url_userinfo("https://user:p@ss@host/x");
		assert_eq!(r, "https://***@host/x");
		assert!(!r.contains("p@ss"));
	}

	#[test]
	fn redacts_password_with_path_chars_before_last_at() {
		let r = redact_url_userinfo("https://u:p/a?b#c@host/x");
		assert_eq!(r, "https://***@host/x");
		assert!(!r.contains("p/a?b#c"));
	}

	#[test]
	fn redacts_password_that_starts_with_delimiter() {
		for secret in ["/secret", "?secret", "#secret"] {
			let url = format!("https://u:{secret}@host/x");
			let redacted = redact_url_userinfo(&url);
			assert_eq!(redacted, "https://***@host/x");
			assert!(!redacted.contains(secret));
		}
	}

	#[test]
	fn redacts_numeric_password_before_delimiter() {
		let r = redact_url_userinfo("https://u:123/path@host/x");
		assert_eq!(r, "https://***@host/x");
		assert!(!r.contains("u:123"));
	}

	#[test]
	fn no_userinfo_means_no_change() {
		let s = "cloning https://github.com/o/r.git into /tmp/x?ref=main";
		assert_eq!(redact_url_userinfo(s), s);
	}

	#[test]
	fn scrub_config_strips_persisted_token() {
		let dir = tempfile::tempdir().unwrap();
		let config = dir.path().join("config");
		std::fs::write(
			&config,
			"[core]\n\tbare = true\n[remote \"origin\"]\n\turl = \
			 https://x-access-token:ghp_SECRET@dev.azure.example/o/r\n\t\
			 fetch = +refs/heads/*:refs/remotes/origin/*\n",
		)
		.unwrap();

		scrub_config_userinfo(&config);

		let after = std::fs::read_to_string(&config).unwrap();
		assert!(
			!after.contains("ghp_SECRET"),
			"token must be gone from config"
		);
		assert!(!after.contains("x-access-token"));
		// Non-credential lines and the host survive.
		assert!(after.contains("dev.azure.example/o/r"));
		assert!(after.contains("fetch = +refs/heads/*"));
	}

	#[test]
	fn scrub_config_missing_file_is_noop() {
		let dir = tempfile::tempdir().unwrap();
		// Must not panic on a nonexistent config.
		scrub_config_userinfo(&dir.path().join("does-not-exist"));
	}

	#[test]
	fn scrub_config_preserves_clean_url_with_port_and_path_at() {
		// A clean URL (no userinfo) with a port AND an `@` in the path must be
		// left untouched — the old string-redaction mistook the port colon for
		// credentials and corrupted it to `https://***@v2.git`.
		let dir = tempfile::tempdir().unwrap();
		let config = dir.path().join("config");
		let clean =
			"[remote \"origin\"]\n\turl = https://git.example:8443/org/repo@v2.git\n";
		std::fs::write(&config, clean).unwrap();

		scrub_config_userinfo(&config);

		let after = std::fs::read_to_string(&config).unwrap();
		assert!(
			after.contains("https://git.example:8443/org/repo@v2.git"),
			"clean URL with port + path-@ must be preserved, got: {after}"
		);
		assert!(!after.contains("***"));
	}

	#[test]
	fn scrub_config_strips_userinfo_but_keeps_port() {
		let dir = tempfile::tempdir().unwrap();
		let config = dir.path().join("config");
		std::fs::write(
			&config,
			"[remote \"origin\"]\n\turl = https://u:tok@tfs.example:8443/a/b\n",
		)
		.unwrap();

		scrub_config_userinfo(&config);

		let after = std::fs::read_to_string(&config).unwrap();
		assert!(!after.contains("tok"), "token must be gone");
		assert!(!after.contains("u:tok"));
		assert!(
			after.contains("https://tfs.example:8443/a/b"),
			"host + port must survive, got: {after}"
		);
	}
}
