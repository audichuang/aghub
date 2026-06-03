//! Shared rename-detection helpers used by both the update-check pipeline and
//! the apply/sync write paths. Centralising the predicate + the user-facing
//! message keeps the four call sites in sync so the desktop UI gets a single
//! error code (`SKILL_RENAMED_CODE`) and the same advice whether the rename
//! is detected at check time, apply time, or sync time.

/// Returns `Some(parsed_name)` when the upstream-parsed name differs from the
/// expected (locked) name — i.e. the skill was renamed in the source. Returns
/// `None` when the names match.
///
/// Cheap pure predicate; intentionally tolerant of whitespace-equivalent
/// comparisons only when the caller says so (we don't trim here because the
/// `SKILL.md` frontmatter parser is the authority on canonical form).
pub fn detect_rename(parsed_name: &str, expected: &str) -> Option<String> {
	if parsed_name == expected {
		None
	} else {
		Some(parsed_name.to_string())
	}
}

/// Canonical, user-facing rename message. The lock owner should delete the
/// old skill and install under the new name. Used by both the apply-update
/// and git-sync routes so the user sees the same advice in either path.
pub fn skill_renamed_message(old_name: &str, new_name: &str) -> String {
	format!(
		"Skill '{old_name}' was renamed to '{new_name}' in the source. \
		 Delete the old skill and install '{new_name}' instead."
	)
}

/// API error code returned to consumers for both apply-time and sync-time
/// renames. Distinct from the legacy `SKILL_NAME_MISMATCH` so consumers can
/// branch on a single stable code.
pub const SKILL_RENAMED_CODE: &str = "SKILL_RENAMED_IN_SOURCE";

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn detect_rename_returns_none_when_names_match() {
		assert_eq!(detect_rename("foo", "foo"), None);
	}

	#[test]
	fn detect_rename_returns_some_when_names_differ() {
		assert_eq!(
			detect_rename("new-name", "old-name"),
			Some("new-name".to_string())
		);
	}

	#[test]
	fn skill_renamed_message_contains_both_names_and_advice() {
		let msg = skill_renamed_message("old", "new");
		assert!(msg.contains("old"));
		assert!(msg.contains("new"));
		assert!(msg.contains("Delete"));
		assert!(msg.contains("install"));
	}
}
