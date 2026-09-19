use super::*;

// `disable`/`enable skills` used to flip the flag in memory and call
// `save_current()`, which serializes MCPs and nothing else: the skill state
// was dropped AND the agent's MCP config was rewritten from the normalized
// model, stripping per-server fields aghub does not model. Two assertions,
// because either alone false-passes — the refusal without the byte check
// would pass even if the write still happened first, and the byte check
// without the refusal would pass on a silent no-op.
#[test]
fn disable_skill_refuses_and_leaves_the_mcp_config_untouched() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();

	let skill_dir = root.join(".claude/skills/demo-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: demo-skill\ndescription: fixture\n---\n\nbody\n",
	)
	.unwrap();

	// `customField` is the canary: aghub does not model it, so any rewrite
	// of this file drops it.
	let mcp_path = root.join(".mcp.json");
	let original = "{\n  \"mcpServers\": {\n    \"demo\": {\n      \"command\": \"echo\",\n      \"args\": [\"hi\"],\n      \"customField\": \"keepme\"\n    }\n  }\n}\n";
	std::fs::write(&mcp_path, original).unwrap();

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	assert!(
		mgr.get_skill("demo-skill").is_some(),
		"fixture must be discoverable, or the refusal below proves nothing"
	);

	let error = mgr
		.disable_skill("demo-skill")
		.expect_err("skill enable/disable has no writer; it must refuse");
	assert!(
		matches!(error, ConfigError::UnsupportedOperation { .. }),
		"expected an unsupported-operation refusal, got: {error}"
	);

	assert_eq!(
		std::fs::read_to_string(&mcp_path).unwrap(),
		original,
		"a skill command must not rewrite the agent's MCP config"
	);
}

#[cfg(unix)]
#[test]
fn add_skill_universal_writes_master_and_symlinks_agent() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("uni-skill");
	skill.description = Some("universal test".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// Real master lives under .agents/skills (NOT duplicated per agent).
	assert!(root.join(".aghub/uni-skill/SKILL.md").exists());
	// Claude's own dir holds a symlink that resolves to the master.
	let link = root.join(".claude/skills/uni-skill");
	assert!(std::fs::symlink_metadata(&link)
		.unwrap()
		.file_type()
		.is_symlink());
	assert!(link.join("SKILL.md").exists());
}

#[cfg(unix)]
#[test]
fn add_skill_universal_idempotent_readd_is_noop() {
	use crate::create_adapter;
	use crate::models::AgentType;
	use crate::skills::linker::Linker;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("idem-skill");
	skill.description = Some("idempotent test".to_string());

	// First install must succeed.
	mgr.add_skill_universal(skill.clone()).unwrap();

	let master = root.join(".aghub/idem-skill");
	let link = root.join(".claude/skills/idem-skill");
	assert!(
		master.join("SKILL.md").exists(),
		"master must exist after first add"
	);
	assert!(Linker::is_link(&link), "link must exist after first add");

	// Second install — same skill name — must be a no-op Ok(()), not error.
	mgr.add_skill_universal(skill).unwrap();

	// Master and link must still be intact.
	assert!(
		master.join("SKILL.md").exists(),
		"master must survive re-add"
	);
	assert!(Linker::is_link(&link), "link must survive re-add");
}

#[cfg(unix)]
#[test]
fn add_skill_universal_real_conflict_still_errors() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("conflict-skill");
	skill.description = Some("conflict test".to_string());

	// Pre-place a REAL directory (not a link) at the agent's slot.
	let slot = root.join(".claude/skills/conflict-skill");
	std::fs::create_dir_all(&slot).unwrap();
	std::fs::write(slot.join("SKILL.md"), "foreign").unwrap();

	// add must error — real dir occupies the slot.
	let res = mgr.add_skill_universal(skill);
	assert!(
		res.is_err(),
		"must error when a real foreign dir occupies the agent slot"
	);
	// Foreign content must survive (no-clobber).
	assert_eq!(
		std::fs::read_to_string(slot.join("SKILL.md")).unwrap(),
		"foreign"
	);
}

#[cfg(unix)]
#[test]
fn add_skill_writes_master_and_symlinks_agent() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("manual-skill");
	skill.description = Some("manual test".to_string());
	mgr.add_skill(skill).unwrap();

	assert!(root.join(".aghub/manual-skill/SKILL.md").exists());
	let link = root.join(".claude/skills/manual-skill");
	assert!(std::fs::symlink_metadata(&link)
		.unwrap()
		.file_type()
		.is_symlink());
	assert!(link.join("SKILL.md").exists());

	let saved = mgr.get_skill("manual-skill").unwrap();
	assert!(saved.canonical_path.is_some());
}

// no-copy regression: add_skill_from_path writes a .agents Master and a
// link in the agent dir, never a private copy (Locked Decision 1).
#[cfg(unix)]
#[test]
fn add_skill_from_path_links_master_not_copy() {
	use crate::create_adapter;
	use crate::models::AgentType;
	use crate::skills::linker::Linker;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// Create a source skill directory with SKILL.md
	let src = tmp.path().join("src/my-skill");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: my-skill\ndescription: d\n---\nbody",
	)
	.unwrap();

	mgr.add_skill_from_path(&src.join("SKILL.md")).unwrap();

	let canonical = root.join(".aghub/my-skill");
	let link = root.join(".claude/skills/my-skill");
	assert!(canonical.join("SKILL.md").exists(), "Master materialized");
	assert!(
		Linker::is_link(&link),
		"agent dir must hold a link to the Master, not a copy"
	);
}

// GAP-2 no-copy regression: add_skill (manual-create) writes a .agents
// Master and a link in the agent dir, never a private copy, and records
// canonical_path (link provenance) -- proving the Task 25 add_skill ->
// add_skill_universal delegation (Locked Decision 1).
#[cfg(unix)]
#[test]
fn add_skill_manual_create_links_master_not_copy() {
	use crate::create_adapter;
	use crate::models::AgentType;
	use crate::skills::linker::Linker;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("manual-skill");
	skill.description = Some("manual create test".to_string());
	mgr.add_skill(skill).unwrap();

	let canonical = root.join(".aghub/manual-skill");
	let link = root.join(".claude/skills/manual-skill");
	assert!(
		canonical.join("SKILL.md").exists(),
		"manual-create must materialize a .agents Master"
	);
	assert!(
		Linker::is_link(&link),
		"manual-create must link the agent dir to the Master, not copy"
	);
	// Link provenance, not copy provenance.
	let recorded = mgr.get_skill("manual-skill").unwrap();
	assert!(
		recorded.canonical_path.is_some(),
		"manual-create must record canonical_path (link provenance)"
	);
}

// OpenCode used to be a project NativeReader: it writes `.opencode/skills`
// but READ `.agents/skills`, so an install gave it the Master and no link of
// its own — along with nine other agents reading that same directory. Now it
// gets its own Referrer into the store and the shared slot stays untouched.
#[cfg(unix)]
#[test]
fn add_skill_opencode_gets_its_own_referrer_into_the_store() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("native-skill");
	skill.description = Some("native reader test".to_string());
	mgr.add_skill(skill).unwrap();

	// Master materialized once in the store — NOT in `.agents/skills`,
	// which is what made storing equal granting.
	assert!(root.join(".aghub/native-skill/SKILL.md").exists());
	assert!(
		std::fs::symlink_metadata(root.join(".agents/skills/native-skill"))
			.is_err(),
		"no shared-slot agent was targeted, so nothing may occupy the slot"
	);
	// OpenCode now gets its OWN Referrer — it used to get none at all,
	// reading the Master through `.agents/skills` along with four others.
	assert!(
		Linker::is_link(&root.join(".opencode/skills/native-skill")),
		"OpenCode must hold its own link into the store"
	);
	// Still recorded with universal provenance.
	assert!(mgr
		.get_skill("native-skill")
		.unwrap()
		.canonical_path
		.is_some());
	// OpenCode's dir is private, so this grant reaches nobody else.
	assert!(
		mgr.skill_target_shares_with().is_empty(),
		"got {:?}",
		mgr.skill_target_shares_with()
	);
}

#[test]
fn remove_skill_path_refuses_dir_outside_allowed_roots() {
	// Defense-in-depth: the legacy copy-removal helper must never
	// `remove_dir_all` a directory that escapes the allow-listed skill roots,
	// even if a crafted `source_path` points outside them.
	let tmp = tempfile::tempdir().unwrap();
	let outside = tmp.path().join("outside/foo");
	std::fs::create_dir_all(&outside).unwrap();
	std::fs::write(outside.join("SKILL.md"), "x").unwrap();
	let allowed = tmp.path().join("allowed");
	std::fs::create_dir_all(&allowed).unwrap();
	let roots = vec![allowed];

	let res = remove_skill_path(
		&outside.join("SKILL.md"),
		"foo",
		false,
		None,
		&roots,
	);

	assert!(res.is_err(), "must refuse to remove a dir outside roots");
	assert!(outside.exists(), "out-of-root dir must survive");
}

#[test]
fn remove_skill_path_removes_contained_dir() {
	let tmp = tempfile::tempdir().unwrap();
	let skills = tmp.path().join("skills");
	let foo = skills.join("foo");
	std::fs::create_dir_all(&foo).unwrap();
	std::fs::write(foo.join("SKILL.md"), "x").unwrap();
	let roots = vec![skills.clone()];

	remove_skill_path(&foo.join("SKILL.md"), "foo", false, None, &roots)
		.unwrap();

	assert!(!foo.exists(), "a contained skill dir is removed normally");
}

// T-REMOVE-SKILL-PATH-JUNCTION: a junction referrer is unlinked on remove,
// and the shared Master directory + its files survive (remove_dir, not
// remove_dir_all). Runs on windows-latest (junctions need no admin).
#[cfg(windows)]
#[test]
fn remove_skill_path_unlinks_junction_keeps_master() {
	use crate::skills::linker::create_junction;
	let tmp = tempfile::tempdir().unwrap();
	let master = tmp.path().join(".aghub/foo");
	std::fs::create_dir_all(&master).unwrap();
	std::fs::write(master.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();
	let claude = tmp.path().join(".claude/skills");
	std::fs::create_dir_all(&claude).unwrap();
	let link = claude.join("foo");
	let abs_master = master.canonicalize().unwrap();
	create_junction(&abs_master, &link).unwrap();

	let roots = vec![tmp.path().to_path_buf()];
	remove_skill_path(
		&master.join("SKILL.md"),
		"foo",
		true, // is_link
		Some(claude.as_path()),
		&roots,
	)
	.unwrap();

	assert!(
		std::fs::symlink_metadata(&link).is_err(),
		"junction must be unlinked"
	);
	assert!(
		master.join("SKILL.md").exists(),
		"shared Master must survive (remove_dir, not remove_dir_all)"
	);
}

#[test]
fn test_format_skill_preserves_body() {
	let mut skill = Skill::new("test-skill");
	skill.description = Some("A test".to_string());
	let body = "\n# Original Title\n\nInstruction content.\n";
	let output = format_skill(&skill, Some(body), &BTreeMap::new());
	assert!(output.contains("# Original Title"));
	assert!(output.contains("Instruction content."));
	// Frontmatter should be valid YAML
	assert!(output.starts_with("---\n"));
	assert!(output.contains("---\n\n# Original Title"));
}

#[test]
fn test_format_skill_generates_placeholder_without_body() {
	let skill = Skill::new("test-skill");
	let output = format_skill(&skill, None, &BTreeMap::new());
	assert!(output.contains("# test-skill"));
}

#[test]
fn test_format_skill_stays_parseable_by_skill_crate() {
	let skill = Skill::new("test-skill");
	let output = format_skill(&skill, None, &BTreeMap::new());
	let parsed = skill::parser::parse_skill_md(&output).unwrap();
	assert_eq!(parsed.name, "test-skill");
	assert_eq!(parsed.description, "");
}

#[test]
fn test_format_skill_quotes_colon_in_description() {
	let mut skill = Skill::new("test");
	skill.description = Some("Source: https://example.com".to_string());
	let output = format_skill(&skill, None, &BTreeMap::new());
	// serde_yaml should quote the value containing ':'
	let reparsed: BTreeMap<String, String> = serde_yaml::from_str(
		output
			.trim_start_matches("---\n")
			.split("---\n")
			.next()
			.unwrap(),
	)
	.expect("Should produce valid YAML");
	assert_eq!(reparsed["description"], "Source: https://example.com");
}

#[test]
fn test_format_skill_quotes_numeric_values() {
	let mut skill = Skill::new("test");
	skill.version = Some("123".to_string());
	skill.author = Some("true".to_string());
	let output = format_skill(&skill, None, &BTreeMap::new());
	let reparsed: BTreeMap<String, String> = serde_yaml::from_str(
		output
			.trim_start_matches("---\n")
			.split("---\n")
			.next()
			.unwrap(),
	)
	.expect("Should produce valid YAML");
	assert_eq!(reparsed["version"], "123");
	assert_eq!(reparsed["author"], "true");
}

// -----------------------------------------------------------------------
// P0-K fix: remove_skill for universal mode was a no-op
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn remove_skill_unlinks_agent_symlink_but_preserves_canonical() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// Install a universal skill
	let mut skill = Skill::new("rm-test");
	skill.description = Some("test".to_string());
	mgr.add_skill_universal(skill).unwrap();

	let canonical = root.join(".aghub/rm-test/SKILL.md");
	let link = root.join(".claude/skills/rm-test");
	assert!(canonical.exists());
	assert!(std::fs::symlink_metadata(&link)
		.unwrap()
		.file_type()
		.is_symlink());

	// Remove the skill
	mgr.remove_skill("rm-test").unwrap();

	// Agent symlink should be gone
	assert!(!link.exists());
	// Canonical should still be there (single-agent removal keeps it)
	assert!(canonical.exists());
	// Config entry should be removed
	assert!(mgr.config.as_ref().unwrap().skills.is_empty());
}

#[cfg(unix)]
#[test]
fn remove_skill_universal_idempotent_when_symlink_already_gone() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("rm-idem");
	skill.description = Some("test".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// Manually remove the symlink before calling remove_skill
	let link = root.join(".claude/skills/rm-idem");
	assert!(link.exists());
	std::fs::remove_file(&link).unwrap();
	assert!(!link.exists());

	// Should not error even though the symlink is already gone
	mgr.remove_skill("rm-idem").unwrap();
}

#[cfg(unix)]
#[test]
fn remove_skill_preserves_canonical_for_multi_agent_ref() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();

	// Claude installs first
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	let mut skill = Skill::new("multi-ref");
	skill.description = Some("test".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// Cursor must NOT see it. Installing for Claude used to hand the skill
	// to Cursor too, because both read `.agents/skills` and that directory
	// held the Master. This assertion pinned that leak as a feature.
	let mut mgr2 = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr2.load().unwrap();
	assert!(
		!mgr2
			.config
			.as_ref()
			.unwrap()
			.skills
			.iter()
			.any(|s| s.name == "multi-ref"),
		"Cursor was never granted multi-ref and must not see it"
	);

	let canonical = root.join(".aghub/multi-ref/SKILL.md");
	assert!(canonical.exists());

	// Remove from Claude only
	mgr.remove_skill("multi-ref").unwrap();

	// Claude symlink gone, canonical preserved
	assert!(!root.join(".claude/skills/multi-ref").exists());
	assert!(canonical.exists());

	// Cursor still cannot see it — it never could, and the Master surviving
	// is about the STORE keeping the bytes, not about another agent
	// silently inheriting them.
	let mut mgr3 = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr3.load().unwrap();
	assert!(
		!mgr3
			.config
			.as_ref()
			.unwrap()
			.skills
			.iter()
			.any(|s| s.name == "multi-ref"),
		"removing Claude's grant must not hand the skill to Cursor"
	);
}

// The `remove_skill` seam's own guard: an agent reading a real directory in
// the shared `.agents/skills` slot (so its discovered entry has
// `canonical_path = None`) must not `remove_dir_all` content another
// agent's symlink still resolves to. Before the guard this returned Ok,
// deleted the Master, and left Claude's referrer dangling.
#[cfg(unix)]
#[test]
fn remove_skill_refuses_master_another_agent_links_to() {
	// `skill_store_roots` (through the guard under test) reads HOME /
	// XDG_CONFIG_HOME, so this must hold the binary's ONE env mutex — see
	// crates/core/AGENTS.md Testing.
	let _env = crate::skills::prune::test_lock::env_lock()
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();

	let mut claude = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	claude.load().unwrap();
	let mut skill = Skill::new("shared-master");
	skill.description = Some("test".to_string());
	claude.add_skill_universal(skill).unwrap();

	let master = root.join(".aghub/shared-master");
	let claude_link = root.join(".claude/skills/shared-master");
	assert!(master.join("SKILL.md").exists());
	assert!(claude_link.exists());

	// The directly-read Master is gone, but the protection it needed is not:
	// a real directory in the SHARED `.agents/skills` slot is read by up to
	// ten agents, and one agent's removal must not `remove_dir_all` it out
	// from under the rest. Put the skill exactly there.
	let shared = root.join(".agents/skills/shared-master");
	std::fs::create_dir_all(&shared).unwrap();
	std::fs::write(
		shared.join("SKILL.md"),
		"---\nname: shared-master\ndescription: test\n---\n",
	)
	.unwrap();

	let mut cursor = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	cursor.load().unwrap();
	// Precondition for the branch under test: the copy layout, i.e. the
	// `remove_dir_all` path, not the harmless unlink one.
	assert!(
		cursor
			.get_skill("shared-master")
			.expect("cursor reads the shared slot")
			.canonical_path
			.is_none(),
		"a real directory in the shared slot takes the remove_dir_all branch"
	);

	let err = cursor
		.remove_skill("shared-master")
		.expect_err("must refuse: other slot readers would lose it");
	assert!(
		matches!(err, ConfigError::UnsupportedOperation(_)),
		"unexpected error: {err:?}"
	);
	assert!(
		shared.join("SKILL.md").exists(),
		"the shared slot must survive a single agent's removal"
	);
	assert!(
		master.join("SKILL.md").exists(),
		"and so must the store Master"
	);
	assert!(
		claude_link.exists(),
		"Claude's referrer must still resolve (not dangle)"
	);
}

// The case a referrer sweep CANNOT see: a real directory in the shared
// `.agents/skills` slot with ZERO symlinks pointing at it, which up to ten
// agents read simply by scanning that directory. A
// `dir_has_external_referrer`-only guard returned Ok here and
// `remove_dir_all`'d it out from under every one of them — silent loss, with
// no dangling link left behind to notice it by.
#[cfg(unix)]
#[test]
fn remove_skill_refuses_a_shared_slot_other_agents_read() {
	// `skill_store_roots` (through the guard under test) reads HOME /
	// XDG_CONFIG_HOME, so this must hold the binary's ONE env mutex — see
	// crates/core/AGENTS.md Testing.
	let _env = crate::skills::prune::test_lock::env_lock()
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/native-shared");
	std::fs::create_dir_all(&master).unwrap();
	std::fs::write(
		master.join("SKILL.md"),
		"---\nname: native-shared\ndescription: test\n---\n",
	)
	.unwrap();
	// The point of the test: nothing links to it.
	assert!(!root.join(".claude/skills").exists());

	// A second slot reader that would lose the skill silently.
	let mut opencode = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	opencode.load().unwrap();
	assert!(opencode.get_skill("native-shared").is_some());

	let mut cursor = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	cursor.load().unwrap();
	let err = cursor
		.remove_skill("native-shared")
		.expect_err("a shared universal master is not this seam's to take");
	assert!(
		matches!(err, ConfigError::UnsupportedOperation(_)),
		"unexpected error: {err:?}"
	);
	assert!(master.join("SKILL.md").exists(), "the master must survive");
	assert!(
		opencode.get_skill("native-shared").is_some(),
		"the other NativeReader must still read it"
	);
}

// The direction that must NOT regress: a private per-agent copy (a real dir
// outside the universal roots, nothing linking into it) is still deletable
// through the seam. The guard is about shared storage, not about dirs.
#[test]
fn remove_skill_still_deletes_private_copy() {
	// `skill_store_roots` (through the guard under test) reads HOME /
	// XDG_CONFIG_HOME, so this must hold the binary's ONE env mutex — see
	// crates/core/AGENTS.md Testing.
	let _env = crate::skills::prune::test_lock::env_lock()
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let copy = root.join(".claude/skills/private-copy");
	std::fs::create_dir_all(&copy).unwrap();
	std::fs::write(
		copy.join("SKILL.md"),
		"---\nname: private-copy\ndescription: test\n---\n",
	)
	.unwrap();

	let mut claude = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	claude.load().unwrap();
	assert!(claude.get_skill("private-copy").is_some());

	claude.remove_skill("private-copy").unwrap();

	assert!(!copy.exists(), "a private copy must stay deletable");
}

// -----------------------------------------------------------------------
// P1 fix: renaming a universal skill must re-point the per-agent symlinks
// and preserve the symlink layout (canonical_path), not dangle + downgrade.
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn update_skill_universal_rename_relinks_agents_and_keeps_canonical() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("old-uni");
	skill.description = Some("universal".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// Rename old-uni -> new-uni via the update path.
	let mut renamed = Skill::new("new-uni");
	renamed.description = Some("universal".to_string());
	mgr.update_skill("old-uni", renamed).unwrap();

	// Canonical master is renamed (old gone, new present).
	assert!(root.join(".aghub/new-uni/SKILL.md").exists());
	assert!(!root.join(".aghub/old-uni").exists());

	// The old-name agent symlink is fully removed (not left dangling).
	assert!(
		std::fs::symlink_metadata(root.join(".claude/skills/old-uni")).is_err(),
		"old-name symlink must be removed, not left dangling"
	);

	// A new-name agent symlink exists and resolves to the renamed master.
	let new_link = root.join(".claude/skills/new-uni");
	let meta = std::fs::symlink_metadata(&new_link)
		.expect("new-name agent symlink must exist");
	assert!(
		meta.file_type().is_symlink(),
		"new agent path must be a symlink"
	);
	assert!(
		new_link.join("SKILL.md").exists(),
		"symlink must resolve through to the renamed master"
	);

	// The layout stays "symlink": canonical_path is preserved (not None),
	// so later layout-aware removal still classifies it correctly.
	let s = mgr.get_skill("new-uni").expect("renamed skill in config");
	assert!(
		s.canonical_path.is_some(),
		"canonical_path must be preserved on a universal rename"
	);
}

// A rename unlinks every old-name referrer BEFORE re-pointing it. If the
// new-name slot is occupied the linker reports a per-agent `conflict` and
// still returns Ok — which the rename used to discard, leaving that agent
// with NO link at all while reporting success. It must fail and roll back.
#[cfg(unix)]
#[test]
fn update_skill_rename_rolls_back_when_a_referrer_slot_is_occupied() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	let mut skill = Skill::new("old-uni");
	skill.description = Some("universal".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// A foreign directory already occupies Claude's slot for the NEW name.
	// Not a link, so the linker refuses to clobber it — and the rollback
	// below must not clobber it either.
	let occupied = root.join(".claude/skills/new-uni");
	std::fs::create_dir_all(&occupied).unwrap();
	std::fs::write(occupied.join("SKILL.md"), "FOREIGN").unwrap();

	let mut renamed = Skill::new("new-uni");
	renamed.description = Some("universal".to_string());
	mgr.update_skill("old-uni", renamed).expect_err(
		"a referrer that cannot be re-pointed must fail the rename",
	);

	// Rolled back: master back under the old name, old-name link restored
	// and resolving, no half-renamed master left behind.
	assert!(
		root.join(".aghub/old-uni/SKILL.md").exists(),
		"the master must be restored to its old name"
	);
	assert!(
		!root.join(".aghub/new-uni").exists(),
		"no master may survive under the new name"
	);
	let old_link = root.join(".claude/skills/old-uni");
	assert!(
		old_link
			.symlink_metadata()
			.is_ok_and(|m| m.file_type().is_symlink()),
		"the old-name referrer must be restored as a link"
	);
	assert!(
		old_link.join("SKILL.md").exists(),
		"the restored referrer must resolve, not dangle"
	);
	// The occupant is someone else's: never touched by either direction.
	assert_eq!(
		std::fs::read_to_string(occupied.join("SKILL.md")).unwrap(),
		"FOREIGN"
	);
}

// A SKILL.md's frontmatter belongs to its author. aghub's model carries five
// keys of it, and `update_skill` reserializes the file from that model — so
// without preservation every edit (and, since the rename went transactional,
// every rename) silently deleted `license`, `compatibility`, and anything
// else the author wrote.
#[test]
fn update_skill_preserves_frontmatter_keys_aghub_does_not_model() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let dir = root.join(".claude/skills/keeper");
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		"---\nname: keeper\ndescription: before\nauthor: someone\n\
		 license: MIT\nmetadata:\n  team: platform\n  tier: 2\n---\n\nBODY\n",
	)
	.unwrap();

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	let mut updated = mgr.get_skill("keeper").unwrap().clone();
	updated.description = Some("after".to_string());
	// Clearing a MODELED key must still clear it — preservation may not
	// resurrect a field the caller deliberately dropped.
	updated.author = None;
	mgr.update_skill("keeper", updated).unwrap();

	let md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
	assert!(md.contains("license: MIT"), "unmodeled scalar lost: {md}");
	assert!(md.contains("team: platform"), "unmodeled map lost: {md}");
	assert!(md.contains("tier: 2"), "unmodeled map lost: {md}");
	assert!(md.contains("description: after"), "edit not applied: {md}");
	assert!(
		!md.contains("author:"),
		"a cleared modeled key must not come back: {md}"
	);
	assert!(md.contains("BODY"), "body lost: {md}");
}

#[cfg(unix)]
#[test]
fn update_skill_rename_refuses_when_target_dir_already_exists() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("collide-old");
	skill.description = Some("u".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// A DIFFERENT (e.g. another skill's) master already occupies the target
	// name. `fs::rename` onto an empty target dir would silently succeed and
	// clobber it; the rename must refuse instead.
	std::fs::create_dir_all(root.join(".aghub/collide-new")).unwrap();

	let res = mgr.update_skill("collide-old", Skill::new("collide-new"));

	assert!(res.is_err(), "must refuse to rename onto an existing dir");
	assert!(
		root.join(".aghub/collide-old/SKILL.md").exists(),
		"the original master must be preserved on conflict"
	);
}

/// Whether 0o555 perms actually block writes for this process. Returns false
/// when running as root (perm bits are bypassed), so permission-injection
/// tests can skip instead of failing spuriously in root CI.
#[cfg(unix)]
fn perms_enforced(under: &std::path::Path) -> bool {
	use std::os::unix::fs::PermissionsExt;
	let probe = under.join(".perm-probe");
	std::fs::create_dir(&probe).unwrap();
	std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o555))
		.unwrap();
	let blocked = std::fs::write(probe.join("x"), b"x").is_err();
	std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755))
		.unwrap();
	std::fs::remove_dir_all(&probe).ok();
	blocked
}

#[cfg(unix)]
#[test]
fn update_skill_universal_rename_rolls_back_when_relink_fails() {
	use crate::create_adapter;
	use crate::models::AgentType;
	use std::os::unix::fs::PermissionsExt;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	if !perms_enforced(root) {
		eprintln!("skipping: 0o555 not enforced (running as root)");
		return;
	}

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("roll-old");
	skill.description = Some("u".to_string());
	mgr.add_skill_universal(skill).unwrap();

	let referrer_dir = root.join(".claude/skills");
	assert!(
		std::fs::symlink_metadata(referrer_dir.join("roll-old"))
			.unwrap()
			.file_type()
			.is_symlink(),
		"precondition: referrer symlink exists"
	);

	// Make the referrer dir read-only so the relink fails AFTER the master
	// has been renamed — the partial-failure window the rollback must close.
	let orig = std::fs::metadata(&referrer_dir).unwrap().permissions();
	std::fs::set_permissions(
		&referrer_dir,
		std::fs::Permissions::from_mode(0o555),
	)
	.unwrap();

	let res = mgr.update_skill("roll-old", Skill::new("roll-new"));

	// Restore perms before asserting so tempdir teardown always works.
	std::fs::set_permissions(&referrer_dir, orig).unwrap();

	let err = res.expect_err("a failed relink must surface as an error");
	// The rollback SUCCEEDS here (the master is renamed back and
	// `link_agents_to_canonical` folds per-link failures into its report
	// rather than erroring), so the original relink failure is returned
	// UNCHANGED — not re-wrapped as a recovery hint. The message must name
	// the stale link it failed on and must NOT claim manual restore.
	let msg = err.to_string();
	assert!(
		msg.contains("roll-old"),
		"the original relink error must name the failing link: {msg}"
	);
	assert!(
		!msg.contains("move them back"),
		"a recovered rollback must not emit ManualRestore wording: {msg}"
	);
	assert!(
		root.join(".aghub/roll-old/SKILL.md").exists(),
		"rollback must rename the master back to its old name"
	);
	assert!(
		!root.join(".aghub/roll-new").exists(),
		"no half-renamed master may be left behind"
	);
	let link = referrer_dir.join("roll-old");
	assert!(
		std::fs::symlink_metadata(&link)
			.map(|m| m.file_type().is_symlink())
			.unwrap_or(false),
		"the surviving referrer symlink must remain"
	);
	assert!(
		link.join("SKILL.md").exists(),
		"the referrer must still resolve to the master (not dangling)"
	);
}

#[cfg(unix)]
#[test]
fn update_skill_universal_rename_rollback_restores_removed_referrer() {
	use crate::create_adapter;
	use crate::models::AgentType;
	use std::os::unix::fs::PermissionsExt;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	if !perms_enforced(root) {
		eprintln!("skipping: 0o555 not enforced (running as root)");
		return;
	}

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("roll2-old");
	skill.description = Some("u".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// Add a SECOND referrer in RooCode's project skills dir (it sorts after
	// Claude in AgentType::ALL, so Claude's symlink is removed first, then
	// RooCode's removal fails — forcing the rollback to RESTORE Claude's
	// already-removed symlink).
	//
	// That ordering is the whole point of the test, and roster order is a
	// knob `agent_roster!` invites you to turn. Flip it and RooCode's
	// unlink fails BEFORE Claude's symlink is ever touched, so the
	// restore assertion below passes on a link that was never removed —
	// green, testing nothing. Pin the precondition rather than the row.
	let pos = |agent: AgentType| {
		AgentType::ALL.iter().position(|a| *a == agent).unwrap()
	};
	assert!(
		pos(AgentType::Claude) < pos(AgentType::RooCode),
		"this test needs Claude's referrer removed before RooCode's — \
		 the roster now puts RooCode first, so the rollback it exercises \
		 never happens"
	);

	let master = root.join(".aghub/roll2-old");
	let roo_dir = root.join(".roo/skills");
	std::fs::create_dir_all(&roo_dir).unwrap();
	std::os::unix::fs::symlink(&master, roo_dir.join("roll2-old")).unwrap();
	assert_eq!(
		std::fs::canonicalize(roo_dir.join("roll2-old")).unwrap(),
		std::fs::canonicalize(&master).unwrap(),
		"precondition: second referrer resolves to the master"
	);

	let claude_dir = root.join(".claude/skills");
	let roo_orig = std::fs::metadata(&roo_dir).unwrap().permissions();
	std::fs::set_permissions(&roo_dir, std::fs::Permissions::from_mode(0o555))
		.unwrap();

	let res = mgr.update_skill("roll2-old", Skill::new("roll2-new"));

	std::fs::set_permissions(&roo_dir, roo_orig).unwrap();

	assert!(res.is_err(), "a failed relink must surface as an error");
	assert!(
		master.join("SKILL.md").exists(),
		"rollback must rename the master back to its old name"
	);
	// The FIRST referrer (Claude) had its symlink removed before the failure;
	// rollback must have recreated it pointing back at the master.
	let claude_link = claude_dir.join("roll2-old");
	assert!(
		std::fs::symlink_metadata(&claude_link)
			.map(|m| m.file_type().is_symlink())
			.unwrap_or(false),
		"the removed referrer symlink must be restored"
	);
	assert!(
		claude_link.join("SKILL.md").exists(),
		"the restored referrer must resolve to the master"
	);
}

// -----------------------------------------------------------------------
// T2 (#8): structured rollback reason via RecoveryHint.
// -----------------------------------------------------------------------

/// Rollback's own master-restore rename fails (its parent is read-only), so
/// the master is the ONLY surviving copy and stays at `new_master`. The
/// error must carry RecoveryHint::ManualRestore wording naming BOTH the
/// recover-from (new_master) and restore-to (old_master) paths plus an
/// actionable next step. Driven through the real `rollback_master_rename`.
#[cfg(unix)]
#[test]
fn rename_rollback_failure_reports_manual_restore() {
	use std::os::unix::fs::PermissionsExt;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	if !perms_enforced(root) {
		eprintln!("skipping: 0o555 not enforced (running as root)");
		return;
	}

	// The renamed master holds the only copy of the contents.
	let skills_dir = root.join(".agents/skills");
	let old_master = skills_dir.join("recover-old");
	let new_master = skills_dir.join("recover-new");
	std::fs::create_dir_all(&new_master).unwrap();
	std::fs::write(new_master.join("SKILL.md"), "real").unwrap();

	// Read-only parent: the rollback's `rename(new_master, old_master)`
	// cannot create the old-name entry, so the restore step itself fails.
	let orig = std::fs::metadata(&skills_dir).unwrap().permissions();
	std::fs::set_permissions(
		&skills_dir,
		std::fs::Permissions::from_mode(0o555),
	)
	.unwrap();

	let err = rollback_master_rename(
		&new_master,
		&old_master,
		&[],
		"recover-new",
		false,
		ConfigError::Io(std::io::Error::other("relink boom")),
	);

	// Restore perms before asserting so tempdir teardown always works.
	std::fs::set_permissions(&skills_dir, orig).unwrap();

	let msg = err.to_string();
	assert!(
		msg.contains(&new_master.display().to_string()),
		"recover_from (new_master) path missing from: {msg}"
	);
	assert!(
		msg.contains(&old_master.display().to_string()),
		"restore_to (old_master) path missing from: {msg}"
	);
	assert!(
		msg.contains("relink boom"),
		"the original relink failure must still be named: {msg}"
	);
	assert!(
		msg.contains("move them back"),
		"missing ManualRestore next step in: {msg}"
	);
	// The master must still be the renamed copy (rollback could not move it).
	assert!(
		new_master.join("SKILL.md").exists(),
		"the only surviving copy must remain at new_master"
	);
}

/// Rollback restores the master successfully, but a leftover new-name
/// referrer symlink can't be removed (its dir is read-only), so the relink
/// step of the rollback fails. Data is safe (master is back at old_master);
/// the error must report RecoveryHint::BrokenSymlink for the offending link,
/// NOT ManualRestore.
#[cfg(unix)]
#[test]
fn rename_rollback_relink_failure_reports_broken_symlink() {
	use std::os::unix::fs::PermissionsExt;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	if !perms_enforced(root) {
		eprintln!("skipping: 0o555 not enforced (running as root)");
		return;
	}

	let skills_dir = root.join(".agents/skills");
	let old_master = skills_dir.join("brk-old");
	let new_master = skills_dir.join("brk-new");
	std::fs::create_dir_all(&new_master).unwrap();
	std::fs::write(new_master.join("SKILL.md"), "real").unwrap();

	// A referrer dir holding a stale NEW-name symlink the rollback must
	// remove; the dir is read-only so the `unlink` fails AFTER the master
	// is renamed back.
	let referrer = root.join(".claude/skills");
	std::fs::create_dir_all(&referrer).unwrap();
	std::os::unix::fs::symlink(&new_master, referrer.join("brk-new")).unwrap();
	let orig = std::fs::metadata(&referrer).unwrap().permissions();
	std::fs::set_permissions(&referrer, std::fs::Permissions::from_mode(0o555))
		.unwrap();

	let err = rollback_master_rename(
		&new_master,
		&old_master,
		std::slice::from_ref(&referrer),
		"brk-new",
		false,
		ConfigError::Io(std::io::Error::other("relink boom")),
	);

	// Restore perms before asserting so tempdir teardown always works.
	std::fs::set_permissions(&referrer, orig).unwrap();

	let msg = err.to_string();
	assert!(
		msg.contains("broken link"),
		"missing BrokenSymlink next step in: {msg}"
	);
	assert!(
		msg.contains(&referrer.join("brk-new").display().to_string()),
		"the offending link path must be named: {msg}"
	);
	assert!(
		!msg.contains("move them back"),
		"a restored master must not emit ManualRestore wording: {msg}"
	);
	// The master must be safely back at its old name.
	assert!(
		old_master.join("SKILL.md").exists(),
		"rollback must have restored the master to old_master"
	);
	assert!(
		!new_master.exists(),
		"no half-renamed master may survive a recovered rollback"
	);
}

// -----------------------------------------------------------------------
// P1 fix (Windows): junction referrer rename/relink + rollback
// -----------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn update_skill_universal_rename_relinks_junction_and_keeps_canonical() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = std::fs::canonicalize(tmp.path()).unwrap();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(&root),
	);
	mgr.load().unwrap();

	// Install universal skill (writes Master + creates a junction
	// referrer in .claude/skills/ via Linker::link which falls through
	// to create_junction when symlink_dir is unavailable).
	let mut skill = Skill::new("old-uni-win");
	skill.description = Some("universal".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// Rename old-uni-win -> new-uni-win via the update path.
	let mut renamed = Skill::new("new-uni-win");
	renamed.description = Some("universal".to_string());
	mgr.update_skill("old-uni-win", renamed).unwrap();

	// Canonical master is renamed (old gone, new present). The store is
	// `.aghub`, not `.agents\skills` — that slot is now an ordinary
	// Referrer directory. Windows-only, so `just preflight` never ran this
	// and the stale path survived the store move.
	assert!(
		root.join(".aghub\\new-uni-win\\SKILL.md").exists(),
		"renamed master must exist"
	);
	assert!(
		!root.join(".aghub\\old-uni-win").exists(),
		"old master must be gone"
	);

	// The old-name referrer link is fully removed (not left dangling).
	assert!(
		std::fs::symlink_metadata(root.join(".claude\\skills\\old-uni-win"))
			.is_err(),
		"old-name junction must be removed, not left dangling"
	);

	// A new-name referrer exists and resolves to the renamed master.
	let new_link = root.join(".claude\\skills\\new-uni-win");
	assert!(
		crate::skills::linker::Linker::is_link(&new_link),
		"new-name referrer must be a reparse point"
	);
	assert!(
		new_link.join("SKILL.md").exists(),
		"junction must resolve through to the renamed master"
	);

	// canonical_path is preserved.
	let s = mgr
		.get_skill("new-uni-win")
		.expect("renamed skill in config");
	assert!(
		s.canonical_path.is_some(),
		"canonical_path must be preserved on a universal rename"
	);
}

// -----------------------------------------------------------------------
// P1-B fix: add_skill_universal silently overwrites existing canonical
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn add_skill_universal_does_not_overwrite_existing_canonical() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// Manually pre-create the canonical with old content
	let canonical = root.join(".aghub/preexist");
	std::fs::create_dir_all(&canonical).unwrap();
	std::fs::write(
		canonical.join("SKILL.md"),
		"---\nname: preexist\ndescription: old\n---\nOld content.\n",
	)
	.unwrap();

	// Install a skill with the same sanitized name
	let mut skill = Skill::new("preexist");
	skill.description = Some("new".to_string());
	mgr.add_skill_universal(skill).unwrap();

	// The SKILL.md should NOT have been overwritten
	let content = std::fs::read_to_string(canonical.join("SKILL.md")).unwrap();
	assert!(
		content.contains("Old content."),
		"SKILL.md was overwritten: {content}"
	);

	// Symlink should still be created (idempotent)
	let link = root.join(".claude/skills/preexist");
	assert!(std::fs::symlink_metadata(&link)
		.unwrap()
		.file_type()
		.is_symlink());
}

#[cfg(unix)]
#[test]
fn add_skill_universal_fresh_install_writes_canonical() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let mut skill = Skill::new("fresh");
	skill.description = Some("fresh install".to_string());
	mgr.add_skill_universal(skill).unwrap();

	let canonical = root.join(".aghub/fresh/SKILL.md");
	assert!(canonical.exists());
	let content = std::fs::read_to_string(&canonical).unwrap();
	assert!(content.contains("fresh install"));

	let link = root.join(".claude/skills/fresh");
	assert!(std::fs::symlink_metadata(&link)
		.unwrap()
		.file_type()
		.is_symlink());
}

// -----------------------------------------------------------------------
// P0-A fix: add_skill_from_path_universal dropped all non-SKILL.md assets
// -----------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn add_skill_from_path_universal_copies_full_source_tree() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// Create a source skill with assets
	let src = root.join("src/my-skill");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: my-skill\ndescription: test\n---\nBody.\n",
	)
	.unwrap();
	std::fs::create_dir_all(src.join("assets")).unwrap();
	std::fs::write(src.join("assets/data.json"), "{}").unwrap();
	std::fs::create_dir_all(src.join("scripts")).unwrap();
	std::fs::write(src.join("scripts/setup.sh"), "#!/bin/sh\necho ok").unwrap();

	let added = mgr.add_skill_from_path_universal(&src, None).unwrap();
	assert_eq!(added.skill.name, "my-skill");
	assert!(!added.already_installed);

	// Canonical should have the full tree
	let canonical = root.join(".aghub/my-skill");
	assert!(canonical.join("SKILL.md").exists());
	assert!(canonical.join("assets/data.json").exists());
	assert!(canonical.join("scripts/setup.sh").exists());
	assert_eq!(
		std::fs::read_to_string(canonical.join("assets/data.json")).unwrap(),
		"{}"
	);

	// Agent dir should be a symlink
	let link = root.join(".claude/skills/my-skill");
	assert!(std::fs::symlink_metadata(&link)
		.unwrap()
		.file_type()
		.is_symlink());

	// Reading assets via the symlink should work
	assert_eq!(
		std::fs::read_to_string(link.join("assets/data.json")).unwrap(),
		"{}"
	);
}

#[cfg(unix)]
#[test]
fn add_skill_from_path_universal_accepts_skill_md_file() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// Pass SKILL.md file directly (should use parent as source root)
	let src = root.join("src/other-skill");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: other-skill\ndescription: test\n---\n",
	)
	.unwrap();
	std::fs::write(src.join("extra.txt"), "bonus").unwrap();

	let skill_md = src.join("SKILL.md");
	let added = mgr.add_skill_from_path_universal(&skill_md, None).unwrap();
	assert_eq!(added.skill.name, "other-skill");
	assert!(!added.already_installed);

	// extra.txt should have been copied to canonical
	let canonical = root.join(".aghub/other-skill");
	assert!(canonical.join("extra.txt").exists());
	assert_eq!(
		std::fs::read_to_string(canonical.join("extra.txt")).unwrap(),
		"bonus"
	);
}

// A `--name` install rewrites the copied Master's frontmatter `name:`. That
// is only ever ITS OWN copy: a Master this call did not create belongs to
// someone else, and rewriting it would change the folder hash the npx lock
// contract is checked against. Here `.aghub/renamed` exists but is
// discovered under a DIFFERENT name, so the duplicate check cannot see it
// and only the materializer's `created_master` receipt catches it.
#[cfg(unix)]
#[test]
fn add_skill_from_path_universal_refuses_to_rename_onto_a_foreign_master() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let foreign = root.join(".aghub/renamed");
	std::fs::create_dir_all(&foreign).unwrap();
	std::fs::write(
		foreign.join("SKILL.md"),
		"---\nname: something-else\ndescription: foreign\n---\nFOREIGN\n",
	)
	.unwrap();

	let src = root.join("src/fresh");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: fresh\ndescription: mine\n---\nMINE\n",
	)
	.unwrap();

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	mgr.add_skill_from_path_universal(&src, Some("renamed"))
		.expect_err("a Master this call did not create is not ours to edit");

	let content = std::fs::read_to_string(foreign.join("SKILL.md")).unwrap();
	assert!(
		content.contains("name: something-else") && content.contains("FOREIGN"),
		"the foreign master must be untouched: {content}"
	);
	// The link the materializer created on the way in is undone, so the
	// refusal leaves nothing of this call behind.
	assert!(
		root.join(".claude/skills/renamed")
			.symlink_metadata()
			.is_err(),
		"the referrer this call created must be undone"
	);
}

#[cfg(unix)]
#[test]
fn add_skill_from_path_universal_does_not_overwrite_existing_canonical() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();

	// Pre-create canonical with old content
	let canonical = root.join(".aghub/shared-skill");
	std::fs::create_dir_all(&canonical).unwrap();
	std::fs::write(
		canonical.join("SKILL.md"),
		"---\nname: shared-skill\ndescription: old\n---\nOld version.\n",
	)
	.unwrap();

	// Source has updated content
	let src = root.join("src/shared-skill");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: shared-skill\ndescription: new\n---\nNew version.\n",
	)
	.unwrap();

	// Claude installs from path — canonical already exists, should NOT
	// be overwritten
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	mgr.add_skill_from_path_universal(&src, None).unwrap();

	let content = std::fs::read_to_string(canonical.join("SKILL.md")).unwrap();
	assert!(
		content.contains("Old version."),
		"Canonical should not be overwritten: {content}"
	);

	// Cursor must NOT discover the skill. It was installed for another
	// agent, and Cursor now has its own Referrer directory rather than
	// reading the store. This assertion used to say the opposite — it
	// pinned the leak as a feature.
	let mut mgr2 = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr2.load().unwrap();
	assert!(
		!mgr2
			.config
			.as_ref()
			.unwrap()
			.skills
			.iter()
			.any(|s| s.name == "shared-skill"),
		"Cursor was never granted shared-skill and must not see it"
	);
}

// -----------------------------------------------------------------------
// Task 5: remove_skill_planned owns the post-delete lock prune.
// -----------------------------------------------------------------------

// Reuse the ONE shared global-lock guard so these tests serialize on the
// same mutex as the prune.rs tests (separate static LOCKs would race on the
// shared XDG_STATE_HOME global lock when the whole suite runs in-process).
use crate::skills::prune::test_lock::GlobalLockGuard;

fn locked_entry() -> skill::SkillLockEntry {
	skill::SkillLockEntry {
		source: "o/r".to_string(),
		source_type: "github".to_string(),
		source_url: "https://github.com/o/r".to_string(),
		ref_name: None,
		skill_path: None,
		skill_folder_hash: "h".to_string(),
		content_hash: None,
		ref_commit: None,
		installed_at: "t".to_string(),
		updated_at: "t".to_string(),
		plugin_name: None,
	}
}

#[test]
fn remove_skill_planned_prunes_lock_on_execute() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let _g = GlobalLockGuard::new();
	let tmp = tempfile::tempdir().unwrap();
	let skills_dir = tmp.path().join("skills");
	std::fs::create_dir_all(&skills_dir).unwrap();
	// A real skill on disk so execute actually deletes something.
	let skill_dir = skills_dir.join("prune-me-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: prune-me-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);

	// Lock entry for the skill being removed (gets pruned once gone) plus an
	// orphan that is never on disk (also pruned).
	skill::lock::add_skill_to_lock("prune-me-skill", locked_entry()).unwrap();
	skill::lock::add_skill_to_lock("orphan-never-on-disk-xyz", locked_entry())
		.unwrap();

	let mut mgr =
		ConfigManager::new(create_adapter(AgentType::Claude), true, None);
	mgr.load().unwrap();

	let outcome = mgr
		.remove_skill_planned("prune-me-skill", false, false, true)
		.unwrap();

	crate::adapter::set_skills_path_override("claude", None);

	assert!(outcome.executed, "copy single-agent removal executes");
	let pruned = match &outcome.prune {
		crate::skills::removal::PruneStatus::Pruned(keys) => keys,
		other => panic!("prune must run on execute, got {other:?}"),
	};
	// The reported keys must name the orphan that was actually dropped — not
	// just "some prune ran". The removed skill is disk-derived and may be
	// gone from the in-memory view before the scan, but the never-on-disk
	// orphan must always be reported as pruned.
	assert!(
		pruned.contains(&"orphan-never-on-disk-xyz".to_string()),
		"reported pruned keys must include the dropped orphan, got {pruned:?}"
	);
	let lock = skill::read_skill_lock();
	assert!(
		!lock.skills.contains_key("prune-me-skill"),
		"removed skill's lock entry must be pruned"
	);
	assert!(
		!lock.skills.contains_key("orphan-never-on-disk-xyz"),
		"orphan lock entry must be pruned"
	);
}

/// Regression for the `PruneStatus::Failed` path through the REAL manager
/// (not synthetic `prune_status`/`combine_prune` inputs): force the
/// post-delete lock write to fail and assert the skill is still deleted, the
/// lock is left unchanged, and the outcome is `Failed { reason, pruned }`. A
/// prune failure is non-fatal — deletion already happened.
#[cfg(unix)]
#[test]
fn remove_skill_planned_failed_prune_keeps_lock_and_deletes_skill() {
	use crate::create_adapter;
	use crate::models::AgentType;
	use std::os::unix::fs::PermissionsExt;

	let _g = GlobalLockGuard::new();
	// GlobalLockGuard points XDG_STATE_HOME at a fresh temp dir; the lock
	// lives at $XDG_STATE_HOME/skills/.skill-lock.json.
	let state = std::env::var("XDG_STATE_HOME").unwrap();
	let lock_dir = std::path::Path::new(&state).join("skills");

	let tmp = tempfile::tempdir().unwrap();
	let skills_dir = tmp.path().join("skills");
	let skill_dir = skills_dir.join("fail-prune-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: fail-prune-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);

	// Seed an orphan a successful prune WOULD drop, then make the lock dir
	// read-only so the prune's atomic temp+rename write fails (Io error).
	skill::lock::add_skill_to_lock("orphan-never-on-disk-xyz", locked_entry())
		.unwrap();
	if !perms_enforced(&lock_dir) {
		crate::adapter::set_skills_path_override("claude", None);
		eprintln!("skip: perms not enforced (root)");
		return;
	}
	let orig = std::fs::metadata(&lock_dir).unwrap().permissions();
	std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(0o555))
		.unwrap();

	let mut mgr =
		ConfigManager::new(create_adapter(AgentType::Claude), true, None);
	mgr.load().unwrap();
	let outcome = mgr
		.remove_skill_planned("fail-prune-skill", false, false, true)
		.unwrap();

	// RESTORE perms before any assertion so a failed assert never leaks an
	// unremovable temp dir.
	std::fs::set_permissions(&lock_dir, orig).unwrap();
	crate::adapter::set_skills_path_override("claude", None);

	assert!(outcome.executed, "deletion runs even if the prune fails");
	assert!(
		!skill_dir.exists(),
		"the skill is deleted before the prune is attempted"
	);
	match outcome.prune {
		crate::skills::removal::PruneStatus::Failed { reason, pruned } => {
			assert!(!reason.is_empty(), "failure reason is reported");
			assert!(
				pruned.is_empty(),
				"single-scope write failure drops nothing: {pruned:?}"
			);
		}
		other => panic!("expected Failed, got {other:?}"),
	}
	let lock = skill::read_skill_lock();
	assert!(
		lock.skills.contains_key("orphan-never-on-disk-xyz"),
		"a failed prune must leave the lock unchanged"
	);
}

#[test]
fn remove_skill_planned_dry_run_discloses_prune_without_writing() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let _g = GlobalLockGuard::new();
	let tmp = tempfile::tempdir().unwrap();
	let skills_dir = tmp.path().join("skills");
	std::fs::create_dir_all(&skills_dir).unwrap();
	let skill_dir = skills_dir.join("keep-me-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: keep-me-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);

	// Orphan present in the lock: a real prune WOULD drop it; a dry-run must
	// not, proving prune never ran.
	skill::lock::add_skill_to_lock("orphan-never-on-disk-xyz", locked_entry())
		.unwrap();

	let mut mgr =
		ConfigManager::new(create_adapter(AgentType::Claude), true, None);
	mgr.load().unwrap();

	let outcome = mgr
		.remove_skill_planned("keep-me-skill", false, true, false)
		.unwrap();

	crate::adapter::set_skills_path_override("claude", None);

	assert!(!outcome.executed, "dry-run must not delete");
	// A dry-run DISCLOSES what a commit would prune, and writes nothing.
	// This asserted `NotRun`, which conflated "did not run it" with "will
	// not tell you about it" — the caller could not see whose provenance a
	// commit was about to discard. The load-bearing invariant, that the
	// lock is untouched, is asserted below and is unchanged.
	assert_eq!(
		outcome.prune,
		crate::skills::removal::PruneStatus::WouldPrune(vec![
			"orphan-never-on-disk-xyz".to_string()
		]),
		"a dry-run must disclose the orphan a commit would drop"
	);
	let lock = skill::read_skill_lock();
	assert!(
		lock.skills.contains_key("orphan-never-on-disk-xyz"),
		"dry-run must not prune the lock"
	);
}

/// The confirm-gated branch (destructive op, NOT yet confirmed) is also a
/// non-executed path: it must leave `prune == NotRun` and the lock untouched,
/// exactly like a dry-run. Distinct from the dry-run test because the gate is
/// `needs_confirm && !confirm` (all-agents), not `dry_run`.
#[test]
fn remove_skill_planned_unconfirmed_discloses_prune_without_writing() {
	use crate::create_adapter;
	use crate::models::AgentType;

	let _g = GlobalLockGuard::new();
	let tmp = tempfile::tempdir().unwrap();
	let skills_dir = tmp.path().join("skills");
	let skill_dir = skills_dir.join("gated-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: gated-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);
	// Orphan a successful prune WOULD drop — proves the gated branch never
	// reaches the prune.
	skill::lock::add_skill_to_lock("orphan-never-on-disk-xyz", locked_entry())
		.unwrap();

	let mut mgr =
		ConfigManager::new(create_adapter(AgentType::Claude), true, None);
	mgr.load().unwrap();
	// all_agents=true => needs_confirm; confirm=false => gated, not executed.
	let outcome = mgr
		.remove_skill_planned("gated-skill", true, false, false)
		.unwrap();

	crate::adapter::set_skills_path_override("claude", None);

	assert!(!outcome.executed, "unconfirmed destructive op must not run");
	// Same as the dry-run above: disclose, write nothing.
	assert_eq!(
		outcome.prune,
		crate::skills::removal::PruneStatus::WouldPrune(vec![
			"orphan-never-on-disk-xyz".to_string()
		]),
		"an unconfirmed destructive op must disclose, not stay silent"
	);
	assert!(skill_dir.exists(), "gated op must not delete");
	let lock = skill::read_skill_lock();
	assert!(
		lock.skills.contains_key("orphan-never-on-disk-xyz"),
		"gated op must not prune the lock"
	);
}

// The pure combine_prune / prune_status folds now live with the
// prune_lock_for_scope seam they feed (crate::skills::prune tests).
// The cases below exercise the seam through the REAL manager.
use crate::skills::removal::PruneStatus;

#[test]
fn remove_skill_planned_project_scope_without_root_leaves_prune_notrun() {
	// ProjectOnly scope with no project root: the manager must NOT attempt a
	// project prune (it has no lock to reconcile) — matching the old caller
	// behavior. Prune is NotRun and the global lock is untouched even though
	// an orphan sits in it.
	use crate::create_adapter;
	use crate::models::AgentType;

	let _g = GlobalLockGuard::new();
	let tmp = tempfile::tempdir().unwrap();
	let skills_dir = tmp.path().join("skills");
	let skill_dir = skills_dir.join("proj-no-root-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: proj-no-root-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);
	skill::lock::add_skill_to_lock("orphan-never-on-disk-xyz", locked_entry())
		.unwrap();

	// global=false, project_root=None => ResourceScope::ProjectOnly, no root.
	let mut mgr =
		ConfigManager::new(create_adapter(AgentType::Claude), false, None);
	mgr.load().unwrap();
	let outcome = mgr
		.remove_skill_planned("proj-no-root-skill", false, false, true)
		.unwrap();

	crate::adapter::set_skills_path_override("claude", None);

	assert!(outcome.executed, "removal still executes");
	assert_eq!(
		outcome.prune,
		PruneStatus::NotRun,
		"project prune without a root must be NotRun, got {:?}",
		outcome.prune
	);
	let lock = skill::read_skill_lock();
	assert!(
		lock.skills.contains_key("orphan-never-on-disk-xyz"),
		"no prune ran, so the global lock is untouched"
	);
}

fn local_entry() -> skill::lock::local::LocalSkillLockEntry {
	skill::lock::local::LocalSkillLockEntry {
		source_url: None,
		source: "o/r".to_string(),
		ref_name: None,
		source_type: "github".to_string(),
		computed_hash: "h".to_string(),
		skill_path: None,
		ref_commit: None,
	}
}

/// Seed a Master in the store with a frontmatter name of its own.
#[cfg(test)]
fn seed_master(dir: &std::path::Path, name: &str) {
	std::fs::create_dir_all(dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: fixture\n---\n"),
	)
	.unwrap();
}

/// An exhaustive cleanup that KEPT everything must not commit.
///
/// The planner keeps the Master when its sweep hits an entry it cannot
/// resolve, and `blocks` cannot always see that: `read_effect_after` stops
/// at a directory whose root `SKILL.md` parses, while the planner's sweep
/// recurses INTO it — so a broken link nested inside an unrelated healthy
/// skill folder leaves `effect.incomplete` false and `survivors` empty.
/// That fell through to `commit`, which reported `kept` with
/// `executed: true` and ran the scope-wide lock GC the preview had just
/// promised would not run, dropping an UNRELATED skill's source
/// provenance on a delete that removed nothing.
///
/// Revert proof: drop the `|| (all_agents && plan.shared_master_kept &&
/// plan.paths.is_empty())` disjunct in `remove_skill_planned` and
/// `second-orphan` disappears from the lock while `executed` flips true.
#[test]
#[cfg(unix)]
fn all_agents_keep_previews_instead_of_running_an_undisclosed_prune() {
	use crate::dto::removal::{RemovalKind, RemovalView};
	use crate::skills::removal::PruneStatus;
	use crate::{create_adapter, models::AgentType};
	let _env = crate::skills::prune::test_lock::env_lock().lock().unwrap();
	let project = tempfile::tempdir().unwrap();
	let root = project.path();

	let master = root.join(".aghub/orphan");
	seed_master(&master, "orphan");

	// An unrelated, perfectly healthy skill in an in-scope agent dir,
	// holding a NESTED link whose canonicalize fails with ENOTDIR — not
	// NotFound, so the planner must keep the Master.
	let healthy = root.join(".claude/skills/healthy");
	seed_master(&healthy, "healthy");
	let plain = root.join("regular-file");
	std::fs::write(&plain, "f").unwrap();
	std::os::unix::fs::symlink(plain.join("y"), healthy.join("broken"))
		.unwrap();

	for key in ["orphan", "second-orphan"] {
		skill::lock::local::add_skill_to_local_lock(
			key,
			local_entry(),
			Some(root),
		)
		.unwrap();
	}

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	manager.load().unwrap();

	let preview = manager
		.remove_skill_planned("orphan", true, true, false)
		.unwrap();
	// Fixture assertion first, so the test cannot pass vacuously.
	assert!(
		preview.plan.shared_master_kept && preview.plan.paths.is_empty(),
		"fixture must reach the planner's keep, got {:?}",
		preview.plan
	);
	assert!(matches!(preview.prune, PruneStatus::NotRun));

	let outcome = manager
		.remove_skill_planned("orphan", true, false, true)
		.unwrap();
	assert!(
		!outcome.executed,
		"a confirmed run that takes nothing must not execute"
	);
	assert!(matches!(
		RemovalView::from_outcome(&outcome, false).outcome,
		RemovalKind::Kept
	));
	assert!(
		matches!(outcome.prune, PruneStatus::NotRun),
		"the preview promised no prune; the commit must not run one, got \
		 {:?}",
		outcome.prune
	);
	assert!(master.exists(), "nothing was removed");
	let lock = skill::lock::local::read_local_lock(Some(root));
	assert!(
		lock.skills.contains_key("second-orphan"),
		"an unrelated skill's source provenance must survive a delete \
		 that removed nothing"
	);
	assert!(lock.skills.contains_key("orphan"));
}

/// A failed `apply-update` leaves `.aghub-backup-<pid>-<n>/target/` beside
/// the Master, holding a full `SKILL.md` with the SAME frontmatter name.
/// Every enumerator of the store must skip aghub's own bookkeeping, or the
/// delete target is picked by `read_dir` order.
///
/// Revert proof: drop the `is_store_bookkeeping` skip in `collect_skills`
/// and the store yields two Masters, so the fallback refuses instead.
#[test]
fn a_retained_update_backup_is_not_a_second_master() {
	use crate::{create_adapter, models::AgentType};
	let _env = crate::skills::prune::test_lock::env_lock().lock().unwrap();
	let project = tempfile::tempdir().unwrap();
	let root = project.path();

	let master = root.join(".aghub/twinned");
	seed_master(&master, "twinned");
	let backup = root.join(".aghub/.aghub-backup-4242-0/target");
	seed_master(&backup, "twinned");
	let staged = root.join(".aghub/.aghub-stage-4242-0/skill");
	seed_master(&staged, "twinned");

	skill::lock::local::add_skill_to_local_lock(
		"twinned",
		local_entry(),
		Some(root),
	)
	.unwrap();

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	manager.load().unwrap();

	let outcome = manager
		.remove_skill_planned("twinned", true, false, true)
		.unwrap();
	assert!(outcome.executed);
	assert!(!master.exists(), "the Master is what a cleanup collects");
	assert!(
		backup.exists() && staged.exists(),
		"aghub's own bookkeeping is not a Master and is never deleted"
	);
}

/// Two live Masters under ONE frontmatter name must be refused, not picked
/// by `read_dir` order.
///
/// `skill_for_planned_removal`'s fallback used `.find()`, so whichever
/// entry the filesystem happened to list first became the delete target —
/// on a DESTRUCTIVE path, with the other copy left behind and the outcome
/// reported as `removed`. `resync::refuse_conflicting_copy` already refuses
/// this shape for updates; a delete has even less licence to guess.
///
/// Revert proof: replace the `(Some(first), Some(second))` arm in
/// `skill_for_planned_removal` with a plain `.find()` and this goes green
/// the wrong way — one of the two directories is deleted and `executed` is
/// true.
#[test]
fn two_masters_under_one_name_are_refused_not_picked_by_read_dir_order() {
	use crate::{create_adapter, models::AgentType};
	let _env = crate::skills::prune::test_lock::env_lock().lock().unwrap();
	let project = tempfile::tempdir().unwrap();
	let root = project.path();

	// Both declare `name: twin`; only one sits at the sanitized slot, so a
	// `dir.join(sanitize_name(name))` probe would not see the conflict
	// either.
	let canonical = root.join(".aghub/twin");
	seed_master(&canonical, "twin");
	let other = root.join(".aghub/twin-from-elsewhere");
	seed_master(&other, "twin");

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	manager.load().unwrap();

	let error = manager
		.remove_skill_planned("twin", true, false, true)
		.expect_err("two Masters for one name must be refused");
	assert!(
		canonical.exists() && other.exists(),
		"a refusal must leave BOTH on disk"
	);
	let message = error.to_string();
	assert!(
		message.contains("twin-from-elsewhere") && message.contains("twin"),
		"the refusal must name both paths so the user can act, got: \
		 {message}"
	);

	// The preview must refuse identically — a preview that printed a plan
	// the commit refuses is the drift `RemovalOutcome::preview` exists to
	// prevent.
	assert!(manager
		.remove_skill_planned("twin", true, true, false)
		.is_err());
}

/// An unreadable entry in the store must not be answered AROUND.
///
/// Its frontmatter `name` is unknown, so it may BE the skill being
/// removed — under a folder name that is not `sanitize_name(name)`, which
/// this layout allows. Two lies follow from trusting a partial list: a
/// hidden same-name Master slips past the duplicate refusal and the
/// readable copy is deleted as if it were the only one, and an unreadable
/// Master answers `absent` (a success no-op) with its bytes still on disk.
/// Both are answered by failing closed and naming the path.
///
/// Revert proof: swap `load_master_skills(&store)?` back to a partial
/// reader in `skill_for_planned_removal` and both halves below go green
/// the wrong way — the first returns `removed`, the second
/// `ResourceNotFound`.
#[test]
#[cfg(unix)]
fn an_unreadable_store_entry_fails_closed_instead_of_guessing() {
	use crate::{create_adapter, models::AgentType};
	use std::os::unix::fs::PermissionsExt;
	let _env = crate::skills::prune::test_lock::env_lock().lock().unwrap();
	let project = tempfile::tempdir().unwrap();
	let root = project.path();

	// A readable Master, plus a SECOND directory whose SKILL.md declares
	// the SAME frontmatter name and cannot be opened. Discovery sees one;
	// the disk holds two.
	let readable = root.join(".aghub/demo");
	seed_master(&readable, "demo");
	let hidden = root.join(".aghub/old-folder");
	seed_master(&hidden, "demo");
	std::fs::set_permissions(
		hidden.join("SKILL.md"),
		std::fs::Permissions::from_mode(0o000),
	)
	.unwrap();

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	manager.load().unwrap();

	let error = manager
		.remove_skill_planned("demo", true, false, true)
		.expect_err("an unreadable peer may BE a second Master");
	assert!(
		readable.exists(),
		"nothing may be deleted while the store cannot be read"
	);
	assert!(
		error.to_string().contains("old-folder"),
		"the error must name the unreadable path, got: {error}"
	);

	// Same store, a name nothing declares: still not `absent`, because the
	// unreadable entry could have been declaring exactly that name.
	let error = manager
		.remove_skill_planned("some-other-name", true, false, true)
		.expect_err("an unreadable entry cannot prove a name absent");
	assert!(
		!matches!(error, ConfigError::ResourceNotFound { .. }),
		"got {error:?}"
	);

	std::fs::set_permissions(
		hidden.join("SKILL.md"),
		std::fs::Permissions::from_mode(0o644),
	)
	.unwrap();
}

/// The fallback sits between "no agent holds it" and the not-found arm, so
/// the idempotent-delete contract now runs through it. A name present in
/// no store must still answer not-found, which the API turns into
/// `absent` — the desktop "clean all" loop hits this on every already
/// cleaned row.
#[test]
fn all_agents_delete_of_an_absent_name_is_still_not_found() {
	use crate::{create_adapter, models::AgentType};
	let _env = crate::skills::prune::test_lock::env_lock().lock().unwrap();
	let project = tempfile::tempdir().unwrap();
	let root = project.path();
	seed_master(&root.join(".aghub/present"), "present");

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	manager.load().unwrap();
	assert!(matches!(
		manager.remove_skill_planned("absent-name", true, false, true),
		Err(ConfigError::ResourceNotFound { .. })
	));
}

/// An agent Referrer must not buy the store a pass.
///
/// `skill_for_planned_removal` answers from the caller's own config first and
/// from a peer agent's second, and both returns sat BEFORE the store scan — so
/// the duplicate refusal and the fail-closed read only ever ran for an ORPHAN
/// Master. Link either Master into any agent and `--all-agents --yes` deleted
/// one of the two, reported `removed`, and pruned the lock key while the other
/// Master stayed on disk.
///
/// Both store refusals are exercised, because they are different arms: a
/// second readable Master under the same frontmatter name, and a peer whose
/// `SKILL.md` will not open (so its name cannot be ruled out).
///
/// Revert proof: move the store scan back below the two early returns and
/// every iteration here reports `Ok(.. executed: true, Pruned(["twin"]) ..)`.
#[test]
#[cfg(unix)]
fn linked_masters_do_not_bypass_exhaustive_store_validation() {
	use crate::{create_adapter, models::AgentType};
	use std::os::unix::fs::PermissionsExt;
	let _env = crate::skills::prune::test_lock::env_lock().lock().unwrap();
	// `.claude` is the caller's own agent (the config hit), `.cursor` a peer
	// reached through `load_all_agents` — one early return each.
	for agent_dir in [".claude", ".cursor"] {
		for unreadable in [false, true] {
			let project = tempfile::tempdir().unwrap();
			let root = project.path();
			// Running as root makes a 0o000 file readable anyway, which would
			// turn the fail-closed half into a false green.
			if unreadable && !perms_enforced(root) {
				continue;
			}
			let master = root.join(".aghub/twin");
			let other = root.join(".aghub/other-folder");
			seed_master(&master, "twin");
			seed_master(&other, "twin");
			if unreadable {
				std::fs::set_permissions(
					other.join("SKILL.md"),
					std::fs::Permissions::from_mode(0o000),
				)
				.unwrap();
			}
			let link = root.join(agent_dir).join("skills/twin");
			std::fs::create_dir_all(link.parent().unwrap()).unwrap();
			std::os::unix::fs::symlink(&master, &link).unwrap();
			skill::lock::local::add_skill_to_local_lock(
				"twin",
				local_entry(),
				Some(root),
			)
			.unwrap();
			let lock_before =
				std::fs::read(root.join("skills-lock.json")).unwrap();
			let mut manager = ConfigManager::new(
				create_adapter(AgentType::Claude),
				false,
				Some(root),
			);
			manager.load().unwrap();

			for dry_run in [false, true] {
				let result = manager
					.remove_skill_planned("twin", true, dry_run, !dry_run);
				assert!(
					master.join("SKILL.md").is_file()
						&& other.exists() && link.exists(),
					"{agent_dir}, unreadable={unreadable}: a refusal must \
					 preserve both Masters and the Referrer; got {result:?}"
				);
				let error = result.expect_err(
					"an agent link must not bypass store validation",
				);
				assert!(
					error.to_string().contains("other-folder"),
					"{agent_dir}, unreadable={unreadable}: the refusal must \
					 name the other store entry, got: {error}"
				);
				assert_eq!(
					std::fs::read(root.join("skills-lock.json")).unwrap(),
					lock_before,
					"{agent_dir}, unreadable={unreadable}: a refusal prunes \
					 no lock key"
				);
			}

			if unreadable {
				std::fs::set_permissions(
					other.join("SKILL.md"),
					std::fs::Permissions::from_mode(0o644),
				)
				.unwrap();
			}
		}
	}
}

/// The store scan now runs on EVERY `--all-agents` removal, including the
/// pre-2.18 shape that has no store at all. A missing `.aghub` must read as
/// "holds nothing" (`collect_skills` returns on `NotFound`), not as a store
/// this cannot read — otherwise moving the scan in front of the early returns
/// turns an everyday delete into an IO error.
#[test]
fn all_agents_delete_works_with_no_master_store_at_all() {
	use crate::{create_adapter, models::AgentType};
	let _env = crate::skills::prune::test_lock::env_lock().lock().unwrap();
	let project = tempfile::tempdir().unwrap();
	let root = project.path();
	let copy = root.join(".claude/skills/legacy");
	seed_master(&copy, "legacy");
	assert!(!root.join(".aghub").exists(), "fixture must have no store");

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	manager.load().unwrap();
	let outcome = manager
		.remove_skill_planned("legacy", true, false, true)
		.expect("a missing store is not an unreadable one");
	assert!(outcome.executed && !copy.exists(), "got {outcome:?}");
}

#[test]
fn remove_skill_planned_all_agents_collects_unlinked_master() {
	use crate::skills::removal::PruneStatus;
	use crate::{create_adapter, models::AgentType};
	let _env = crate::skills::prune::test_lock::env_lock().lock().unwrap();
	let project = tempfile::tempdir().unwrap();
	let root = project.path();
	let master = root.join(".aghub/retired-skill");
	let backup = root.join(".aghub/.quarantine/retired-skill");
	#[cfg(unix)]
	{
		let agent_dir = root.join(".claude/skills");
		std::fs::create_dir_all(&agent_dir).unwrap();
		std::os::unix::fs::symlink(
			root.join("missing"),
			agent_dir.join("unrelated"),
		)
		.unwrap();
	}
	for dir in [&master, &backup] {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			"---\nname: retired-skill\ndescription: fixture\n---\n",
		)
		.unwrap();
	}
	skill::lock::local::add_skill_to_local_lock(
		"retired-skill",
		local_entry(),
		Some(root),
	)
	.unwrap();
	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	manager.load().unwrap();
	assert!(manager.get_skill("retired-skill").is_none());
	assert!(manager
		.remove_skill_planned("retired-skill", false, false, true)
		.is_err());
	let preview = manager
		.remove_skill_planned("retired-skill", true, true, false)
		.unwrap();
	assert!(preview.plan.paths.iter().any(|p| p == &master));
	assert!(master.exists());
	let outcome = manager
		.remove_skill_planned("retired-skill", true, false, true)
		.unwrap();
	assert!(outcome.executed);
	assert!(
		!master.exists(),
		"source cleanup must remove the unlinked Master"
	);
	assert!(backup.exists(), "repair backups are not live Masters");
	#[cfg(unix)]
	assert!(root.join(".claude/skills/unrelated").is_symlink());
	assert!(
		matches!(outcome.prune, PruneStatus::Pruned(ref keys) if keys.contains(&"retired-skill".to_string()))
	);
	assert!(!skill::lock::local::read_local_lock(Some(root))
		.skills
		.contains_key("retired-skill"));
	assert!(manager
		.remove_skill_planned("retired-skill", true, true, false)
		.is_err());
}

#[test]
fn remove_skill_planned_both_scope_prunes_global_and_project_locks() {
	// Both scope reconciles two independent locks (global + project). Seed an
	// orphan in each, execute the removal, and assert the returned Pruned keys
	// name BOTH dropped orphans and that both locks are updated on disk.
	use crate::create_adapter;
	use crate::models::{AgentType, ResourceScope};

	let _g = GlobalLockGuard::new();
	let project = tempfile::tempdir().unwrap();
	let skills_dir = project.path().join("skills");
	let skill_dir = skills_dir.join("both-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: both-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);
	// One orphan per lock — neither is on disk, so a real prune drops both.
	skill::lock::add_skill_to_lock("orphan-global-xyz", locked_entry())
		.unwrap();
	skill::lock::local::add_skill_to_local_lock(
		"orphan-project-xyz",
		local_entry(),
		Some(project.path()),
	)
	.unwrap();

	// scope=Both with a project root; write_scope=ProjectOnly (global=false).
	let mut mgr = ConfigManager::with_scope(
		create_adapter(AgentType::Claude),
		false,
		Some(project.path()),
		ResourceScope::Both,
	);
	mgr.load().unwrap();
	let outcome = mgr
		.remove_skill_planned("both-skill", false, false, true)
		.unwrap();

	crate::adapter::set_skills_path_override("claude", None);

	assert!(outcome.executed, "Both-scope removal executes");
	let pruned = match &outcome.prune {
		PruneStatus::Pruned(keys) => keys,
		other => panic!("expected Pruned, got {other:?}"),
	};
	assert!(
		pruned.contains(&"orphan-global-xyz".to_string()),
		"global orphan must be reported pruned, got {pruned:?}"
	);
	assert!(
		pruned.contains(&"orphan-project-xyz".to_string()),
		"project orphan must be reported pruned, got {pruned:?}"
	);
	let global = skill::read_skill_lock();
	assert!(
		!global.skills.contains_key("orphan-global-xyz"),
		"global lock orphan must be pruned on disk"
	);
	let local = skill::lock::local::read_local_lock(Some(project.path()));
	assert!(
		!local.skills.contains_key("orphan-project-xyz"),
		"project lock orphan must be pruned on disk"
	);
}

/// Regression (issue #1): `Both` must short-circuit on a GLOBAL prune
/// failure — the project lock must be left UNTOUCHED, not mutated behind a
/// `Failed { pruned: [] }`. Force the GLOBAL lock write to fail (read-only
/// global lock dir) while a project orphan sits ready to drop, then assert
/// the project lock still holds its orphan and prune is `Failed` with an
/// empty `pruned`.
#[cfg(unix)]
#[test]
fn remove_skill_planned_both_global_failure_leaves_project_lock_untouched() {
	use crate::create_adapter;
	use crate::models::{AgentType, ResourceScope};
	use std::os::unix::fs::PermissionsExt;

	let _g = GlobalLockGuard::new();
	let state = std::env::var("XDG_STATE_HOME").unwrap();
	let lock_dir = std::path::Path::new(&state).join("skills");

	let project = tempfile::tempdir().unwrap();
	let skills_dir = project.path().join("skills");
	let skill_dir = skills_dir.join("both-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: both-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);
	// Seed an orphan in EACH lock; neither is on disk so a real prune would
	// drop both. The project orphan must SURVIVE because the global prune
	// fails first and the project prune must never run.
	skill::lock::add_skill_to_lock("orphan-global-xyz", locked_entry())
		.unwrap();
	skill::lock::local::add_skill_to_local_lock(
		"orphan-project-xyz",
		local_entry(),
		Some(project.path()),
	)
	.unwrap();

	if !perms_enforced(&lock_dir) {
		crate::adapter::set_skills_path_override("claude", None);
		eprintln!("skip: perms not enforced (root)");
		return;
	}
	// Make the GLOBAL lock dir read-only so its atomic temp+rename fails.
	let orig = std::fs::metadata(&lock_dir).unwrap().permissions();
	std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(0o555))
		.unwrap();

	let mut mgr = ConfigManager::with_scope(
		create_adapter(AgentType::Claude),
		false,
		Some(project.path()),
		ResourceScope::Both,
	);
	mgr.load().unwrap();
	let outcome = mgr
		.remove_skill_planned("both-skill", false, false, true)
		.unwrap();

	std::fs::set_permissions(&lock_dir, orig).unwrap();
	crate::adapter::set_skills_path_override("claude", None);

	assert!(outcome.executed, "Both-scope removal still executes");
	match outcome.prune {
		PruneStatus::Failed { reason, pruned } => {
			assert!(!reason.is_empty(), "global failure reason is reported");
			assert!(
				pruned.is_empty(),
				"global failed before pruning anything: {pruned:?}"
			);
		}
		other => panic!("expected Failed on global failure, got {other:?}"),
	}
	let local = skill::lock::local::read_local_lock(Some(project.path()));
	assert!(
		local.skills.contains_key("orphan-project-xyz"),
		"global failure must short-circuit: the project lock is untouched"
	);
}

/// Regression (issue #4): the global-success / project-FAIL partial path
/// through the REAL `remove_skill_planned` (not synthetic `combine_prune`
/// inputs). Force the PROJECT lock write to fail AFTER the global prune
/// succeeds, then assert the global lock WAS mutated and prune is
/// `Failed { pruned: [<global key>] }`.
#[cfg(unix)]
#[test]
fn remove_skill_planned_both_project_failure_reports_partial_global_pruned() {
	use crate::create_adapter;
	use crate::models::{AgentType, ResourceScope};
	use std::os::unix::fs::PermissionsExt;

	let _g = GlobalLockGuard::new();

	let project = tempfile::tempdir().unwrap();
	let skills_dir = project.path().join("skills");
	let skill_dir = skills_dir.join("both-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: both-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);
	// Global orphan WILL be pruned; project orphan would be pruned too, but
	// the project lock write fails so the project lock stays intact.
	skill::lock::add_skill_to_lock("orphan-global-xyz", locked_entry())
		.unwrap();
	skill::lock::local::add_skill_to_local_lock(
		"orphan-project-xyz",
		local_entry(),
		Some(project.path()),
	)
	.unwrap();

	if !perms_enforced(project.path()) {
		crate::adapter::set_skills_path_override("claude", None);
		eprintln!("skip: perms not enforced (root)");
		return;
	}
	// The project lock is `<root>/skills-lock.json`; making the project root
	// read-only blocks the atomic temp+rename inside it (the global lock
	// lives under XDG_STATE_HOME and stays writable, so global succeeds).
	let orig = std::fs::metadata(project.path()).unwrap().permissions();
	std::fs::set_permissions(
		project.path(),
		std::fs::Permissions::from_mode(0o555),
	)
	.unwrap();

	let mut mgr = ConfigManager::with_scope(
		create_adapter(AgentType::Claude),
		false,
		Some(project.path()),
		ResourceScope::Both,
	);
	mgr.load().unwrap();
	let outcome = mgr
		.remove_skill_planned("both-skill", false, false, true)
		.unwrap();

	std::fs::set_permissions(project.path(), orig).unwrap();
	crate::adapter::set_skills_path_override("claude", None);

	assert!(outcome.executed, "Both-scope removal still executes");
	match outcome.prune {
		PruneStatus::Failed { reason, pruned } => {
			assert!(!reason.is_empty(), "project failure reason is reported");
			assert_eq!(
				pruned,
				vec!["orphan-global-xyz".to_string()],
				"the global keys dropped before the project failure must \
				 be reported, got {pruned:?}"
			);
		}
		other => panic!("expected partial Failed, got {other:?}"),
	}
	let global = skill::read_skill_lock();
	assert!(
		!global.skills.contains_key("orphan-global-xyz"),
		"the global lock WAS mutated before the project failure"
	);
}

#[test]
fn remove_skill_planned_project_scope_with_root_prunes_project_lock() {
	// ProjectOnly scope WITH a root: the project lock is reconciled and the
	// dropped orphan is reported. (The no-root variant is covered above.)
	use crate::create_adapter;
	use crate::models::AgentType;

	let _g = GlobalLockGuard::new();
	let project = tempfile::tempdir().unwrap();
	let skills_dir = project.path().join("skills");
	let skill_dir = skills_dir.join("proj-root-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: proj-root-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);
	skill::lock::local::add_skill_to_local_lock(
		"orphan-project-xyz",
		local_entry(),
		Some(project.path()),
	)
	.unwrap();

	// global=false + a project root => ResourceScope::ProjectOnly with a root.
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(project.path()),
	);
	mgr.load().unwrap();
	let outcome = mgr
		.remove_skill_planned("proj-root-skill", false, false, true)
		.unwrap();

	crate::adapter::set_skills_path_override("claude", None);

	assert!(outcome.executed, "ProjectOnly removal executes");
	let pruned = match &outcome.prune {
		PruneStatus::Pruned(keys) => keys,
		other => panic!("expected Pruned, got {other:?}"),
	};
	assert!(
		pruned.contains(&"orphan-project-xyz".to_string()),
		"project orphan must be reported pruned, got {pruned:?}"
	);
	let local = skill::lock::local::read_local_lock(Some(project.path()));
	assert!(
		!local.skills.contains_key("orphan-project-xyz"),
		"project lock orphan must be pruned on disk"
	);
}

/// ProjectOnly scope WITH a root where the PROJECT lock write FAILS: the
/// prune is non-fatal, so the skill is still removed, the single-scope
/// failure drops nothing (`Failed { pruned: [] }`), and the project lock
/// stays intact. Mirrors the Both-project-failure technique (RO root).
#[cfg(unix)]
#[test]
fn remove_skill_planned_project_scope_with_root_failed_prune_keeps_lock() {
	use crate::create_adapter;
	use crate::models::AgentType;
	use std::os::unix::fs::PermissionsExt;

	let _g = GlobalLockGuard::new();
	let project = tempfile::tempdir().unwrap();
	let skills_dir = project.path().join("skills");
	let skill_dir = skills_dir.join("proj-fail-skill");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: proj-fail-skill\ndescription: d\n---\n",
	)
	.unwrap();

	crate::adapter::set_skills_path_override(
		"claude",
		Some(skills_dir.clone()),
	);
	// Orphan a successful project prune WOULD drop — but the write fails,
	// so it must survive in the project lock.
	skill::lock::local::add_skill_to_local_lock(
		"orphan-project-xyz",
		local_entry(),
		Some(project.path()),
	)
	.unwrap();

	if !perms_enforced(project.path()) {
		crate::adapter::set_skills_path_override("claude", None);
		eprintln!("skip: perms not enforced (root)");
		return;
	}
	// The project lock is `<root>/skills-lock.json`; a read-only root
	// blocks the atomic temp+rename inside it (skills_dir was created
	// beforehand, so the skill itself is still deletable under it).
	let orig = std::fs::metadata(project.path()).unwrap().permissions();
	std::fs::set_permissions(
		project.path(),
		std::fs::Permissions::from_mode(0o555),
	)
	.unwrap();

	// global=false + a project root => ProjectOnly scope with a root.
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(project.path()),
	);
	mgr.load().unwrap();
	let outcome = mgr
		.remove_skill_planned("proj-fail-skill", false, false, true)
		.unwrap();

	// RESTORE perms before any assertion so a failed assert never leaks an
	// unremovable temp dir.
	std::fs::set_permissions(project.path(), orig).unwrap();
	crate::adapter::set_skills_path_override("claude", None);

	assert!(outcome.executed, "deletion runs even if the prune fails");
	assert!(
		!skill_dir.exists(),
		"the skill is deleted before the prune is attempted"
	);
	match outcome.prune {
		PruneStatus::Failed { reason, pruned } => {
			assert!(!reason.is_empty(), "failure reason is reported");
			assert!(
				pruned.is_empty(),
				"single-scope write failure drops nothing: {pruned:?}"
			);
		}
		other => panic!("expected Failed, got {other:?}"),
	}
	let local = skill::lock::local::read_local_lock(Some(project.path()));
	assert!(
		local.skills.contains_key("orphan-project-xyz"),
		"a failed project prune must leave the project lock unchanged"
	);
}

// T3: exhaustive branch coverage of the helper that maps the shared
// materializer's single-agent result onto the CLI add path's historical
// error contract. NeedsLink-ok -> Ok; NeedsLink-error -> Err(resource_exists);
// Unsupported -> Ok regardless of the result (no writable dir was never an
// error). The `NativeReader` arm went with the variant.
#[test]
fn ensure_single_agent_installed_covers_every_branch() {
	use crate::models::AgentType;
	use crate::skills::install_fetched::AgentInstallResult;
	use crate::skills::linker::LinkNeed;

	let needs_link = LinkNeed::NeedsLink {
		referrer_dir: PathBuf::from("/x"),
	};

	// NeedsLink + error-free result -> Ok.
	let ok = [AgentInstallResult {
		agent: AgentType::Claude,
		installed: true,
		error: None,
	}];
	assert!(ConfigManager::ensure_single_agent_installed(
		&ok,
		&needs_link,
		"s"
	)
	.is_ok());

	// NeedsLink + a soft failure (occupied slot / link error) -> Err.
	let conflict = [AgentInstallResult {
		agent: AgentType::Claude,
		installed: false,
		error: Some("slot occupied".to_string()),
	}];
	let err = ConfigManager::ensure_single_agent_installed(
		&conflict,
		&needs_link,
		"my-skill",
	)
	.unwrap_err();
	assert!(
		matches!(err, ConfigError::ResourceExists { .. }),
		"a NeedsLink soft-failure must surface resource_exists, got {err:?}"
	);

	// NeedsLink + empty results (defensive) -> Err.
	assert!(ConfigManager::ensure_single_agent_installed(
		&[],
		&needs_link,
		"s"
	)
	.is_err());

	// Unsupported -> Ok (defensive arm only: the add path never reaches
	// this helper — the materialize preflight hard-errors first; that
	// contract is pinned by
	// `add_skill_from_path_unsupported_scope_errors_and_writes_nothing`
	// in tests/test_agent_paths.rs).
	assert!(ConfigManager::ensure_single_agent_installed(
		&conflict,
		&LinkNeed::Unsupported,
		"s"
	)
	.is_ok());
}

// T3 parity guard: the CLI add-from-path materialization and the
// fetched/desktop materialization must produce a BYTE-IDENTICAL
// `.agents/skills/<name>/SKILL.md` and the same agent link shape for the
// same source skill — so the two install paths can never diverge again
// (they once did when the CLI used a narrower link check). Both copy the
// source tree verbatim; only the canonical SKILL.md bytes + link shape are
// asserted, not the lock (the lock contract is pinned elsewhere).
#[cfg(unix)]
#[test]
fn cli_add_and_fetched_install_produce_identical_master_and_link() {
	use crate::create_adapter;
	use crate::models::{AgentType, ResourceScope};
	use crate::skills::install_fetched::{
		install_fetched_skill_and_lock, FetchedSkillInstallRequest,
	};
	use crate::skills::linker::{LinkTarget, Linker};

	// One source skill, copied verbatim by both paths. Non-canonical
	// frontmatter ordering + a body + an asset so a re-serialization (which
	// would NOT be byte-identical) is detectable.
	let src_tmp = tempfile::tempdir().unwrap();
	let src = src_tmp.path().join("parity-skill");
	std::fs::create_dir_all(&src).unwrap();
	let skill_md =
		"---\ndescription: parity\nname: parity-skill\n---\nThe body.\n";
	std::fs::write(src.join("SKILL.md"), skill_md).unwrap();
	std::fs::create_dir_all(src.join("assets")).unwrap();
	std::fs::write(src.join("assets/data.json"), "{}").unwrap();

	// Path A: CLI add-from-path universal install.
	let cli_root_tmp = tempfile::tempdir().unwrap();
	let cli_root = cli_root_tmp.path().canonicalize().unwrap();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(&cli_root),
	);
	mgr.load().unwrap();
	mgr.add_skill_from_path_universal(&src, None).unwrap();

	// Path B: fetched/desktop universal install of the same source.
	let fetched_root_tmp = tempfile::tempdir().unwrap();
	let fetched_root = fetched_root_tmp.path().canonicalize().unwrap();
	let lock_source = skill::InstallLockSource {
		source: "local/test".to_string(),
		source_type: "local".to_string(),
		source_url: "file:///local/test".to_string(),
		ref_name: None,
	};
	let req = FetchedSkillInstallRequest {
		skill_file: &src.join("SKILL.md"),
		source: &lock_source,
		lock_skill_path: "parity-skill/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&fetched_root),
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
		force_unsafe: false,
	};
	install_fetched_skill_and_lock(req).unwrap();

	// The canonical Master SKILL.md must be byte-identical across paths.
	let cli_master = cli_root.join(".aghub/parity-skill/SKILL.md");
	let fetched_master = fetched_root.join(".aghub/parity-skill/SKILL.md");
	let cli_bytes = std::fs::read(&cli_master).unwrap();
	let fetched_bytes = std::fs::read(&fetched_master).unwrap();
	assert_eq!(
		cli_bytes, fetched_bytes,
		"CLI-add and fetched-install master SKILL.md must be \
		 byte-identical"
	);
	assert_eq!(
		cli_bytes,
		skill_md.as_bytes(),
		"both paths must copy the source SKILL.md verbatim"
	);

	// The asset must survive on both (whole-tree copy, not SKILL.md only).
	assert_eq!(
		std::fs::read_to_string(
			cli_root.join(".aghub/parity-skill/assets/data.json")
		)
		.unwrap(),
		std::fs::read_to_string(
			fetched_root.join(".aghub/parity-skill/assets/data.json")
		)
		.unwrap(),
	);

	// Identical link shape: each agent dir holds a symlink to its Master.
	let cli_link = cli_root.join(".claude/skills/parity-skill");
	let fetched_link = fetched_root.join(".claude/skills/parity-skill");
	assert!(Linker::is_link(&cli_link), "CLI add must leave a link");
	assert!(
		Linker::is_link(&fetched_link),
		"fetched install must leave a link"
	);
	assert!(cli_link.join("SKILL.md").exists());
	assert!(fetched_link.join("SKILL.md").exists());
}
