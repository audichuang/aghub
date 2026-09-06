//! Prepared input for an audit run — everything already read into memory.

/// One resource bundled with a skill (a script, reference, or asset file).
#[derive(Debug, Clone)]
pub struct ResourceFile {
	/// Path relative to the skill root, e.g. `"scripts/setup.sh"`.
	pub path: String,
	pub content: Vec<u8>,
}

/// Everything the offline audit needs.
///
/// `skill_md` is the **raw** UTF-8 SKILL.md text including frontmatter —
/// injection detection must see the whole file, not the parsed markdown body.
#[derive(Debug, Clone)]
pub struct AuditInput {
	pub name: String,
	pub skill_md: String,
	pub resources: Vec<ResourceFile>,
}
