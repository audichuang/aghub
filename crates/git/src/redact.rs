//! Strip URL userinfo (`user:token@`) from any string before it becomes an error,
//! a log line, an Uncheckable reason, or a persisted URL.

use std::path::Path;

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
	let scrubbed = redact_url_userinfo(&text);
	if scrubbed == text {
		return; // no userinfo persisted (public repo / no credentials)
	}
	if let Err(e) = std::fs::write(config_path, scrubbed) {
		log::warn!(
			"failed to scrub credentials from git config at {}: {e}",
			config_path.display()
		);
	}
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
}
