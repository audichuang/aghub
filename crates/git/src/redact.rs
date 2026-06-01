//! Strip URL userinfo (`user:token@`) from any string before it becomes an error,
//! a log line, an Uncheckable reason, or a persisted URL.

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
			let suspicious_userinfo =
				authority.rsplit_once(':').is_some_and(|(_, suffix)| {
					!suffix.is_empty()
						&& !suffix.chars().all(|c| c.is_ascii_digit())
				});
			if suspicious_userinfo {
				token.rfind('@')
			} else {
				None
			}
		});

		if let Some(at) = at {
			out.push_str("***@");
			rest = &rest[at + 1..];
		}
	}

	out.push_str(rest);
	out
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
	fn no_userinfo_means_no_change() {
		let s = "cloning https://github.com/o/r.git into /tmp/x?ref=main";
		assert_eq!(redact_url_userinfo(s), s);
	}
}
