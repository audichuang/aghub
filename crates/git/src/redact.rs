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

		// Look for the userinfo terminator '@' before any path/query/fragment
		// or whitespace boundary.
		if let Some(boundary) = rest.find(|c: char| {
			matches!(c, '@' | '/' | '?' | '#') || c.is_whitespace()
		}) {
			if rest.as_bytes()[boundary] == b'@' {
				out.push_str("***@");
				rest = &rest[boundary + 1..];
			}
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
	fn no_userinfo_means_no_change() {
		let s = "cloning https://github.com/o/r.git into /tmp/x?ref=main";
		assert_eq!(redact_url_userinfo(s), s);
	}
}
