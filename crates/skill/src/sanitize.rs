//! Name sanitization for skill installation.
//!
//! Converts arbitrary strings into safe skill directory names.
//! Mirrors the TypeScript `sanitizeName` function in `installer.ts`.

use std::collections::BTreeSet;

const MAX_NAME_LENGTH: usize = 255;

/// Convert an arbitrary string into a safe skill directory name.
///
/// Rules:
/// - Lowercase
/// - Replace spaces with hyphens, collapse multiple spaces into one hyphen
/// - Preserve `.` and `_` in middle positions
/// - Replace other non-`[a-z0-9._-]` characters with hyphens
/// - Collapse multiple consecutive hyphens into one
/// - Remove leading dots and hyphens
/// - Remove trailing dots and hyphens
/// - Apply Unicode lowercasing before filtering to match JS `.toLowerCase()`
/// - Truncate to 255 chars
/// - Return `"unnamed-skill"` if result is empty
pub fn sanitize_name(input: &str) -> String {
	// Process character by character:
	// - Unicode lowercase first, matching JS `String.toLowerCase()`.
	// - ASCII alphanumeric, `.`, `_` → keep
	// - Everything else (spaces, special chars, non-ASCII) → hyphen (collapsed)
	let mut result = String::new();
	let mut last_was_hyphen = false;

	for c in input.chars() {
		for c in c.to_lowercase() {
			if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
				result.push(c);
				last_was_hyphen = false;
			} else {
				// Non-ASCII, spaces, special chars, `/`, `\`, `@`, etc. → hyphen
				if !last_was_hyphen {
					result.push('-');
					last_was_hyphen = true;
				}
			}
		}
	}

	// Remove leading dots and hyphens
	let trimmed_start = result.trim_start_matches(['.', '-']);
	let mut result = trimmed_start.to_string();

	// Remove trailing dots and hyphens
	while result.ends_with('.') || result.ends_with('-') {
		result.pop();
	}

	// Truncate to 255 chars
	if result.len() > MAX_NAME_LENGTH {
		result.truncate(MAX_NAME_LENGTH);
		// After truncation, re-trim trailing dots and hyphens
		while result.ends_with('.') || result.ends_with('-') {
			result.pop();
		}
	}

	if result.is_empty() {
		"unnamed-skill".to_string()
	} else {
		result
	}
}

/// Previous ASCII-only sanitizer used before Unicode lowercasing was fixed.
///
/// This is kept only for lock pruning so existing folders created by older
/// aghub versions are recognized as present instead of dropped from locks.
pub fn legacy_sanitize_name(input: &str) -> String {
	let mut result = String::new();
	let mut last_was_hyphen = false;

	for c in input.chars() {
		let c = c.to_ascii_lowercase();
		if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
			result.push(c);
			last_was_hyphen = false;
		} else if !last_was_hyphen {
			result.push('-');
			last_was_hyphen = true;
		}
	}

	let trimmed_start = result.trim_start_matches(['.', '-']);
	let mut result = trimmed_start.to_string();

	while result.ends_with('.') || result.ends_with('-') {
		result.pop();
	}

	if result.len() > MAX_NAME_LENGTH {
		result.truncate(MAX_NAME_LENGTH);
		while result.ends_with('.') || result.ends_with('-') {
			result.pop();
		}
	}

	if result.is_empty() {
		"unnamed-skill".to_string()
	} else {
		result
	}
}

pub fn skill_present_on_disk(key: &str, present: &BTreeSet<String>) -> bool {
	present.contains(&sanitize_name(key))
		|| present.contains(&legacy_sanitize_name(key))
		|| present.contains(key)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_converts_to_lowercase() {
		assert_eq!(sanitize_name("MySkill"), "myskill");
		assert_eq!(sanitize_name("UPPERCASE"), "uppercase");
	}

	#[test]
	fn test_replaces_spaces_with_hyphens() {
		assert_eq!(sanitize_name("my skill"), "my-skill");
		assert_eq!(
			sanitize_name("Convex Best Practices"),
			"convex-best-practices"
		);
	}

	#[test]
	fn test_collapses_multiple_spaces() {
		assert_eq!(sanitize_name("my   skill"), "my-skill");
	}

	#[test]
	fn test_preserves_dots_and_underscores() {
		assert_eq!(sanitize_name("bun.sh"), "bun.sh");
		assert_eq!(sanitize_name("my_skill"), "my_skill");
		assert_eq!(sanitize_name("skill.v2_beta"), "skill.v2_beta");
	}

	#[test]
	fn test_preserves_numbers() {
		assert_eq!(sanitize_name("skill123"), "skill123");
		assert_eq!(sanitize_name("v2.0"), "v2.0");
	}

	#[test]
	fn test_replaces_special_chars_with_hyphens() {
		assert_eq!(sanitize_name("skill@name"), "skill-name");
		assert_eq!(sanitize_name("skill#name"), "skill-name");
		assert_eq!(sanitize_name("skill$name"), "skill-name");
		assert_eq!(sanitize_name("skill!name"), "skill-name");
	}

	#[test]
	fn test_collapses_multiple_special_chars() {
		assert_eq!(sanitize_name("skill@#$name"), "skill-name");
		assert_eq!(sanitize_name("a!!!b"), "a-b");
	}

	#[test]
	fn test_prevents_path_traversal_unix() {
		assert_eq!(sanitize_name("../etc/passwd"), "etc-passwd");
		assert_eq!(sanitize_name("../../secret"), "secret");
	}

	#[test]
	fn test_prevents_path_traversal_backslash() {
		assert_eq!(sanitize_name("..\\..\\secret"), "secret");
	}

	#[test]
	fn test_handles_absolute_paths() {
		assert_eq!(sanitize_name("/etc/passwd"), "etc-passwd");
		assert_eq!(
			sanitize_name("C:\\Windows\\System32"),
			"c-windows-system32"
		);
	}

	#[test]
	fn test_removes_leading_dots() {
		assert_eq!(sanitize_name(".hidden"), "hidden");
		assert_eq!(sanitize_name("..hidden"), "hidden");
		assert_eq!(sanitize_name("...skill"), "skill");
	}

	#[test]
	fn test_removes_trailing_dots() {
		assert_eq!(sanitize_name("skill."), "skill");
		assert_eq!(sanitize_name("skill.."), "skill");
	}

	#[test]
	fn test_removes_leading_hyphens() {
		assert_eq!(sanitize_name("-skill"), "skill");
		assert_eq!(sanitize_name("--skill"), "skill");
	}

	#[test]
	fn test_removes_trailing_hyphens() {
		assert_eq!(sanitize_name("skill-"), "skill");
		assert_eq!(sanitize_name("skill--"), "skill");
	}

	#[test]
	fn test_removes_mixed_leading_dots_and_hyphens() {
		assert_eq!(sanitize_name(".-.-skill"), "skill");
		assert_eq!(sanitize_name("-.-.skill"), "skill");
	}

	#[test]
	fn test_empty_string_returns_unnamed_skill() {
		assert_eq!(sanitize_name(""), "unnamed-skill");
	}

	#[test]
	fn test_only_special_chars_returns_unnamed_skill() {
		assert_eq!(sanitize_name("..."), "unnamed-skill");
		assert_eq!(sanitize_name("---"), "unnamed-skill");
		assert_eq!(sanitize_name("@#$%"), "unnamed-skill");
	}

	#[test]
	fn test_truncates_long_names() {
		let long_name = "a".repeat(300);
		let result = sanitize_name(&long_name);
		assert_eq!(result.len(), 255);
		assert_eq!(result, "a".repeat(255));
	}

	#[test]
	fn test_strips_unicode_characters() {
		assert_eq!(sanitize_name("skill日本語"), "skill");
		// 'é' is non-ASCII, '🎉' is non-ASCII; 'moji' and 'skill' remain
		assert_eq!(sanitize_name("émoji🎉skill"), "moji-skill");
	}

	#[test]
	fn test_unicode_lowercase_matches_js_before_ascii_filter() {
		assert_eq!(sanitize_name("İstanbul"), "i-stanbul");
		assert_eq!(sanitize_name("Kelvin"), "kelvin");
		assert_eq!(sanitize_name("ẞeta"), "eta");
	}

	#[test]
	fn legacy_sanitize_name_reproduces_prefix_folder() {
		assert_eq!(legacy_sanitize_name("İstanbul"), "stanbul");
		assert_eq!(legacy_sanitize_name("ẞeta"), "eta");
	}

	#[test]
	fn legacy_equals_new_for_ascii() {
		for value in ["My Skill", "vercel/next.js", "skill日本語"] {
			assert_eq!(legacy_sanitize_name(value), sanitize_name(value));
		}
	}

	#[test]
	fn skill_present_on_disk_accepts_new_legacy_and_raw_names() {
		let mut present = BTreeSet::new();
		present.insert("i-stanbul".to_string());
		assert!(skill_present_on_disk("İstanbul", &present));
		present.clear();
		present.insert("stanbul".to_string());
		assert!(skill_present_on_disk("İstanbul", &present));
		present.clear();
		present.insert("İstanbul".to_string());
		assert!(skill_present_on_disk("İstanbul", &present));
	}

	#[test]
	fn test_github_repo_style_names() {
		assert_eq!(sanitize_name("vercel/next.js"), "vercel-next.js");
		assert_eq!(sanitize_name("owner/repo-name"), "owner-repo-name");
	}

	#[test]
	fn test_handles_urls() {
		assert_eq!(sanitize_name("https://example.com"), "https-example.com");
	}

	#[test]
	fn test_handles_mintlify_style_names() {
		assert_eq!(sanitize_name("docs.example.com"), "docs.example.com");
		assert_eq!(sanitize_name("bun.sh"), "bun.sh");
	}
}
