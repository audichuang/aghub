use assert_cmd::Command;
use serde_json::Value;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../fixtures")
		.canonicalize()
		.unwrap()
}

fn aghub_cli() -> Command {
	let mut cmd = Command::cargo_bin("aghub-cli").unwrap();
	let dir = fixtures_dir();
	cmd.env("HOME", &dir);
	cmd.env("USERPROFILE", &dir);
	cmd.env("APPDATA", &dir);
	clear_agent_home_overrides(&mut cmd);
	cmd
}

#[test]
fn test_agent_all_get_skills_is_valid_json_array() {
	let dir = fixtures_dir();
	let out = aghub_cli()
		.current_dir(&dir)
		.args(["--json", "--agent", "all", "--all", "get", "skills"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let json: Value =
		serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");
	let arr = json.as_array().expect("output must be a JSON array");
	assert!(!arr.is_empty(), "array must not be empty");

	// Each entry is a skill with an agent field
	for entry in arr {
		assert!(entry["name"].is_string(), "each entry must have 'name'");
		assert!(entry["agent"].is_string(), "each entry must have 'agent'");
	}

	// Cline has universal_skills + project_skills_path = root/.agents/skills.
	// fixtures/.cline/ makes fixtures/ the project root, so cline sees:
	// fixtures/.agents/skills/vercel-react-best-practices/SKILL.md
	assert!(
		arr.iter().any(|s| s["agent"] == "cline"
			&& s["name"] == "vercel-react-best-practices"),
		"must have cline entry with vercel-react-best-practices skill"
	);
}

#[test]
fn test_agent_all_get_mcps_is_valid_json_array() {
	let dir = fixtures_dir();
	let out = aghub_cli()
		.current_dir(&dir)
		.args(["--json", "--agent", "all", "--all", "get", "mcps"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let json: Value =
		serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");
	let arr = json.as_array().expect("output must be a JSON array");
	assert!(
		arr.iter()
			.any(|m| m["agent"] == "cursor" && m["name"] == "test-mcp"),
		"the project-scoped cursor fixture must be listed: {json}"
	);

	// Each entry is an MCP with an agent field. The PROJECT-scoped
	// fixtures/.cursor/mcp.json is what keeps this non-empty everywhere:
	// Cline's fixture is global-scoped, and on Windows `dirs::home_dir()` reads
	// the known-folder API rather than the `HOME` this harness overrides, so a
	// global fixture is simply not visible there.
	for entry in arr {
		assert!(entry["name"].is_string(), "each entry must have 'name'");
		assert!(entry["agent"].is_string(), "each entry must have 'agent'");
		assert!(entry["type"].is_string(), "each entry must have 'type'");
	}
}

#[test]
fn test_agent_all_non_get_command_fails() {
	let out = aghub_cli()
		.args(["--agent", "all", "add", "skills", "--name", "foo"])
		.output()
		.unwrap();

	assert!(!out.status.success(), "--agent all with add should fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("all") || stderr.contains("get"),
		"error must mention the restriction, got: {}",
		stderr
	);
}

#[test]
fn test_agent_list_get_skills_filters_to_requested_agents() {
	// cline AND cursor both read the fixtures project master, so BOTH must
	// appear (an implementation that drops one fails) and NOTHING else may
	// (an implementation that skips the filter fails).
	let dir = fixtures_dir();
	let out = aghub_cli()
		.current_dir(&dir)
		.args(["--json", "-a", "cline,cursor", "--all", "get", "skills"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value =
		serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");
	let arr = json.as_array().expect("output must be a JSON array");
	let agents: std::collections::BTreeSet<&str> = arr
		.iter()
		.map(|e| e["agent"].as_str().expect("agent field"))
		.collect();
	assert_eq!(
		agents,
		["cline", "cursor"].into_iter().collect(),
		"filtered output must contain exactly the requested agents"
	);
	for agent in ["cline", "cursor"] {
		assert!(
			arr.iter().any(|s| s["agent"] == agent
				&& s["name"] == "vercel-react-best-practices"),
			"{agent} must list the master skill"
		);
	}
}

#[test]
fn test_agent_list_unknown_agent_fails() {
	let out = aghub_cli()
		.args(["-a", "claude,nonesuch", "get", "skills"])
		.output()
		.unwrap();

	assert!(!out.status.success(), "unknown agent in list must fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("nonesuch"),
		"error must name the bad token, got: {stderr}"
	);
	// The valid-id list must be present: assert on an id NOT in the input,
	// so a message that merely echoes the input can't pass.
	assert!(
		stderr.contains("cursor"),
		"error must list the valid agent ids, got: {stderr}"
	);
}

#[test]
fn test_agent_list_lock_scoped_command_fails() {
	// `check` ignores the agent flag (lock-scoped): a list would repeat
	// identical work, so it is rejected with a pointer to single-agent use.
	let out = aghub_cli()
		.args(["-a", "claude,grok", "check", "skills"])
		.output()
		.unwrap();

	assert!(!out.status.success(), "-a list with check should fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("single agent"),
		"error must mention the restriction, got: {stderr}"
	);
}

#[test]
fn test_agent_list_early_dispatched_command_rejected() {
	// Plain `doctor` is Master/lock scoped and ignores the agent flag. A list
	// must be rejected up front unless the caller explicitly requests the
	// roster-aware link audit.
	let out = aghub_cli()
		.args(["-a", "claude,grok", "doctor"])
		.output()
		.unwrap();

	assert!(!out.status.success(), "-a list with doctor should fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("does not take an --agent list"),
		"error must mention the list restriction, got: {stderr}"
	);
}

#[cfg(unix)]
#[test]
fn doctor_verify_links_audits_the_selected_roster() {
	use std::os::unix::fs::symlink;

	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let master = home.path().join(".agents/skills/rostered");
	std::fs::create_dir_all(&master).unwrap();
	std::fs::write(
		master.join("SKILL.md"),
		"---\nname: rostered\ndescription: roster audit fixture\n---\n",
	)
	.unwrap();

	let claude_skills = home.path().join(".claude/skills");
	std::fs::create_dir_all(&claude_skills).unwrap();
	symlink(&master, claude_skills.join("rostered")).unwrap();
	// Grok intentionally has no referrer. Codex reads the universal Master
	// directly, so the three agents exercise linked/missing/autoCovered.

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"claude,codex,grok",
			"doctor",
			"--verify-links",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let rows: Value = serde_json::from_slice(&out.stdout).unwrap();
	let agents = rows[0]["linkAudit"]["agents"].as_array().unwrap();
	// This fixture's master is UNTRACKED (no lock entry), so grok — which needs
	// a link and has no slot — is an `orphanMaster`, not a `missing` link. The
	// two have opposite remedies: nothing can relink an orphan, because there
	// is no source to relink from.
	//
	// And claude is `linked` on that SAME untracked master. That combination is
	// exactly why the orphan-master note has to hedge instead of saying "delete
	// it": deleting this master would dangle claude's live symlink.
	for (agent, state) in [
		("claude", "linked"),
		("codex", "autoCovered"),
		("grok", "orphanMaster"),
	] {
		assert!(
			agents
				.iter()
				.any(|row| row["agent"] == agent && row["state"] == state),
			"expected {agent}:{state}, got {agents:?}"
		);
	}
	assert_eq!(
		rows[0]["linkAudit"]["state"], "issues",
		"a row carrying an orphanMaster is not `verified`: {}",
		rows[0]["linkAudit"]
	);
}

#[test]
fn test_agent_all_is_case_insensitive() {
	// `-a ALL` goes through the same shared parser as `-a all`.
	let dir = fixtures_dir();
	let out = aghub_cli()
		.current_dir(&dir)
		.args(["--json", "-a", "ALL", "--all", "get", "skills"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value =
		serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");
	assert!(!json.as_array().unwrap().is_empty());
}

#[test]
fn test_agent_all_mixed_with_ids_fails() {
	let out = aghub_cli()
		.args(["-a", "all,claude", "get", "skills"])
		.output()
		.unwrap();

	assert!(!out.status.success(), "'all,claude' must fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("cannot be combined"),
		"mixed all+ids must get the dedicated error, got: {stderr}"
	);
}

#[test]
fn check_skills_outputs_json_array() {
	// No network: with an empty/local-only lock, check returns an array (possibly
	// with Uncheckable entries) and exits 0.
	let dir = tempfile::tempdir().unwrap();
	let out = aghub_cli()
		.current_dir(dir.path())
		.args(["-a", "claude", "check", "skills", "--json"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	assert!(v.is_array());
}

#[test]
fn check_skills_online_empty_lock_outputs_empty_array() {
	// `--online` with an isolated, empty global lock and no project lock has no
	// entries to resolve, so the orchestrator returns `[]` and never touches the
	// network — exercising the online plumbing (runtime + orchestrator +
	// rendering) end-to-end without a remote.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "check", "skills", "--online", "--json"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(v.as_array().map(Vec::len), Some(0));
}

/// `--online` end-to-end against a real public repo: seed the global lock with
/// one entry and assert the check emits a status for it without crashing or
/// leaking a token. Ignored by default (needs network).
#[ignore = "network"]
#[test]
fn check_skills_online_public_repo_emits_status() {
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let lock_dir = state.path().join("skills");
	std::fs::create_dir_all(&lock_dir).unwrap();
	std::fs::write(
		lock_dir.join(".skill-lock.json"),
		r#"{"version":3,"skills":{"hello":{"source":"octocat/Hello-World","sourceType":"github","sourceUrl":"https://github.com/octocat/Hello-World","skillPath":"SKILL.md","skillFolderHash":"","installedAt":"t","updatedAt":"t"}}}"#,
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-a", "claude", "-g", "check", "skills", "--online", "--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	let arr = v.as_array().unwrap();
	assert!(arr.iter().any(|e| e["name"] == "hello"));
}

#[test]
fn test_pi_add_mcp_fails_for_unsupported_agent() {
	let out = aghub_cli()
		.args([
			"--agent",
			"pi",
			"add",
			"mcps",
			"--name",
			"pi-mcp",
			"--command",
			"echo hello",
		])
		.output()
		.unwrap();

	assert!(
		!out.status.success(),
		"pi MCP add should fail for unsupported agent"
	);

	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("Cannot add MCP server for pi agent"),
		"stderr must mention the unsupported MCP operation, got: {}",
		stderr
	);
}

// ==================== F2: layout-aware delete + prune-lock ====================

/// An aghub-cli command with an ISOLATED temp HOME + XDG_STATE_HOME so the
/// destructive skill/lock tests below never touch the shared fixtures dir.
fn isolated_cli(home: &std::path::Path, state: &std::path::Path) -> Command {
	let mut cmd = Command::cargo_bin("aghub-cli").unwrap();
	cmd.env("HOME", home);
	cmd.env("USERPROFILE", home);
	cmd.env("APPDATA", home);
	cmd.env("XDG_STATE_HOME", state);
	clear_agent_home_overrides(&mut cmd);
	cmd.current_dir(home);
	cmd
}

/// Overriding `$HOME` is NOT enough to isolate a run: several descriptors honour
/// their agent's own home/config variable, which outranks `$HOME` and would send
/// the test's writes straight into the developer's real config. (It already did:
/// an ambient `OPENCODE_CONFIG_DIR` had these tests writing MCP servers into a
/// live `opencode.json`.) Clear every such variable for the child process.
fn clear_agent_home_overrides(cmd: &mut Command) {
	for key in [
		"OPENCODE_CONFIG",
		"OPENCODE_CONFIG_DIR",
		"XDG_CONFIG_HOME",
		"CODEX_HOME",
		"COPILOT_HOME",
		"KIMI_SHARE_DIR",
		"VIBE_HOME",
		"HERMES_HOME",
		"GROK_HOME",
		"OPENCLAW_CONFIG_PATH",
		"OPENCLAW_STATE_DIR",
	] {
		cmd.env_remove(key);
	}
}

#[test]
fn top_level_scope_flags_are_mutually_exclusive() {
	for flags in [["-g", "-p"], ["-g", "--all"], ["-p", "--all"]] {
		let out = aghub_cli()
			.args([flags[0], flags[1], "get", "skills"])
			.output()
			.unwrap();

		let stderr = String::from_utf8_lossy(&out.stderr);
		assert!(
			!out.status.success(),
			"scope pair {flags:?} must be rejected: {stderr}"
		);
		assert!(
			stderr.contains("mutually exclusive"),
			"scope pair {flags:?} must be reported as a conflict: {stderr}"
		);
	}
}

#[test]
fn source_sync_help_distinguishes_update_scope_from_install_roster() {
	let out = aghub_cli()
		.args(["source", "sync", "--help"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout.contains("does not narrow update targets"),
		"update help must explain its scope-wide semantics: {stdout}"
	);
	assert!(
		stdout.contains("roster selected by `-a/--agent`"),
		"install help must explain roster fan-out: {stdout}"
	);
}

#[cfg(unix)]
#[test]
fn generic_mutations_reject_all_scope_before_writing() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	let cases: &[&[&str]] = &[
		&["--all", "add", "skills", "--name", "blocked"],
		&[
			"--all",
			"update",
			"skills",
			"blocked",
			"--description",
			"changed",
		],
		&["--all", "delete", "skills", "blocked"],
		// enable/disable take the narrowed `McpResource` (clap rejects
		// `skills` at parse time — no agent supports it), so the scope guard
		// has to be exercised through their real resource.
		&["--all", "enable", "mcps", "blocked"],
		&["--all", "disable", "mcps", "blocked"],
	];

	for args in cases {
		let out = isolated_cli(home.path(), state.path())
			.current_dir(project.path())
			.args(*args)
			.output()
			.unwrap();

		let stderr = String::from_utf8_lossy(&out.stderr);
		assert!(
			!out.status.success(),
			"generic mutation {args:?} must reject --all: {stderr}"
		);
		assert!(
			stderr.contains("does not support --all")
				&& stderr.contains("-g/--global")
				&& stderr.contains("-p/--project"),
			"generic mutation {args:?} must prescribe one write scope: {stderr}"
		);
	}

	assert!(
		!home.path().join(".agents/skills/blocked").exists(),
		"a rejected --all mutation must not create the global Master"
	);
	assert!(
		!project.path().join(".agents/skills/blocked").exists(),
		"a rejected --all mutation must not create the project Master"
	);
}

#[cfg(unix)]
#[test]
fn apply_update_is_not_gated_by_agent_config() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	std::fs::write(home.path().join(".claude.json"), "{not-json").unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["-g", "apply-update", "skills", "missing"])
		.output()
		.unwrap();

	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(!out.status.success(), "missing --yes must fail: {stderr}");
	assert!(
		stderr.contains("without --yes"),
		"lock-scoped apply-update must reach its own confirmation gate: {stderr}"
	);
	assert!(
		!stderr.contains("Failed to load config"),
		"missing or malformed agent config must not gate apply-update: {stderr}"
	);
}

// These delete tests rely on redirecting the home dir via env (HOME/USERPROFILE).
// On Windows `dirs::home_dir()` goes through `SHGetKnownFolderPath` and ignores
// those env vars, so the spawned aghub-cli resolves the real home and the temp
// skill is never inside an allow-listed root -> the delete refuses and the
// command exits non-zero. The delete logic is platform-agnostic and covered on
// unix; gate these home-dependent tests (and the helper) to unix.
#[cfg(unix)]
fn write_claude_skill(
	home: &std::path::Path,
	name: &str,
) -> std::path::PathBuf {
	let dir = home.join(".claude/skills").join(name);
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: d\n---\n"),
	)
	.unwrap();
	dir
}

const ORPHAN_LOCK: &str = r#"{"version":3,"skills":{"orphan":{"source":"o/r","sourceType":"github","sourceUrl":"https://github.com/o/r","skillFolderHash":"","installedAt":"t","updatedAt":"t"}}}"#;

fn seed_global_lock(state: &std::path::Path) -> std::path::PathBuf {
	let dir = state.join("skills");
	std::fs::create_dir_all(&dir).unwrap();
	let path = dir.join(".skill-lock.json");
	std::fs::write(&path, ORPHAN_LOCK).unwrap();
	path
}

#[cfg(unix)]
#[test]
fn delete_skill_dry_run_is_default_and_lists_paths() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "mytool");

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "delete", "skills", "mytool"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	// Task 10: delete JSON is now the snake_case RemovalView shape.
	assert_eq!(json["dry_run"], true);
	assert_eq!(json["executed"], false);
	let paths = json["paths"].as_array().unwrap();
	assert!(
		paths
			.iter()
			.any(|p| p.as_str().unwrap().ends_with("mytool")),
		"paths: {paths:?}"
	);
	assert!(skill_dir.exists(), "dry-run must not delete");
}

#[cfg(unix)]
#[test]
fn delete_skill_yes_removes_copy() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "goner");

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json", "-a", "claude", "delete", "skills", "goner", "--yes",
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["executed"], true);
	assert!(!skill_dir.exists(), "--yes removes the copy");
}

#[cfg(unix)]
#[test]
fn delete_skill_yes_prunes_and_reports() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "goner");
	// An orphan lock entry (no on-disk skill) the executed prune must drop and
	// the JSON must report under `pruned_lock_entries`.
	seed_global_lock(state.path());

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json", "-a", "claude", "delete", "skills", "goner", "--yes",
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["executed"], true);
	assert!(!skill_dir.exists(), "--yes removes the copy");
	let pruned = json["pruned_lock_entries"]
		.as_array()
		.expect("pruned_lock_entries present on executed delete");
	assert!(
		pruned.iter().any(|n| n == "orphan"),
		"orphan lock entry must be reported pruned: {pruned:?}"
	);
	// One wire shape with the API/desktop DTO: never the legacy camelCase key.
	assert!(
		json.get("prunedLockEntries").is_none(),
		"prune keys must be snake_case to match the API DeleteSkillByPathResponse"
	);
}

// ==================== #5: MCP delete dry-run/confirm gate ====================
//
// MCP delete now routes through `remove_mcp_planned` with the same
// `--yes`/`--dry-run` gate + snake_case `RemovalView` JSON shape as skills
// (Task 14). MCP removal is a flat config-file rewrite (no symlink/allowlist),
// so these are platform-agnostic — NOT unix-gated.

/// Seed an MCP via `add mcps`, returning the isolated HOME/STATE temp dirs so
/// the follow-up delete + get run against the same config.
// Only used by #[cfg(unix)] tests (Windows global MCP config isn't HOME-isolated).
#[cfg(unix)]
fn seed_mcp(home: &std::path::Path, state: &std::path::Path, name: &str) {
	let add = isolated_cli(home, state)
		.args([
			"-a", "claude", "add", "mcps", "--name", name, "--url", "http://h",
		])
		.output()
		.unwrap();
	assert!(
		add.status.success(),
		"seed add must succeed; stderr: {}",
		String::from_utf8_lossy(&add.stderr)
	);
}

/// True when `get mcps` lists an MCP named `name` for the given env.
// Only used by #[cfg(unix)] tests (Windows global MCP config isn't HOME-isolated).
#[cfg(unix)]
fn mcp_listed(
	home: &std::path::Path,
	state: &std::path::Path,
	name: &str,
) -> bool {
	let out = isolated_cli(home, state)
		.args(["--json", "-a", "claude", "get", "mcps"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"get mcps must succeed; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	json.as_array().unwrap().iter().any(|m| m["name"] == name)
}

/// `-a claude,opencode add mcps` must write EACH agent's own config — the
/// desktop multi-select's CLI parity. Asserted via per-agent reads, not the
/// add's exit code alone.
#[cfg(unix)]
#[test]
fn add_mcp_agent_list_writes_each_agent_config() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude,opencode",
			"add",
			"mcps",
			"--name",
			"multi",
			"--url",
			"http://h",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	// stdout is ONE valid JSON document: the shared core batch envelope
	// (same wire shape the API /mcps/batch returns) with machine-readable
	// per-agent attribution — not N concatenated docs.
	let envelope: Value = serde_json::from_slice(&out.stdout)
		.expect("batch stdout must be a single valid JSON document");
	assert_eq!(envelope["success_count"], 2, "{envelope}");
	assert_eq!(envelope["failed_count"], 0, "{envelope}");
	let rows = envelope["results"].as_array().expect("results array");
	assert_eq!(rows.len(), 2, "one row per agent: {envelope}");
	for (row, agent) in rows.iter().zip(["claude", "opencode"]) {
		assert_eq!(row["agent"], agent, "rows keep the list order");
		assert_eq!(row["ok"], true, "{agent} must succeed: {row}");
		assert_eq!(
			row["output"]["name"], "multi",
			"{agent} row must carry the command's own payload"
		);
	}

	for agent in ["claude", "opencode"] {
		let get = isolated_cli(home.path(), state.path())
			.args(["--json", "-a", agent, "get", "mcps"])
			.output()
			.unwrap();
		assert!(
			get.status.success(),
			"get mcps for {agent}: {}",
			String::from_utf8_lossy(&get.stderr)
		);
		let json: Value = serde_json::from_slice(&get.stdout).unwrap();
		assert!(
			json.as_array()
				.unwrap()
				.iter()
				.any(|m| m["name"] == "multi"),
			"{agent} config must list the MCP added via the agent list"
		);
	}
}

/// A batch naming an agent with NO MCP support (pi) must be rejected by the
/// preflight BEFORE any write — claude's config must stay untouched.
#[cfg(unix)]
#[test]
fn add_mcp_agent_list_preflight_rejects_unsupported_agent() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude,pi",
			"add",
			"mcps",
			"--name",
			"never",
			"--url",
			"http://h",
		])
		.output()
		.unwrap();
	assert!(
		!out.status.success(),
		"unsupported agent must fail the batch"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("pi") && stderr.contains("nothing was written"),
		"preflight must name the agent and promise no writes, got: {stderr}"
	);
	// The preflight fired BEFORE any write: claude has no 'never' MCP.
	assert!(
		!mcp_listed(home.path(), state.path(), "never"),
		"claude config must be untouched after a preflight rejection"
	);
}

/// The preflight must judge the scope the batch WRITES: augmentcode holds
/// MCPs globally but not per-project, so `-p` must reject the whole batch
/// BEFORE claude's project config is written.
#[cfg(unix)]
#[test]
fn add_mcp_agent_list_preflight_rejects_wrong_scope() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let proj = home.path().join("proj");
	std::fs::create_dir_all(proj.join(".claude")).unwrap();

	let out = isolated_cli(home.path(), state.path())
		.current_dir(&proj)
		.args([
			"-p",
			"-a",
			"claude,augmentcode",
			"add",
			"mcps",
			"--name",
			"never",
			"--url",
			"http://h",
		])
		.output()
		.unwrap();
	assert!(
		!out.status.success(),
		"global-only agent must fail -p batch"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("augmentcode")
			&& stderr.contains("project")
			&& stderr.contains("nothing was written"),
		"preflight must name the agent and the scope, got: {stderr}"
	);
	assert!(
		!proj.join(".mcp.json").exists(),
		"claude's project config must not be created before the rejection"
	);
}

/// enable/disable must also preflight the toggle capability: windsurf holds
/// MCPs but cannot enable/disable them, so the batch is rejected before
/// hermes (which CAN toggle) is modified.
#[test]
fn toggle_mcp_agent_list_preflight_rejects_non_toggleable() {
	let out = aghub_cli()
		.args(["-g", "-a", "hermes,windsurf", "disable", "mcps", "ghost"])
		.output()
		.unwrap();
	assert!(
		!out.status.success(),
		"non-toggleable agent must fail batch"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("windsurf") && stderr.contains("enable/disable"),
		"preflight must name windsurf's missing toggle, got: {stderr}"
	);
	assert!(
		!stderr.contains("hermes"),
		"hermes supports toggling and must not be blamed: {stderr}"
	);
}

/// A SYNTACTIC list that dedups to one agent still emits the batch
/// envelope — the top-level output shape must not depend on duplicates.
#[cfg(unix)]
#[test]
fn agent_list_duplicate_still_emits_envelope() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude,claude",
			"add",
			"mcps",
			"--name",
			"solo",
			"--url",
			"http://h",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let envelope: Value = serde_json::from_slice(&out.stdout)
		.expect("stdout must be one valid JSON document");
	let rows = envelope["results"]
		.as_array()
		.expect("must be the batch envelope");
	assert_eq!(rows.len(), 1, "deduped to one row: {envelope}");
	assert_eq!(rows[0]["agent"], "claude");
	assert_eq!(rows[0]["ok"], true);
	assert_eq!(envelope["success_count"], 1);
}

/// A mid-batch runtime failure (duplicate on claude) must NOT skip the
/// remaining agents: opencode still gets the MCP, the envelope reports the
/// partial state per agent, and the exit code is non-zero.
#[cfg(unix)]
#[test]
fn add_mcp_agent_list_reports_partial_failure_and_continues() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	seed_mcp(home.path(), state.path(), "dup");

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude,opencode",
			"add",
			"mcps",
			"--name",
			"dup",
			"--url",
			"http://h",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "a failed agent must exit non-zero");

	let envelope: Value = serde_json::from_slice(&out.stdout)
		.expect("batch stdout must be a single valid JSON document");
	assert_eq!(envelope["failed_count"], 1, "{envelope}");
	assert_eq!(envelope["success_count"], 1, "{envelope}");
	let rows = envelope["results"].as_array().unwrap();
	assert_eq!(rows.len(), 2);
	assert_eq!(rows[0]["agent"], "claude");
	assert_eq!(rows[0]["ok"], false, "claude already has 'dup': {envelope}");
	assert!(
		rows[0]["error"]
			.as_str()
			.unwrap_or_default()
			.contains("dup"),
		"claude row must carry the error: {envelope}"
	);
	assert_eq!(rows[1]["agent"], "opencode");
	assert_eq!(
		rows[1]["ok"], true,
		"opencode must still be attempted: {envelope}"
	);

	// Observable outcome: opencode really has the MCP on disk.
	let get = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "opencode", "get", "mcps"])
		.output()
		.unwrap();
	let json: Value = serde_json::from_slice(&get.stdout).unwrap();
	assert!(
		json.as_array().unwrap().iter().any(|m| m["name"] == "dup"),
		"opencode config must contain the MCP despite claude's failure"
	);
}

// ================= source sync: agent list + dry-run fan-out =================

/// Write a minimal source-repo layout (one skill dir with SKILL.md) that the
/// debug-only `AGHUB_TEST_SOURCE_FETCH_ROOT` fetch hook can serve.
#[cfg(unix)]
fn write_source_repo(root: &std::path::Path, skill: &str) {
	let dir = root.join(skill);
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!("---\nname: {skill}\ndescription: d\n---\nbody\n"),
	)
	.unwrap();
}

/// The JSON dry-run must expose the install fan-out (`targetAgents`) so a
/// `-a all`-scale install is visible BEFORE `--yes` — for machines, not just
/// the human-readable plan.
#[cfg(unix)]
#[test]
fn source_sync_dry_run_json_lists_target_agents() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let source = tempfile::TempDir::new().unwrap();
	write_source_repo(source.path(), "my-skill");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", source.path())
		.args([
			"-g",
			"-a",
			"claude,grok",
			"source",
			"sync",
			"owner/testrepo",
			"--skill",
			"my-skill",
			"--install-missing",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["dryRun"], true);
	assert_eq!(
		json["targetAgents"],
		serde_json::json!(["claude", "grok"]),
		"dry-run JSON must list the exact fan-out: {json}"
	);
	assert_eq!(json["actions"][0]["action"], "install");
	// Dry-run wrote nothing.
	assert!(!home.path().join(".agents/skills/my-skill").exists());
}

/// An invalid agent list must fail BEFORE any fetch: no fetch root is set
/// here, so reaching the fetcher would report a network/credential error —
/// the unknown-agent error proves the selection was validated first.
#[cfg(unix)]
#[test]
fn source_sync_rejects_unknown_agent_before_fetch() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"claude,nonesuch",
			"source",
			"sync",
			"owner/testrepo",
			"--install-missing",
		])
		.output()
		.unwrap();
	assert!(!out.status.success());
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("nonesuch"),
		"must fail on the agent list, got: {stderr}"
	);
	assert!(
		!stderr.contains("Failed to fetch"),
		"must fail BEFORE the fetch, got: {stderr}"
	);
}

/// `--yes` with an agent list installs the master once and links each listed
/// agent — asserted on disk, not on the exit code alone.
#[cfg(unix)]
#[test]
fn source_sync_agent_list_installs_for_each_listed_agent() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let source = tempfile::TempDir::new().unwrap();
	write_source_repo(source.path(), "my-skill");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", source.path())
		.args([
			"-g",
			"-a",
			"claude,grok",
			"source",
			"sync",
			"owner/testrepo",
			"--skill",
			"my-skill",
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap();
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		out.status.success(),
		"stderr: {}\nstdout: {stdout}",
		String::from_utf8_lossy(&out.stderr)
	);
	// Master installed once; claude linked to it.
	let master = home.path().join(".agents/skills/my-skill");
	assert!(master.join("SKILL.md").exists(), "master must exist");
	let claude_link = home.path().join(".claude/skills/my-skill");
	assert!(
		claude_link.exists(),
		"claude must get its per-agent link; stdout: {stdout}"
	);
	// Both listed agents appear in the per-agent breakdown.
	assert!(
		stdout.contains("claude") && stdout.contains("grok"),
		"per-agent breakdown must name both agents: {stdout}"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_agent_list_preflights_before_writing_master() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let source = tempfile::TempDir::new().unwrap();
	write_source_skill(source.path(), "alpha", "alpha");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", source.path())
		.args([
			"-g",
			"-a",
			"claude,augmentcode",
			"source",
			"sync",
			"owner/repo",
			"--skill",
			"alpha",
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap();

	assert!(!out.status.success(), "unsupported target must reject sync");
	assert!(
		!home.path().join(".agents/skills/alpha").exists(),
		"capability preflight must happen before the shared Master write",
	);
	assert!(
		!home.path().join(".claude/skills/alpha").exists(),
		"no earlier agent may be linked before all targets pass preflight",
	);
}

#[cfg(unix)] // Windows: global MCP config not HOME-isolated
#[test]
fn cli_delete_mcp_dry_run_default() {
	// No --yes => dry-run: JSON reports dry_run:true/executed:false and the
	// MCP is still listed afterward (non-executed branch leaves state intact).
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	seed_mcp(home.path(), state.path(), "m");

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "delete", "mcps", "m"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	// Same snake_case RemovalView keys as skills, + {type,name} envelope.
	assert_eq!(json["type"], "mcp");
	assert_eq!(json["name"], "m");
	assert_eq!(json["dry_run"], true);
	assert_eq!(json["executed"], false);
	assert_eq!(json["needs_confirm"], false);
	assert!(json["paths"].is_array(), "paths must be an array: {json}");
	assert!(
		json["skipped"].is_array(),
		"skipped must be an array: {json}"
	);
	// dryRun camelCase must NOT leak (the snake_case flip, parity with skills).
	assert!(
		json.get("dryRun").is_none(),
		"dryRun must be absent: {json}"
	);

	assert!(
		mcp_listed(home.path(), state.path(), "m"),
		"dry-run must leave the MCP installed"
	);
}

#[cfg(unix)] // Windows: global MCP config not HOME-isolated
#[test]
fn cli_delete_mcp_yes_removes() {
	// --yes executes: JSON reports executed:true and the MCP is gone.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	seed_mcp(home.path(), state.path(), "goner");

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "delete", "mcps", "goner", "--yes"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["type"], "mcp");
	assert_eq!(json["name"], "goner");
	assert_eq!(json["executed"], true);
	assert_eq!(json["dry_run"], false);

	assert!(
		!mcp_listed(home.path(), state.path(), "goner"),
		"--yes must remove the MCP"
	);
}

#[cfg(unix)] // Windows: global MCP config not HOME-isolated
#[test]
fn cli_delete_mcp_missing_config_is_noop_ok() {
	// Audit (#5): the API answers DELETE on a missing config with an
	// idempotent no-op success body; the CLI must NOT error. No MCP was ever
	// added, so no config file exists.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "delete", "mcps", "ghost", "--yes"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"missing config delete must succeed; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["type"], "mcp");
	assert_eq!(json["name"], "ghost");
	assert_eq!(json["success"], true);
	assert_eq!(json["executed"], false);
	assert_eq!(json["paths"], serde_json::json!([]));
	assert_eq!(json["skipped"], serde_json::json!([]));
}

#[cfg(unix)] // Windows: global MCP config not HOME-isolated
#[test]
fn cli_delete_mcp_missing_name_is_noop_ok() {
	// Audit (#5): config exists but the named MCP does not. Match the API's
	// idempotent no-op (success:true, executed:false), not a ResourceNotFound
	// error. An unrelated MCP is seeded so the config file is present.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	seed_mcp(home.path(), state.path(), "other");

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "delete", "mcps", "ghost", "--yes"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"missing MCP name delete must succeed; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["type"], "mcp");
	assert_eq!(json["name"], "ghost");
	assert_eq!(json["success"], true);
	assert_eq!(json["executed"], false);

	// The unrelated MCP is untouched by the no-op.
	assert!(
		mcp_listed(home.path(), state.path(), "other"),
		"no-op delete must not touch other MCPs"
	);
}

// ==================== #4: SkillView command-surface contract ====================
//
// get/update/describe/add all now emit the core SkillView shape (snake_case,
// native_reader present, raw Skill `content` absent). These pin the exact wire
// keys so the changed command surfaces can't silently revert to the raw Skill
// serialization. Unix-gated for the same HOME-redirection reason as the delete
// tests above.

/// Assert a JSON object is a SkillView: snake_case keys, `native_reader`
/// present (the #4 advisory), and the raw-Skill-only `content` field absent.
#[cfg(unix)]
fn assert_skill_view_shape(obj: &Value) {
	assert!(
		obj.get("source_path").is_some(),
		"snake_case source_path key"
	);
	assert!(
		obj.get("native_reader").is_some(),
		"native_reader advisory present"
	);
	assert!(
		obj.get("content").is_none(),
		"raw Skill `content` must not leak into the view"
	);
}

#[cfg(unix)]
#[test]
fn get_skills_outputs_skill_view_shape() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	write_claude_skill(home.path(), "mytool");

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "get", "skills"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let arr = json.as_array().expect("get skills is a JSON array");
	let entry = arr
		.iter()
		.find(|s| s["name"] == "mytool")
		.expect("mytool in output");
	assert_skill_view_shape(entry);
	assert_eq!(entry["native_reader"], false);
	assert_eq!(entry["agent"], Value::Null, "single-agent get has no agent");
}

#[cfg(unix)]
#[test]
fn get_skills_all_agents_tags_agent_and_native_reader() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	write_claude_skill(home.path(), "mytool");

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "--agent", "all", "get", "skills"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let arr = json.as_array().expect("get skills is a JSON array");
	let entry = arr
		.iter()
		.find(|s| s["name"] == "mytool" && s["agent"] == "claude")
		.expect("mytool/claude entry in --agent all output");
	assert_skill_view_shape(entry);
	assert_eq!(entry["agent"], "claude", "--agent all tags the agent");
}

#[cfg(unix)]
#[test]
fn update_skill_outputs_skill_view_shape() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	write_claude_skill(home.path(), "mytool");

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude",
			"update",
			"skills",
			"mytool",
			"--description",
			"newdesc",
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_skill_view_shape(&json);
	assert_eq!(json["name"], "mytool");
	assert_eq!(json["description"], "newdesc");
	// update does no install prep, so the advisory stays false.
	assert_eq!(json["native_reader"], false);
}

#[cfg(unix)]
#[test]
fn describe_skill_outputs_skill_view_shape() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	write_claude_skill(home.path(), "mytool");

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "describe", "skills", "mytool"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_skill_view_shape(&json);
	assert_eq!(json["name"], "mytool");
}

#[cfg(unix)]
#[test]
fn add_skill_from_path_outputs_skill_view_with_native_reader() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	// A source skill on disk to import from.
	let src = home.path().join("src/myimport");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: myimport\ndescription: imported\n---\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude",
			"add",
			"skills",
			"--from",
			src.to_str().unwrap(),
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	// The --from branch must emit the SkillView DTO, not the raw Skill.
	assert_skill_view_shape(&json);
	assert_eq!(json["name"], "myimport");
	assert_eq!(json["description"], "imported");
	// Claude is not a NativeReader, so an isolated copy install => false.
	assert_eq!(json["native_reader"], false);
}

/// Root bypasses `0o555`, so probe + skip (CI often runs as root).
#[cfg(unix)]
fn perms_enforced(under: &std::path::Path) -> bool {
	use std::os::unix::fs::PermissionsExt;
	let p = under.join(".perm-probe");
	std::fs::create_dir(&p).unwrap();
	std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o555))
		.unwrap();
	let blocked = std::fs::write(p.join("x"), b"x").is_err();
	std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
		.unwrap();
	std::fs::remove_dir_all(&p).ok();
	blocked
}

/// A prune write failure is non-fatal: the skill is still deleted and the JSON
/// surfaces `prune_error` (read-only lock dir forces the post-delete lock write
/// to fail). Pins the `PruneStatus::Failed` -> `prune_error` serialization.
///
/// The mutation lock file is pre-created writable BEFORE the directory is frozen,
/// so the guard can still open it and this test keeps exercising the lock-WRITE
/// failure it is about. Without that the guard itself would be refused (it cannot
/// create its file in a read-only dir) and the delete would never run — which is
/// the intended behaviour for that case, covered separately by
/// `delete_skill_refuses_when_the_mutation_lock_cannot_be_created`.
#[cfg(unix)]
#[test]
fn delete_skill_yes_reports_prune_error_when_lock_unwritable() {
	use std::os::unix::fs::PermissionsExt;

	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "goner");
	let lock_path = seed_global_lock(state.path());
	let lock_dir = lock_path.parent().unwrap().to_path_buf();

	if !perms_enforced(&lock_dir) {
		eprintln!("skip: perms not enforced (root)");
		return;
	}
	std::fs::write(lock_dir.join(".aghub-mutation.lock"), b"").unwrap();
	let orig = std::fs::metadata(&lock_dir).unwrap().permissions();
	std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(0o555))
		.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json", "-a", "claude", "delete", "skills", "goner", "--yes",
		])
		.output()
		.unwrap();

	// Restore before asserting so a failed assert never leaks the temp dir.
	std::fs::set_permissions(&lock_dir, orig).unwrap();

	assert!(
		out.status.success(),
		"a prune failure is non-fatal; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["executed"], true);
	assert!(
		!skill_dir.exists(),
		"--yes deletes the skill even if prune fails"
	);
	assert!(
		json["prune_error"].is_string(),
		"prune failure must surface prune_error: {json}"
	);
	// One wire shape with the API/desktop DTO: never the legacy camelCase key.
	assert!(
		json.get("pruneError").is_none(),
		"prune_error must be snake_case to match the API DeleteSkillByPathResponse"
	);
}

/// The other half of the case above: when the interprocess mutation lock cannot
/// even be CREATED, the delete is REFUSED rather than run unprotected. Deleting
/// unlocked could remove a Master another aghub process just re-linked, and a
/// state dir this broken cannot record any mutation, so the user needs to see it
/// instead of finding a lock that silently drifted from disk.
#[cfg(unix)]
#[test]
fn delete_skill_refuses_when_the_mutation_lock_cannot_be_created() {
	use std::os::unix::fs::PermissionsExt;

	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "keeper");
	let lock_path = seed_global_lock(state.path());
	let lock_dir = lock_path.parent().unwrap().to_path_buf();

	if !perms_enforced(&lock_dir) {
		eprintln!("skip: perms not enforced (root)");
		return;
	}
	// No pre-created lock file this time, so the guard cannot open one.
	let orig = std::fs::metadata(&lock_dir).unwrap().permissions();
	std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(0o555))
		.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "delete", "skills", "keeper", "--yes"])
		.output()
		.unwrap();

	std::fs::set_permissions(&lock_dir, orig).unwrap();

	assert!(
		!out.status.success(),
		"an unacquirable mutation lock must refuse the delete"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("mutation lock") && stderr.contains("refused"),
		"the error must explain why nothing was deleted: {stderr}"
	);
	assert!(
		skill_dir.exists(),
		"a refused delete must leave the skill on disk"
	);
}

#[test]
fn prune_lock_default_dry_run_reports_orphan_without_mutating() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let lock_path = seed_global_lock(state.path());
	let before = std::fs::read(&lock_path).unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "prune-lock"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["dryRun"], true);
	assert!(json["pruned"]
		.as_array()
		.unwrap()
		.iter()
		.any(|n| n == "orphan"));
	assert_eq!(
		std::fs::read(&lock_path).unwrap(),
		before,
		"dry-run must not mutate the lock"
	);
}

#[test]
fn prune_lock_yes_removes_orphan_entry() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let lock_path = seed_global_lock(state.path());

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "prune-lock", "--yes"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let raw = std::fs::read_to_string(&lock_path).unwrap();
	let parsed: Value = serde_json::from_str(&raw).unwrap();
	assert!(
		parsed["skills"].get("orphan").is_none(),
		"orphan must be pruned: {raw}"
	);
	assert_eq!(parsed["version"], 3, "version preserved");
}

// ==================== Task #8 T5: apply-update --dry-run ====================

/// Seed a global lock entry whose `source` cannot be resolved offline (a
/// `file://` URL is not a valid GitHub shorthand and is skipped as a File
/// scheme), so `apply-update` fails deterministically at the resolve step
/// WITHOUT any network round-trip — letting us assert the pre-fetch flag gates.
#[cfg(unix)]
fn seed_unresolvable_global_lock(
	state: &std::path::Path,
	name: &str,
) -> std::path::PathBuf {
	let dir = state.join("skills");
	std::fs::create_dir_all(&dir).unwrap();
	let path = dir.join(".skill-lock.json");
	let body = format!(
		r#"{{"version":3,"skills":{{"{name}":{{"source":"file:///definitely/not/a/repo","sourceType":"git","sourceUrl":"file:///definitely/not/a/repo","skillPath":"SKILL.md","skillFolderHash":"","installedAt":"t","updatedAt":"t"}}}}}}"#
	);
	std::fs::write(&path, body).unwrap();
	path
}

/// Without `--yes`, apply-update refuses up front.
#[cfg(unix)]
#[test]
fn apply_update_without_yes_or_dry_run_refuses() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	write_claude_skill(home.path(), "mytool");
	seed_unresolvable_global_lock(state.path(), "mytool");

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "-g", "apply-update", "skills", "mytool"])
		.output()
		.unwrap();

	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(!out.status.success(), "must refuse: {stderr}");
	assert!(
		stderr.contains("without --yes"),
		"expected the --yes refusal: {stderr}"
	);
}

// ==================== Task 3.2-3.5: `source` subcommand ====================

#[test]
fn source_list_runs_with_no_agent_config() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	isolated_cli(home.path(), state.path())
		.args(["source", "list"])
		.assert()
		.success();
}

/// `source list -g -p` must reject the ambiguous both-scopes combo instead of
/// silently taking global.
#[test]
fn source_list_rejects_both_scopes() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["-g", "-p", "source", "list"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "both scopes must fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("mutually exclusive"),
		"expected both-scope rejection: {stderr}"
	);
}

/// Top-level `--all` has no meaning for `transfer`/`reconcile` (single writing
/// scope); it must be rejected, not silently ignored.
#[test]
fn transfer_rejects_all_scope() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--all",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"x",
			"--to",
			"cursor",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "--all must be rejected");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("'all'"),
		"expected --all rejection: {stderr}"
	);
}

/// Codex finding (global/`--all` source paths must not require a project root):
/// the old code called `current_project_root()` — a `current_dir()`/`getcwd()`
/// syscall — UNCONDITIONALLY, so from a broken/deleted cwd even a global-only
/// invocation died with "No such file or directory" before it could do the
/// global-only work (or, for `--all sync`, before the `--all` rejection).
///
/// These tests reproduce that exact "deleted cwd" condition: spawn the binary
/// with a cwd that exists at fork (so `chdir` succeeds) but is removed in
/// `pre_exec` before exec, so the child's `getcwd()` returns ENOENT. A correct
/// global path must NOT touch the cwd and so must succeed (or, for `--all sync`,
/// fail for the RIGHT reason). Unix-only: `pre_exec`/deleted-cwd is POSIX.
#[cfg(unix)]
fn cli_in_deleted_cwd(
	home: &std::path::Path,
	state: &std::path::Path,
	fetch_root: Option<&std::path::Path>,
	args: &[&str],
) -> std::process::Output {
	use std::os::unix::process::CommandExt;

	let gone = tempfile::TempDir::new().unwrap();
	let gone_path = gone.path().to_path_buf();

	let mut cmd =
		std::process::Command::new(assert_cmd::cargo::cargo_bin("aghub-cli"));
	cmd.env("HOME", home)
		.env("USERPROFILE", home)
		.env("APPDATA", home)
		.env("XDG_STATE_HOME", state)
		.current_dir(&gone_path)
		.args(args);
	if let Some(root) = fetch_root {
		cmd.env("AGHUB_TEST_SOURCE_FETCH_ROOT", root);
	}
	// SAFETY: `pre_exec` runs in the forked child before exec; removing the cwd
	// here makes `getcwd()` ENOENT for the child without racing the parent.
	unsafe {
		cmd.pre_exec(move || {
			std::fs::remove_dir(&gone_path).ok();
			Ok(())
		});
	}
	cmd.output().unwrap()
}

#[cfg(unix)]
#[test]
fn source_list_global_does_not_touch_cwd() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = cli_in_deleted_cwd(
		home.path(),
		state.path(),
		None,
		&["-g", "source", "list"],
	);
	assert!(
		out.status.success(),
		"`-g source list` must not resolve a project root: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}

#[cfg(unix)]
#[test]
fn source_diff_global_does_not_touch_cwd() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");
	let out = cli_in_deleted_cwd(
		home.path(),
		state.path(),
		Some(src.path()),
		&["-g", "source", "diff", "owner/repo"],
	);
	assert!(
		out.status.success(),
		"`-g source diff` must not resolve a project root: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}

#[cfg(unix)]
#[test]
fn source_sync_global_does_not_touch_cwd() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");
	let out = cli_in_deleted_cwd(
		home.path(),
		state.path(),
		Some(src.path()),
		&["-g", "source", "sync", "owner/repo", "--install-missing"],
	);
	assert!(
		out.status.success(),
		"`-g source sync` must not resolve a project root: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}

/// `--all source sync` is rejected (`--all` is meaningless for a single write
/// scope). That rejection must happen BEFORE any cwd IO: from a deleted cwd the
/// old code hit `current_dir()` ENOENT and failed with the wrong error before
/// reaching the `AllNotAllowedForWrite` guard. Assert it fails for the RIGHT
/// reason (the scope message), not "No such file or directory".
#[cfg(unix)]
#[test]
fn source_sync_all_rejected_before_cwd_io() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");
	let out = cli_in_deleted_cwd(
		home.path(),
		state.path(),
		Some(src.path()),
		&["--all", "source", "sync", "owner/repo", "--install-missing"],
	);
	assert!(
		!out.status.success(),
		"`--all source sync` must be rejected"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("--all is not allowed"),
		"must fail with the scope error, not a cwd 'No such file or \
		 directory': {stderr}"
	);
}

/// Write a source dir containing one skill `<name>/SKILL.md` with frontmatter.
fn write_source_skill(
	root: &std::path::Path,
	dir: &str,
	name: &str,
) -> std::path::PathBuf {
	let skill_dir = root.join(dir);
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: d\n---\nbody\n"),
	)
	.unwrap();
	skill_dir
}

/// Count symlinks named `name` anywhere under `root` (the per-agent skill links;
/// the `.agents/skills/<name>` master is a real dir and is NOT counted). Used to
/// assert that `-a all` linked more than one agent.
#[cfg(unix)]
fn count_symlinks_named(root: &std::path::Path, name: &str) -> usize {
	let mut n = 0usize;
	let mut stack = vec![root.to_path_buf()];
	while let Some(dir) = stack.pop() {
		let Ok(rd) = std::fs::read_dir(&dir) else {
			continue;
		};
		for e in rd.flatten() {
			let Ok(ft) = e.file_type() else { continue };
			let p = e.path();
			if ft.is_symlink() {
				if p.file_name().and_then(|s| s.to_str()) == Some(name) {
					n += 1;
				}
			} else if ft.is_dir() {
				stack.push(p);
			}
		}
	}
	n
}

/// Run `-g -a <agent> source sync owner/repo --skill <skill> --install-missing
/// --yes` against the local fetch source — the shape shared by the sync tests
/// below; only `agent` and `skill` vary.
#[cfg(unix)]
fn run_sync_install(
	home: &std::path::Path,
	state: &std::path::Path,
	src: &std::path::Path,
	agent: &str,
	skill: &str,
) -> std::process::Output {
	isolated_cli(home, state)
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src)
		.args([
			"-g",
			"-a",
			agent,
			"source",
			"sync",
			"owner/repo",
			"--skill",
			skill,
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap()
}

#[test]
fn source_diff_reports_not_installed() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args(["source", "diff", "owner/repo"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(stdout.contains("alpha"), "stdout: {stdout}");
	assert!(stdout.contains("notInstalled"), "stdout: {stdout}");
}

#[cfg(unix)]
#[test]
fn source_sync_dry_run_writes_nothing() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args(["-g", "source", "sync", "owner/repo", "--install-missing"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	// Nothing written: no claude skills dir entry, no global lock.
	let agent_skill = home.path().join(".claude/skills/alpha");
	assert!(!agent_skill.exists(), "dry-run must not install the skill");
	let lock = state.path().join("skills/.skill-lock.json");
	assert!(!lock.exists(), "dry-run must not create the global lock");
}

#[cfg(unix)]
#[test]
fn source_sync_project_scope_reports_project_and_skips_global_lock() {
	// Finding 2 (project-scope boundary): `source sync -p` must map the
	// `Project` selector through the shared `write_scope` mapper back to the
	// CLI's `ProjectOnly` scope, carry the detected project root, and emit the
	// "project" label — never touching the global lock. The shared mapper unit
	// test only covers the pure function; this locks the CLI end to end.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	// `.claude/` is an agent marker, so `find_project_root` detects the root.
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let mut cmd = isolated_cli(home.path(), state.path());
	cmd.current_dir(project.path());
	let out = cmd
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-p",
			"source",
			"sync",
			"owner/repo",
			"--install-missing",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(
		json["scope"].as_str(),
		Some("project"),
		"project-scope sync must report the project label: {json}"
	);

	// Dry-run by default: writes nothing to either lock.
	let global_lock = state.path().join("skills/.skill-lock.json");
	assert!(
		!global_lock.exists(),
		"project-scope sync must never touch the global lock"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_project_scope_yes_writes_project_lock_not_global() {
	// Finding 3 (project-scope WRITE path): `source sync -p --yes` must apply
	// to the PROJECT scope — writing `<project>/skills-lock.json` and the
	// project agent dir — and NEVER touch the global lock. The dry-run test
	// above exercises only the read prologue; this locks the ResourceScope /
	// project_root wiring on the actual `--yes` apply branch.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let mut cmd = isolated_cli(home.path(), state.path());
	cmd.current_dir(project.path());
	let out = cmd
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-p",
			"-a",
			"claude",
			"source",
			"sync",
			"owner/repo",
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	// Project lock written at the project root, recording the skill.
	let project_lock = project.path().join("skills-lock.json");
	assert!(
		project_lock.exists(),
		"project-scope --yes must write the PROJECT lock"
	);
	let raw = std::fs::read_to_string(&project_lock).unwrap();
	let parsed: Value = serde_json::from_str(&raw).unwrap();
	assert!(
		!parsed["skills"]["alpha"].is_null(),
		"alpha must be recorded in the project lock: {raw}"
	);

	// The global lock must NOT exist — project scope writes only the project.
	let global_lock = state.path().join("skills/.skill-lock.json");
	assert!(
		!global_lock.exists(),
		"project-scope --yes must never write the global lock"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_skill_filter_installs_only_named_skill() {
	// `--skill alpha` must narrow --install-missing to just `alpha`, leaving a
	// sibling `beta` in the same source untouched — the selective-install path
	// that spares you the source's other skills.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");
	write_source_skill(src.path(), "beta", "beta");

	let out = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"alpha",
	);
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	// alpha installed; beta (a sibling in the same source) left alone.
	assert!(
		home.path().join(".claude/skills/alpha").exists(),
		"--skill alpha must install alpha"
	);
	assert!(
		!home.path().join(".claude/skills/beta").exists(),
		"--skill alpha must NOT install the sibling beta"
	);

	let lock = state.path().join("skills/.skill-lock.json");
	let raw = std::fs::read_to_string(&lock).unwrap();
	let parsed: Value = serde_json::from_str(&raw).unwrap();
	assert!(
		!parsed["skills"]["alpha"].is_null(),
		"alpha must be recorded in the global lock: {raw}"
	);
	assert!(
		parsed["skills"]["beta"].is_null(),
		"beta must NOT be recorded in the lock: {raw}"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_skill_filter_unknown_name_warns_and_installs_nothing() {
	// A typo'd `--skill` name must surface as a warning listing the available
	// skills (not a silent no-op) and install nothing.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let out = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"nope",
	);
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(stderr.contains("no skill named"), "should warn: {stderr}");
	assert!(
		stderr.contains("alpha"),
		"warning should list available skills: {stderr}"
	);
	assert!(
		!home.path().join(".claude/skills/alpha").exists(),
		"an unknown --skill name must install nothing"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_all_agents_links_more_than_one_agent() {
	// `-a all` must fan the install across every registered agent, not just
	// claude — the multi-agent extract-and-replace scenario. One shared master
	// plus a per-agent symlink for each non-native-reader agent.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let out =
		run_sync_install(home.path(), state.path(), src.path(), "all", "alpha");
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	// Master materialized once; MANY agents linked (far more than the single
	// claude link a default sync would create).
	assert!(
		home.path().join(".agents/skills/alpha").is_dir(),
		"the shared master must exist"
	);
	let links = count_symlinks_named(home.path(), "alpha");
	assert!(
		links >= 2,
		"`-a all` must link more than one agent, found {links} symlink(s)"
	);
	assert!(
		home.path().join(".claude/skills/alpha").exists(),
		"claude must be among the linked agents"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_all_agents_repairs_missing_links_after_single_agent_install() {
	// The Codex-flagged hazard: once a single-agent install writes the scope
	// lock, a later `--install-missing` is lock-gated and would no-op, silently
	// leaving other agents unlinked. With an explicit `--skill`, `-a all` must
	// ENSURE (idempotently re-materialize) the named skill for every agent even
	// though the lock already says "installed".
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	// Step 1: single-agent install (claude only).
	let out1 = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"alpha",
	);
	assert!(out1.status.success());
	assert_eq!(
		count_symlinks_named(home.path(), "alpha"),
		1,
		"single-agent install must link exactly one agent"
	);

	// Step 2: `-a all` re-run must repair — link the rest despite the lock.
	let out2 =
		run_sync_install(home.path(), state.path(), src.path(), "all", "alpha");
	assert!(
		out2.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out2.stderr)
	);
	assert!(
		count_symlinks_named(home.path(), "alpha") >= 2,
		"`-a all` re-run must repair the missing agent links (ensure semantic)"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_all_agents_output_follows_registry_order() {
	// `-a all` must list agents in registry::ALL_AGENTS order (claude-first) —
	// NOT AgentType::ALL order (cursor-first). Locks the ordering so a future
	// switch back to AgentType::ALL can't silently reorder output/first-error.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-g",
			"-a",
			"all",
			"source",
			"sync",
			"owner/repo",
			"--skill",
			"alpha",
			"--install-missing",
			"--yes",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let v: Value = serde_json::from_slice(&out.stdout).unwrap();
	let order: Vec<&str> = v["actions"][0]["agents"]
		.as_array()
		.expect("agents array present for a multi-agent install")
		.iter()
		.map(|a| a["agent"].as_str().unwrap())
		.collect();
	let pos = |name: &str| order.iter().position(|a| *a == name);
	// claude leads registry::ALL_AGENTS; cursor leads AgentType::ALL.
	assert_eq!(
		order.first().copied(),
		Some("claude"),
		"registry order → claude first, got {order:?}"
	);
	assert!(
		pos("claude") < pos("cursor"),
		"claude must precede cursor (registry, not AgentType::ALL order): {order:?}"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_conflict_exits_nonzero() {
	// A foreign real dir occupying an agent's skill slot is a conflict (never
	// clobbered). It must surface as a NON-ZERO exit, not a swallowed success.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");
	// Occupy claude's slot with a real dir that is NOT a link to our master.
	let slot = home.path().join(".claude/skills/alpha");
	std::fs::create_dir_all(&slot).unwrap();
	std::fs::write(slot.join("FOREIGN.md"), "not ours").unwrap();

	let out = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"alpha",
	);
	assert!(
		!out.status.success(),
		"a conflict must exit non-zero; stdout: {} stderr: {}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	);
}

#[cfg(unix)]
#[test]
fn source_sync_already_linked_exits_zero() {
	// Re-running an ensure on an already-linked skill is a no-op success
	// ("already present" is NOT a failure) and must exit zero.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let mk = || {
		run_sync_install(
			home.path(),
			state.path(),
			src.path(),
			"claude",
			"alpha",
		)
	};
	assert!(mk().status.success(), "first install must succeed");
	let again = mk();
	assert!(
		again.status.success(),
		"re-run on an already-linked skill must exit zero; stderr: {}",
		String::from_utf8_lossy(&again.stderr)
	);
}

#[cfg(unix)]
#[test]
fn source_sync_project_scope_without_project_root_errors() {
	// Finding 3 (project-scope, no root): `source sync -p` from a directory
	// with no agent marker has no project root to write to. The shared
	// `write_scope` mapper rejects it (`ProjectRootRequired`) and the CLI
	// must surface that as a failure — not silently fall back to global.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	// A bare cwd with NO agent marker => `find_project_root` returns None.
	let bare = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let mut cmd = isolated_cli(home.path(), state.path());
	cmd.current_dir(bare.path());
	let out = cmd
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args(["-p", "source", "sync", "owner/repo", "--install-missing"])
		.output()
		.unwrap();
	assert!(
		!out.status.success(),
		"`-p` with no project root must fail, not default to global"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("no project root"),
		"expected the project-root-required error: {stderr}"
	);

	// Nothing written to the global lock as a side effect.
	let global_lock = state.path().join("skills/.skill-lock.json");
	assert!(
		!global_lock.exists(),
		"a rejected project-scope sync must not write the global lock"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_yes_installs_missing() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-g",
			"-a",
			"claude",
			"source",
			"sync",
			"owner/repo",
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let agent_skill = home.path().join(".claude/skills/alpha/SKILL.md");
	assert!(agent_skill.exists(), "--yes must install the skill");

	// A follow-up `source list` shows the source.
	let list = isolated_cli(home.path(), state.path())
		.args(["source", "list", "--json"])
		.output()
		.unwrap();
	assert!(list.status.success());
	let json: Value = serde_json::from_slice(&list.stdout).unwrap();
	let raw = json.to_string();
	assert!(raw.contains("owner/repo"), "source list: {raw}");
}

/// Seed a global lock entry for `source` recording a non-default `ref` and a
/// `skillPath` that does NOT match the discovered skill (so the discovered
/// skill classifies as notInstalled and `--install-missing` fires). Mirrors the
/// fields the CLI/API write so the recorded-ref fallback has something to read.
#[cfg(unix)]
fn seed_global_lock_with_ref(
	state: &std::path::Path,
	source: &str,
	ref_name: &str,
) -> std::path::PathBuf {
	let dir = state.join("skills");
	std::fs::create_dir_all(&dir).unwrap();
	let path = dir.join(".skill-lock.json");
	let body = format!(
		r#"{{"version":3,"skills":{{"other":{{"source":"{source}","sourceType":"github","sourceUrl":"https://github.com/{source}","ref":"{ref_name}","skillPath":"unrelated/SKILL.md","skillFolderHash":"","installedAt":"t","updatedAt":"t"}}}}}}"#
	);
	std::fs::write(&path, body).unwrap();
	path
}

#[cfg(unix)]
#[test]
fn source_sync_yes_records_recorded_ref_on_install() {
	// Finding 2 (ref_name parity): a source already in the lock with a recorded
	// `ref` (and NO `--ref` flag) must persist that recorded ref on the freshly
	// installed skill's lock entry — matching what the API records — NOT None.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");
	let lock_path = seed_global_lock_with_ref(state.path(), "owner/repo", "v2");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-g",
			"-a",
			"claude",
			"source",
			"sync",
			"owner/repo",
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let agent_skill = home.path().join(".claude/skills/alpha/SKILL.md");
	assert!(agent_skill.exists(), "--yes must install the skill");

	let raw = std::fs::read_to_string(&lock_path).unwrap();
	let parsed: Value = serde_json::from_str(&raw).unwrap();
	let alpha = &parsed["skills"]["alpha"];
	assert!(!alpha.is_null(), "alpha entry must be written: {raw}");
	assert_eq!(
		alpha["ref"].as_str(),
		Some("v2"),
		"installed entry must persist the recorded ref, not None: {raw}"
	);
}

#[cfg(unix)]
#[test]
fn source_sync_skips_deprecated_skill() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "deprecated/foo", "foo");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-g",
			"-a",
			"claude",
			"source",
			"sync",
			"owner/repo",
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let agent_skill = home.path().join(".claude/skills/foo");
	assert!(
		!agent_skill.exists(),
		"a deprecated skill must not be installed"
	);
}
#[cfg(unix)]
#[test]
fn source_sync_no_action_flag_prints_plan_and_guidance() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args(["-g", "source", "sync", "owner/repo"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let stdout = String::from_utf8_lossy(&out.stdout);
	// The plan: the per-skill state overview lists `alpha`.
	assert!(
		stdout.contains("alpha"),
		"plan must list the skill: {stdout}"
	);
	// The guidance: no action selected, pointing at --install-missing.
	assert!(
		stdout.contains("No action selected"),
		"missing guidance: {stdout}"
	);
	assert!(
		stdout.contains("--install-missing"),
		"missing flag guidance: {stdout}"
	);

	// Wrote NOTHING: no agent skill dir entry, no global lock.
	let agent_skill = home.path().join(".claude/skills/alpha");
	assert!(
		!agent_skill.exists(),
		"no-action sync must not install the skill"
	);
	let lock = state.path().join("skills/.skill-lock.json");
	assert!(
		!lock.exists(),
		"no-action sync must not create the global lock"
	);
}

/// Seed a global lock entry under `name` recording the source coordinates an
/// accept-rename needs (`source`/`sourceUrl`/`skillPath`). `skill_path` points
/// at the renamed skill's location inside the fetched source tree.
#[cfg(unix)]
fn seed_global_lock_entry(
	state: &std::path::Path,
	name: &str,
	source: &str,
	skill_path: &str,
) -> std::path::PathBuf {
	let dir = state.join("skills");
	std::fs::create_dir_all(&dir).unwrap();
	let path = dir.join(".skill-lock.json");
	let body = format!(
		r#"{{"version":3,"skills":{{"{name}":{{"source":"{source}","sourceType":"github","sourceUrl":"https://github.com/{source}","skillPath":"{skill_path}","skillFolderHash":"","installedAt":"t","updatedAt":"t"}}}}}}"#
	);
	std::fs::write(&path, body).unwrap();
	path
}

#[cfg(unix)]
#[test]
fn source_accept_rename_installs_new_removes_old() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	// Old skill is installed under the Claude agent dir.
	let old_dir = home.path().join(".claude/skills/old-skill");
	std::fs::create_dir_all(&old_dir).unwrap();
	std::fs::write(
		old_dir.join("SKILL.md"),
		"---\nname: old-skill\ndescription: original\n---\nbody\n",
	)
	.unwrap();

	// Global lock records the source coordinates for the old name. The locked
	// skillPath points at where the RENAMED skill lives in the fetched source.
	let lock_path = seed_global_lock_entry(
		state.path(),
		"old-skill",
		"owner/repo",
		"new-dir/SKILL.md",
	);

	// The fetched source: `new-dir/SKILL.md` now declares `name: new-skill`.
	let fetch_root = tempfile::TempDir::new().unwrap();
	let new_skill_dir = fetch_root.path().join("new-dir");
	std::fs::create_dir_all(&new_skill_dir).unwrap();
	std::fs::write(
		new_skill_dir.join("SKILL.md"),
		"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", fetch_root.path())
		.args([
			"-g",
			"-a",
			"claude",
			"source",
			"accept-rename",
			"old-skill",
			"new-skill",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	assert!(
		home.path()
			.join(".claude/skills/new-skill/SKILL.md")
			.exists(),
		"new skill must be installed"
	);
	assert!(
		!home.path().join(".claude/skills/old-skill").exists(),
		"old skill must be removed"
	);

	// Lock transitioned from old-skill to new-skill.
	let raw = std::fs::read_to_string(&lock_path).unwrap();
	let parsed: Value = serde_json::from_str(&raw).unwrap();
	assert!(
		parsed["skills"]["old-skill"].is_null(),
		"old lock entry must be removed: {raw}"
	);
	assert!(
		!parsed["skills"]["new-skill"].is_null(),
		"new lock entry must be written: {raw}"
	);
}

#[cfg(unix)]
#[test]
fn source_accept_rename_dry_run_writes_nothing() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let old_dir = home.path().join(".claude/skills/old-skill");
	std::fs::create_dir_all(&old_dir).unwrap();
	std::fs::write(
		old_dir.join("SKILL.md"),
		"---\nname: old-skill\ndescription: original\n---\nbody\n",
	)
	.unwrap();
	let lock_path = seed_global_lock_entry(
		state.path(),
		"old-skill",
		"owner/repo",
		"new-dir/SKILL.md",
	);

	let fetch_root = tempfile::TempDir::new().unwrap();
	let new_skill_dir = fetch_root.path().join("new-dir");
	std::fs::create_dir_all(&new_skill_dir).unwrap();
	std::fs::write(
		new_skill_dir.join("SKILL.md"),
		"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", fetch_root.path())
		.args([
			"-g",
			"-a",
			"claude",
			"source",
			"accept-rename",
			"old-skill",
			"new-skill",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	// Dry-run mutates nothing: old skill stays, new skill is not installed.
	assert!(
		home.path().join(".claude/skills/old-skill").exists(),
		"dry-run must not remove the old skill"
	);
	assert!(
		!home.path().join(".claude/skills/new-skill").exists(),
		"dry-run must not install the new skill"
	);
	let raw = std::fs::read_to_string(&lock_path).unwrap();
	assert!(
		raw.contains("old-skill"),
		"dry-run must keep the old lock entry"
	);
	assert!(
		!raw.contains("new-skill"),
		"dry-run must not write new entry"
	);
}

/// P0-2 guard (b): accept-rename must refuse when the new name is ALREADY
/// installed (on-disk dir), leaving the pre-existing skill untouched. Without
/// the guard the install would clobber it.
#[cfg(unix)]
#[test]
fn source_accept_rename_rejects_when_new_name_installed() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let old_dir = home.path().join(".claude/skills/old-skill");
	std::fs::create_dir_all(&old_dir).unwrap();
	std::fs::write(
		old_dir.join("SKILL.md"),
		"---\nname: old-skill\ndescription: original\n---\nbody\n",
	)
	.unwrap();
	// New skill ALREADY present with sentinel content.
	let new_dir = home.path().join(".claude/skills/new-skill");
	std::fs::create_dir_all(&new_dir).unwrap();
	let pre_existing =
		"---\nname: new-skill\ndescription: PRE-EXISTING\n---\nkeep\n";
	std::fs::write(new_dir.join("SKILL.md"), pre_existing).unwrap();

	let lock_path = seed_global_lock_entry(
		state.path(),
		"old-skill",
		"owner/repo",
		"new-dir/SKILL.md",
	);

	let fetch_root = tempfile::TempDir::new().unwrap();
	let new_skill_src = fetch_root.path().join("new-dir");
	std::fs::create_dir_all(&new_skill_src).unwrap();
	std::fs::write(
		new_skill_src.join("SKILL.md"),
		"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", fetch_root.path())
		.args([
			"-g",
			"-a",
			"claude",
			"source",
			"accept-rename",
			"old-skill",
			"new-skill",
			"--yes",
		])
		.output()
		.unwrap();

	assert!(
		!out.status.success(),
		"must refuse to clobber an existing new-skill; stdout: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	// Pre-existing new-skill dir must be untouched.
	let still = std::fs::read_to_string(new_dir.join("SKILL.md")).unwrap();
	assert_eq!(still, pre_existing, "new-skill must not be clobbered");
	// Old skill + its lock entry remain (nothing mutated).
	assert!(old_dir.exists(), "old skill dir must remain");
	let raw = std::fs::read_to_string(&lock_path).unwrap();
	assert!(
		raw.contains("old-skill"),
		"old lock entry must remain: {raw}"
	);
}

/// P0-2 guard (a): a degenerate rename whose old/new names sanitize to the same
/// on-disk dir must be rejected up front (before any fetch/mutation).
#[cfg(unix)]
#[test]
fn source_accept_rename_rejects_degenerate_sanitized_collision() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	// "old skill" and "old-skill" both sanitize to "old-skill".
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"claude",
			"source",
			"accept-rename",
			"old skill",
			"old-skill",
			"--yes",
		])
		.output()
		.unwrap();

	assert!(
		!out.status.success(),
		"degenerate rename must be rejected; stdout: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("same on-disk skill"),
		"expected degenerate-rename error, got: {stderr}"
	);
}

#[test]
fn source_list_json_runs_with_no_agent_config() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["source", "list", "--json"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	// Empty lock -> empty JSON array.
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert!(json.is_array(), "expected a JSON array, got: {json}");
}

#[test]
fn source_diff_json_uses_wire_state_strings() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "alpha", "alpha");

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args(["source", "diff", "owner/repo", "--json"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	// Find the `alpha` skill across all scopes and assert the wire state string.
	let raw = json.to_string();
	assert!(raw.contains("notInstalled"), "json: {raw}");
}
#[test]
fn source_help_renders() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	isolated_cli(home.path(), state.path())
		.args(["source", "--help"])
		.assert()
		.success();
}

#[test]
fn source_sync_help_renders() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	isolated_cli(home.path(), state.path())
		.args(["source", "sync", "--help"])
		.assert()
		.success();
}

// ===== Task 25 [#2]: scope-mapper end-to-end message contract =====
// These pin the three `source sync` scope rejections to their exact CLI
// messages so the collapse onto `skill_update::sources::write_scope`
// (ScopeError Display) stays behavior-preserving end to end.

#[test]
fn source_sync_all_is_rejected() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args(["--all", "source", "sync", "owner/repo", "--install-missing"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "--all must be rejected for sync");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(stderr.contains("--all is not allowed"), "stderr: {stderr}");
}

#[test]
fn source_sync_without_scope_is_rejected() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args(["source", "sync", "owner/repo", "--install-missing"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "no scope must be rejected for sync");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(stderr.contains("needs a scope"), "stderr: {stderr}");
}

#[test]
fn source_sync_both_flags_is_rejected() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-p",
			"source",
			"sync",
			"owner/repo",
			"--install-missing",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "both -g/-p must be rejected");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(stderr.contains("mutually exclusive"), "stderr: {stderr}");
}

#[test]
fn source_diff_help_still_lists_flags() {
	// No regression to the Diff surface from adding the Credential variant.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args(["source", "diff", "--help"])
		.output()
		.unwrap();
	assert!(out.status.success(), "diff --help must render");
	let help = String::from_utf8_lossy(&out.stdout);
	assert!(help.contains("--ref"), "diff still has --ref: {help}");
	assert!(help.contains("--json"), "diff still has --json: {help}");
}

// Symlink-only install: `aghub add skill --from <dir>` writes a single
// .agents/skills/<name> master and a link in the agent's own dir — never an
// isolated copy. The legacy `--universal` flag is accepted but ignored (no
// unknown-arg error, identical result), proving the deprecation no-op.
#[cfg(unix)]
#[test]
fn cli_add_skill_from_path_is_symlink_only() {
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path();
	// Agent marker so project-root detection picks this dir.
	std::fs::create_dir_all(project.join(".claude")).unwrap();
	let src = project.join("src/my-skill");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: my-skill\ndescription: d\n---\nbody",
	)
	.unwrap();

	// Project scope; isolate HOME so nothing leaks to the real ~/.agents.
	let mut cmd = assert_cmd::Command::cargo_bin("aghub-cli").unwrap();
	cmd.env("HOME", project)
		.env("USERPROFILE", project)
		.env("APPDATA", project)
		.current_dir(project)
		.args(["-a", "claude", "-p", "add", "skill", "--from"])
		.arg(src.join("SKILL.md"));
	cmd.assert().success();

	let canonical = project.join(".agents/skills/my-skill");
	let link = project.join(".claude/skills/my-skill");
	assert!(
		canonical.join("SKILL.md").exists(),
		"a .agents master must be materialized"
	);
	assert!(
		std::fs::symlink_metadata(&link)
			.map(|m| m.file_type().is_symlink())
			.unwrap_or(false),
		"the agent dir must hold a link, not a copy"
	);

	// Legacy `--universal` flag: accepted (no unknown-arg error). Use a fresh
	// skill name so the duplicate-name guard does not reject it.
	let src2 = project.join("src/other-skill");
	std::fs::create_dir_all(&src2).unwrap();
	std::fs::write(
		src2.join("SKILL.md"),
		"---\nname: other-skill\ndescription: d\n---\nbody",
	)
	.unwrap();
	let mut cmd2 = assert_cmd::Command::cargo_bin("aghub-cli").unwrap();
	cmd2.env("HOME", project)
		.env("USERPROFILE", project)
		.env("APPDATA", project)
		.current_dir(project)
		.args([
			"-a",
			"claude",
			"-p",
			"add",
			"skill",
			"--universal",
			"--from",
		])
		.arg(src2.join("SKILL.md"));
	cmd2.assert().success();
	assert!(
		std::fs::symlink_metadata(project.join(".claude/skills/other-skill"),)
			.map(|m| m.file_type().is_symlink())
			.unwrap_or(false),
		"--universal must be accepted and produce the same symlink result"
	);
}

/// `--universal` is deprecated: the flag is accepted (exit 0), prints a
/// one-line deprecation notice to stderr, and still produces the
/// master + per-agent symlink (no isolated copy).
#[cfg(unix)]
#[test]
fn add_skill_universal_flag_prints_deprecation_notice() {
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path();
	// Agent marker so project-root detection picks this dir.
	std::fs::create_dir_all(project.join(".claude")).unwrap();
	let src = project.join("src/dep-skill");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: dep-skill\ndescription: d\n---\nbody",
	)
	.unwrap();

	let out = assert_cmd::Command::cargo_bin("aghub-cli")
		.unwrap()
		.env("HOME", project)
		.env("USERPROFILE", project)
		.env("APPDATA", project)
		.current_dir(project)
		.args([
			"-a",
			"claude",
			"-p",
			"add",
			"skill",
			"--universal",
			"--from",
		])
		.arg(src.join("SKILL.md"))
		.output()
		.unwrap();

	// exits 0
	assert!(
		out.status.success(),
		"--universal must exit 0; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	// deprecation notice on stderr
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("--universal is deprecated"),
		"expected deprecation notice on stderr, got: {stderr}"
	);

	// master + per-agent symlink produced (not a copy)
	let master = project.join(".agents/skills/dep-skill");
	let link = project.join(".claude/skills/dep-skill");
	assert!(
		master.join("SKILL.md").exists(),
		".agents master must be materialized"
	);
	assert!(
		std::fs::symlink_metadata(&link)
			.map(|m| m.file_type().is_symlink())
			.unwrap_or(false),
		"the agent dir must hold a symlink, not a copy"
	);
}

// ==================== #7: MCP transport flag validation + --timeout ====

#[test]
fn add_mcp_command_with_header_is_rejected() {
	// --header is only valid with --url; a stdio (--command) MCP must reject it
	// instead of silently dropping it.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--command",
			"echo",
			"--header",
			"A:B",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "must reject --header with --command");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("--header is only valid with --url"),
		"stderr must explain the rejection, got: {stderr}"
	);
}

#[test]
fn add_mcp_url_with_env_is_rejected() {
	// --env is only valid with --command; a url MCP must reject it.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-a", "claude", "add", "mcps", "--name", "m", "--url", "http://h",
			"--env", "K=V",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "must reject --env with --url");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("--env is only valid with --command"),
		"stderr must explain the rejection, got: {stderr}"
	);
}

#[test]
fn add_mcp_command_and_url_is_rejected() {
	// clap's mutually-exclusive group already forbids this; pin the behavior.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--command",
			"echo",
			"--url",
			"http://h",
		])
		.output()
		.unwrap();
	assert!(
		!out.status.success(),
		"--command and --url together must be rejected"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("--command") && stderr.contains("--url"),
		"rejection must name both flags, got: {stderr}"
	);
}

#[test]
fn add_mcp_command_with_malformed_header_is_rejected() {
	// A malformed --header (no colon) must error, not be silently dropped.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--command",
			"echo",
			"--header",
			"bad",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "malformed --header must be rejected");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("KEY:VALUE"),
		"stderr must name the expected format, got: {stderr}"
	);
}

#[test]
fn add_mcp_command_with_malformed_env_is_rejected() {
	// A malformed --env (no equals) must error, not be silently dropped.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--command",
			"echo",
			"--env",
			"BAD",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "malformed --env must be rejected");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("KEY=VALUE"),
		"stderr must name the expected format, got: {stderr}"
	);
}

#[test]
fn add_mcp_zero_timeout_is_rejected() {
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--url",
			"http://h",
			"--timeout",
			"0",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "--timeout 0 must be rejected");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("timeout must be greater than 0"),
		"stderr must explain the rejection, got: {stderr}"
	);
}

#[cfg(unix)] // Windows: global MCP config not HOME-isolated
#[test]
fn add_mcp_url_with_timeout_succeeds_and_sets_it() {
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--url",
			"http://h",
			"--timeout",
			"30",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"valid --timeout must succeed; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(
		v["transport"]["timeout"], 30,
		"printed transport must carry timeout:30, got: {v}"
	);
}

#[test]
fn add_mcp_unknown_transport_is_rejected() {
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--url",
			"http://h",
			"--transport",
			"bogus",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "unknown transport must be rejected");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("unknown transport type"),
		"stderr must explain the rejection, got: {stderr}"
	);
}

#[cfg(unix)] // Windows: global MCP config not HOME-isolated
#[test]
fn update_mcp_timeout_flag_overrides_existing() {
	// Seed an MCP, then update its timeout via --timeout; the new value wins.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let add = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--url",
			"http://h",
			"--timeout",
			"10",
		])
		.output()
		.unwrap();
	assert!(
		add.status.success(),
		"seed add must succeed; stderr: {}",
		String::from_utf8_lossy(&add.stderr)
	);

	let out = isolated_cli(home.path(), state.path())
		.args([
			"--json",
			"-a",
			"claude",
			"update",
			"mcps",
			"m",
			"--timeout",
			"45",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"update --timeout must succeed; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(
		v["transport"]["timeout"], 45,
		"updated transport must carry timeout:45, got: {v}"
	);
}

#[cfg(unix)] // Windows: global MCP config not HOME-isolated
#[test]
fn update_mcp_zero_timeout_is_rejected() {
	// The update path has its own timeout code (effective_timeout +
	// set_transport_timeout). --timeout 0 alone (no --command/--url) still
	// routes through parse_mcp_transport -> from_inputs, which rejects a zero
	// timeout BEFORE returning Ok(None). Pin that so the rejection can't
	// silently regress if the update path stops calling from_inputs.
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let add = isolated_cli(home.path(), state.path())
		.args([
			"-a",
			"claude",
			"add",
			"mcps",
			"--name",
			"m",
			"--url",
			"http://h",
			"--timeout",
			"10",
		])
		.output()
		.unwrap();
	assert!(
		add.status.success(),
		"seed add must succeed; stderr: {}",
		String::from_utf8_lossy(&add.stderr)
	);

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "update", "mcps", "m", "--timeout", "0"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "update --timeout 0 must be rejected");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("timeout must be greater than 0"),
		"stderr must explain the rejection, got: {stderr}"
	);
}

// ============ Task 10: CLI emits core RemovalView/SkillView builders ============

/// CONTRACT (Task 10): CLI `delete skills` now serializes the shared
/// `aghub_core::dto::RemovalView`, so the JSON keys are snake_case
/// (`dry_run`/`needs_confirm`) matching the API + desktop, NOT the old camelCase
/// `dryRun`/`needsConfirm`. This pins the breaking key flip.
#[cfg(unix)]
#[test]
fn delete_skill_dry_run_outputs_snake_case_keys() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "snaketool");

	let out = isolated_cli(home.path(), state.path())
		.args(["--json", "-a", "claude", "delete", "skills", "snaketool"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	// New snake_case keys from RemovalView.
	assert_eq!(json["dry_run"], true, "json: {json}");
	assert_eq!(json["executed"], false, "json: {json}");
	assert_eq!(json["needs_confirm"], false, "json: {json}");
	// Old camelCase keys must be gone.
	assert!(
		json.get("dryRun").is_none(),
		"dryRun must be removed: {json}"
	);
	assert!(
		json.get("needsConfirm").is_none(),
		"needsConfirm must be removed: {json}"
	);
	// CLI-only envelope keys are still present.
	assert_eq!(json["type"], "skill");
	assert_eq!(json["name"], "snaketool");
	let paths = json["paths"].as_array().unwrap();
	assert!(
		paths
			.iter()
			.any(|p| p.as_str().unwrap().ends_with("snaketool")),
		"paths: {paths:?}"
	);
	assert!(skill_dir.exists(), "dry-run must not delete");
}

/// CLI `add skill` output is now a `SkillView`, which carries the
/// `native_reader` advisory field. For Claude at global scope the agent is NOT a
/// NativeReader, so the field is present and `false`.
#[cfg(unix)]
#[test]
fn add_skill_output_includes_native_reader_field() {
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path();
	std::fs::create_dir_all(project.join(".claude")).unwrap();

	let out = assert_cmd::Command::cargo_bin("aghub-cli")
		.unwrap()
		.env("HOME", project)
		.env("USERPROFILE", project)
		.env("APPDATA", project)
		.current_dir(project)
		.args([
			"--json",
			"-a",
			"claude",
			"add",
			"skill",
			"--name",
			"nrtool",
			"--description",
			"d",
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["name"], "nrtool");
	assert_eq!(
		json["native_reader"], false,
		"claude global add is not a NativeReader: {json}"
	);
	// SkillView shape: source_path key present, raw-Skill `content` absent.
	assert!(json.get("source_path").is_some(), "json: {json}");
	assert!(
		json.get("content").is_none(),
		"content not on SkillView: {json}"
	);
}

/// The other branch of the `native_reader` advisory: OpenCode at global scope
/// reads the `~/.agents/skills` master directly (a NativeReader), so the
/// `add skill` SkillView reports `native_reader: true`.
#[cfg(unix)]
#[test]
fn add_skill_output_native_reader_true_for_opencode() {
	let tmp = tempfile::tempdir().unwrap();
	let home = tmp.path();
	std::fs::create_dir_all(home.join(".config/opencode")).unwrap();

	let out = assert_cmd::Command::cargo_bin("aghub-cli")
		.unwrap()
		.env("HOME", home)
		.env("USERPROFILE", home)
		.env("APPDATA", home)
		.current_dir(home)
		.args([
			"--json",
			"-a",
			"opencode",
			"add",
			"skill",
			"--name",
			"ocnrtool",
			"--description",
			"d",
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(
		json["native_reader"], true,
		"opencode global is a NativeReader: {json}"
	);
}

// ==================== #6: inference inventory CRUD ====================
//
// The inference store roots at `dirs::data_dir()/aghub` in production, but the
// CLI honors `$AGHUB_DATA_DIR` to override that — so these tests pin the root to
// a throwaway tempdir DIRECTLY (no per-platform XDG_DATA_HOME vs
// ~/Library/Application Support guessing). API keys live in the OS keyring
// (unavailable in headless CI), so the CLI also honors
// `$AGHUB_TEST_CREDENTIAL_FILE` to use a plaintext file-backed credential store
// instead. Together this makes the inference CLI fully exercisable cross-platform
// with NO keyring and NO skip-on-failure escape hatches.

/// The credential file every `inference_cli` for one tempdir shares — the
/// headless replacement for a shared keyring namespace.
fn cred_file(data: &std::path::Path) -> std::path::PathBuf {
	data.join("creds.json")
}

/// An aghub-cli command with an ISOLATED data dir + file-backed credential store
/// so the inference SQLite db and API keys are throwaway and keyring-free.
fn inference_cli(data: &std::path::Path) -> Command {
	let mut cmd = Command::cargo_bin("aghub-cli").unwrap();
	cmd.env("AGHUB_DATA_DIR", data);
	cmd.env("AGHUB_TEST_CREDENTIAL_FILE", cred_file(data));
	cmd.env("HOME", data);
	cmd.env("USERPROFILE", data);
	cmd.current_dir(data);
	cmd
}

/// Add a provider and return its id. Fails the test on any error — no keyring
/// escape hatch, because the file-backed credential store always works.
fn add_provider(data: &std::path::Path, latin: &str) -> String {
	let out = inference_cli(data)
		.args([
			"inference",
			"add",
			"--latin-name",
			latin,
			"--display-name",
			"Disp",
			"--format",
			"anthropic",
			"--api-base-url",
			"https://api.example.com",
			"--api-key",
			"sk-test-secret-value",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"inference add failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["latin_name"], latin, "add --json echoes latin_name");
	json["id"].as_str().expect("add --json carries id").into()
}

#[test]
fn inference_add_then_list_shows_provider() {
	let data = tempfile::tempdir().unwrap();
	let _id = add_provider(data.path(), "acme");

	// Table output lists it.
	let table = inference_cli(data.path())
		.args(["inference", "list"])
		.output()
		.unwrap();
	assert!(
		table.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&table.stderr)
	);
	let text = String::from_utf8_lossy(&table.stdout);
	assert!(text.contains("acme"), "table must list acme: {text}");

	// JSON output parses as an array containing acme.
	let out = inference_cli(data.path())
		.args(["inference", "list", "--json"])
		.output()
		.unwrap();
	assert!(out.status.success());
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let arr = json.as_array().expect("list --json is an array");
	assert!(
		arr.iter().any(|p| p["latin_name"] == "acme"),
		"list --json must contain acme: {json}"
	);
}

#[test]
fn inference_get_returns_provider() {
	let data = tempfile::tempdir().unwrap();
	let id = add_provider(data.path(), "acme");

	let out = inference_cli(data.path())
		.args(["inference", "get", &id, "--json"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["id"], id);
	assert_eq!(json["latin_name"], "acme");
	// The masked key is preview-only; the raw secret must never appear.
	assert!(
		!String::from_utf8_lossy(&out.stdout).contains("sk-test-secret-value"),
		"raw api key must never be printed"
	);
}

#[test]
fn inference_update_changes_display_name() {
	let data = tempfile::tempdir().unwrap();
	let id = add_provider(data.path(), "acme");

	let out = inference_cli(data.path())
		.args([
			"inference",
			"update",
			&id,
			"--display-name",
			"Renamed",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["display_name"], "Renamed");
}

#[test]
fn inference_delete_yes_removes_then_list_empty() {
	let data = tempfile::tempdir().unwrap();
	let id = add_provider(data.path(), "acme");

	let del = inference_cli(data.path())
		.args(["inference", "delete", &id, "--yes", "--json"])
		.output()
		.unwrap();
	assert!(
		del.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&del.stderr)
	);

	let out = inference_cli(data.path())
		.args(["inference", "list", "--json"])
		.output()
		.unwrap();
	assert!(out.status.success());
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(
		json.as_array().map(Vec::len),
		Some(0),
		"delete --yes leaves the inventory empty: {json}"
	);
}

#[test]
fn inference_delete_cascades_agent_bindings() {
	// Finding #1 regression: CLI delete must run the same agent-reference
	// cleanup the API delete route does. We seed a Claude binding row pointing
	// at the provider (the binding table the API/desktop write), delete the
	// provider via the CLI, then prove the dangling binding is gone — not left
	// pointing at a removed provider. The CLI roots its store at
	// `$AGHUB_DATA_DIR` (= `data.path()`, set by `inference_cli`), so we open the
	// same SQLite db directly here. Binding rows live in SQLite, not the keyring,
	// so the credential backend is irrelevant for this assertion.
	use aghub_inference::InferenceProviderStore;

	let data = tempfile::tempdir().unwrap();
	let id = add_provider(data.path(), "acme");

	let store = InferenceProviderStore::new(data.path());
	store
		.create_agent_binding("claude", &id, None)
		.expect("seed claude binding");
	assert_eq!(
		store.list_agent_bindings("claude").unwrap().len(),
		1,
		"precondition: one claude binding references the provider"
	);

	let del = inference_cli(data.path())
		.args(["inference", "delete", &id, "--yes", "--json"])
		.output()
		.unwrap();
	assert!(
		del.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&del.stderr)
	);

	assert!(
		store.list_agent_bindings("claude").unwrap().is_empty(),
		"CLI delete must remove the agent binding, not leave it dangling"
	);
}

#[test]
fn inference_delete_without_yes_is_rejected() {
	let data = tempfile::tempdir().unwrap();
	let id = add_provider(data.path(), "acme");

	let del = inference_cli(data.path())
		.args(["inference", "delete", &id])
		.output()
		.unwrap();
	assert!(
		!del.status.success(),
		"delete without --yes must be rejected"
	);
	let stderr = String::from_utf8_lossy(&del.stderr);
	assert!(
		stderr.contains("--yes"),
		"error must point at the --yes guard: {stderr}"
	);

	// The non-executed branch must leave the provider intact.
	let out = inference_cli(data.path())
		.args(["inference", "list", "--json"])
		.output()
		.unwrap();
	assert!(out.status.success());
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert!(
		json.as_array().unwrap().iter().any(|p| p["id"] == id),
		"rejected delete must NOT remove the provider: {json}"
	);
}

#[test]
fn inference_add_missing_api_key_errors_clearly() {
	// No --api-key, no stdin, no env => a clear error before any keyring touch.
	// This is metadata-stable across platforms (no keyring involved).
	let data = tempfile::tempdir().unwrap();
	let out = inference_cli(data.path())
		.env_remove("AGHUB_INFERENCE_API_KEY")
		.args([
			"inference",
			"add",
			"--latin-name",
			"acme",
			"--display-name",
			"Disp",
			"--format",
			"anthropic",
			"--api-base-url",
			"https://api.example.com",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "missing api key must fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("api key") || stderr.contains("api-key"),
		"error must name the missing api key: {stderr}"
	);
}

#[test]
fn inference_add_invalid_format_errors() {
	// An unknown --format value is rejected by InferenceProviderFormat::FromStr
	// before any store/keyring work; platform-stable.
	let data = tempfile::tempdir().unwrap();
	let out = inference_cli(data.path())
		.args([
			"inference",
			"add",
			"--latin-name",
			"acme",
			"--display-name",
			"Disp",
			"--format",
			"not-a-format",
			"--api-base-url",
			"https://api.example.com",
			"--api-key",
			"sk-test",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "invalid format must fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("format"),
		"error must name the bad format: {stderr}"
	);
}

#[test]
fn inference_get_missing_id_errors() {
	// A get for an id that was never created surfaces the store NotFound error.
	let data = tempfile::tempdir().unwrap();
	let out = inference_cli(data.path())
		.args(["inference", "get", "does-not-exist", "--json"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "get on a missing id must fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("not found") || stderr.contains("does-not-exist"),
		"error must surface the missing id: {stderr}"
	);
}

#[test]
fn inference_add_api_key_from_env() {
	// The api key resolves from AGHUB_INFERENCE_API_KEY when --api-key is absent.
	let data = tempfile::tempdir().unwrap();
	let out = inference_cli(data.path())
		.env("AGHUB_INFERENCE_API_KEY", "sk-from-env-secret")
		.args([
			"inference",
			"add",
			"--latin-name",
			"acme",
			"--display-name",
			"Disp",
			"--format",
			"anthropic",
			"--api-base-url",
			"https://api.example.com",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["latin_name"], "acme");
	// The raw env secret must never be echoed back.
	assert!(
		!String::from_utf8_lossy(&out.stdout).contains("sk-from-env-secret"),
		"raw env api key must never be printed"
	);
}

#[test]
fn inference_add_api_key_from_stdin() {
	// `--api-key -` reads the key from stdin.
	//
	// BEHAVIOUR CHANGE: stdin used to be read whenever it was not a tty, with
	// no flag at all. That blocked to EOF on the open, idle pipe a
	// non-interactive harness leaves on stdin — an infinite hang with no
	// prompt, no output and no diagnostic. `-` makes the read explicit (the
	// curl/gpg convention) and keeps the key off argv.
	let data = tempfile::tempdir().unwrap();
	let out = inference_cli(data.path())
		.env_remove("AGHUB_INFERENCE_API_KEY")
		.args([
			"inference",
			"add",
			"--latin-name",
			"acme",
			"--display-name",
			"Disp",
			"--format",
			"anthropic",
			"--api-base-url",
			"https://api.example.com",
			"--api-key",
			"-",
			"--json",
		])
		.write_stdin("sk-from-stdin-secret\n")
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["latin_name"], "acme");
	assert!(
		!String::from_utf8_lossy(&out.stdout).contains("sk-from-stdin-secret"),
		"raw stdin api key must never be printed"
	);
}

#[test]
fn inference_key_reports_presence_without_leaking() {
	// The `key` subcommand prints the masked preview + stored=true, never the
	// raw secret.
	let data = tempfile::tempdir().unwrap();
	let id = add_provider(data.path(), "acme");

	let out = inference_cli(data.path())
		.args(["inference", "key", &id])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout.contains("stored=true"),
		"key must report presence: {stdout}"
	);
	assert!(
		!stdout.contains("sk-test-secret-value"),
		"key must never print the raw secret: {stdout}"
	);
}

#[test]
fn inference_update_each_branch_applies() {
	// Exercises the update arms beyond display-name: format, base URL, preset,
	// api-key replacement, and the model list.
	let data = tempfile::tempdir().unwrap();
	let id = add_provider(data.path(), "acme");

	let out = inference_cli(data.path())
		.args([
			"inference",
			"update",
			&id,
			"--format",
			"openai-responses",
			"--api-base-url",
			"https://updated.example.com/v1",
			"--preset",
			"my-preset",
			"--api-key",
			"sk-rotated-secret",
			"--model",
			"gpt-x",
			"--model",
			"gpt-y",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["api_base_url"], "https://updated.example.com/v1");
	assert_eq!(json["preset"], "my-preset");
	assert_eq!(json["format"], "openai_responses");
	let models: Vec<&str> = json["models"]
		.as_array()
		.unwrap()
		.iter()
		.map(|m| m.as_str().unwrap())
		.collect();
	assert_eq!(
		models,
		vec!["gpt-x", "gpt-y"],
		"model list must be replaced"
	);
	// The rotated raw key must never be echoed back.
	assert!(
		!String::from_utf8_lossy(&out.stdout).contains("sk-rotated-secret"),
		"rotated api key must never be printed"
	);
}

// ==================== Task 30: transfer + reconcile ====================
//
// Thin CLI adapters over `aghub_core::transfer`. The core fns are already tested
// inline in transfer.rs; these e2e tests pin only the CLI wiring: scope/agent
// arg parsing, the OperationBatchResult rendering, the project-root requirement,
// and the non-zero exit on a failed batch. They build an isolated project (a
// `.claude/` agent marker + a project skill) and run from that cwd so `-p`
// resolves the temp project, never the user's real tree.

/// Build an isolated temp project containing a Claude project skill named
/// `skill`. The `.claude/` dir is itself an agent marker, so `find_project_root`
/// detects the project from this cwd. Returns the project root TempDir.
fn transfer_project(skill: &str) -> tempfile::TempDir {
	let project = tempfile::TempDir::new().unwrap();
	let dir = project.path().join(".claude/skills").join(skill);
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!("---\nname: {skill}\ndescription: d\n---\nbody\n"),
	)
	.unwrap();
	project
}

/// An aghub-cli command rooted at `project` (cwd + redirected HOME) so transfer
/// scope resolution and any global lookups stay inside the throwaway dir.
fn transfer_cli(project: &std::path::Path) -> Command {
	let mut cmd = Command::cargo_bin("aghub-cli").unwrap();
	cmd.env("HOME", project);
	cmd.env("USERPROFILE", project);
	cmd.env("APPDATA", project);
	cmd.current_dir(project);
	cmd
}

#[test]
fn transfer_skill_copies_claude_to_opencode_project() {
	let project = transfer_project("repo-helper");

	let out = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--to",
			"opencode",
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	assert!(
		project
			.path()
			.join(".agents/skills/repo-helper/SKILL.md")
			.exists(),
		"OpenCode is a NativeReader and must use the shared Master",
	);
}

#[test]
fn transfer_skill_json_reports_success_for_target() {
	let project = transfer_project("repo-helper");

	let out = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--to",
			"opencode",
			"--json",
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let row = json["results"]
		.as_array()
		.and_then(|a| a.iter().find(|r| r["agent"] == "opencode"))
		.expect("opencode row present");
	assert_eq!(row["success"], true);
	assert_eq!(row["action"], "copy");
}

#[test]
fn transfer_json_shape_matches_api_dto() {
	// Finding #3: CLI and API must emit ONE shape. The API's
	// OperationBatchResponse is `{success_count, failed_count, results:[{agent,
	// scope, project_root, action, success, error}]}`. The CLI --json must
	// match it field-for-field (incl. the batch wrapper + per-row scope), not a
	// bare array of {agent, action, success, error}.
	let project = transfer_project("repo-helper");

	let out = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--to",
			"opencode",
			"--json",
		])
		.output()
		.unwrap();
	assert!(out.status.success());

	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert!(
		json.get("success_count").and_then(Value::as_u64).is_some(),
		"batch wrapper must carry success_count: {json}"
	);
	assert!(
		json.get("failed_count").and_then(Value::as_u64).is_some(),
		"batch wrapper must carry failed_count: {json}"
	);
	let row = json["results"]
		.as_array()
		.and_then(|a| a.iter().find(|r| r["agent"] == "opencode"))
		.expect("results array with opencode row");
	assert_eq!(row["scope"], "project", "per-row scope like the API DTO");
	assert_eq!(row["action"], "copy");
	assert_eq!(row["success"], true);
}

/// A repeat `transfer` of the same skill into the same target is an idempotent
/// SUCCESS that says so, not a conflict.
///
/// This asserted `!status.success()` and `row["success"] == false` until
/// `transfer_skill`'s own `get_skill(..).is_some()` guard was removed:
/// `reconcile --add` accepted exactly the state `transfer --to` refused, so the
/// same operation had opposite verdicts depending on which verb you reached
/// for. The refusal also had no remedy in it — it named neither
/// `reconcile --add` nor the fact that nothing was wrong.
///
/// Two INDEPENDENT reverts each turn this red, so it is not a false green:
///   1. restore the guard in `core/src/transfer.rs` (the
///      `manager.get_skill(&skill.name).is_some()` early return) → the row goes
///      `success:false` and the process exits non-zero;
///   2. hard-code `already_present: false` in `operation_batch`'s row mapping →
///      exit code and `success` stay right, and the `already_present`
///      assertion alone goes red.
#[cfg(unix)]
#[test]
fn transfer_skill_second_run_is_idempotent_and_says_so() {
	let project = transfer_project("repo-helper");

	let first = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--to",
			"opencode",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		first.status.success(),
		"first transfer must succeed: {}",
		String::from_utf8_lossy(&first.stderr)
	);
	let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
	let first_row = first_json["results"]
		.as_array()
		.and_then(|a| a.iter().find(|r| r["agent"] == "opencode"))
		.expect("opencode row present");
	assert_eq!(
		first_row["already_present"], false,
		"a REAL copy must not claim it was already there: {first_row}"
	);

	// Second run: same source, same target, nothing left to do.
	let out = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--to",
			"opencode",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"an idempotent re-transfer must exit 0; stdout: {} stderr: {}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["failed_count"], 0, "{json}");
	let row = json["results"]
		.as_array()
		.and_then(|a| a.iter().find(|r| r["agent"] == "opencode"))
		.expect("opencode row present");
	assert_eq!(row["success"], true, "{row}");
	assert_eq!(row["ok"], true, "{row}");
	assert_eq!(
		row["already_present"], true,
		"the row must say nothing was written, or a caller cannot tell an \
		 idempotent no-op from a real copy: {row}"
	);
	assert!(row["error"].is_null(), "{row}");
}

/// A Master-reading agent's FIRST transfer must succeed — this is the case the
/// old guard actually broke, and no test covered it.
///
/// cursor, cline, codex, opencode and warp read `.agents/skills` directly. Once
/// ANY agent in the scope has installed a skill, the Master exists, so those
/// agents already "hold" it — and `transfer_skill`'s `get_skill(..).is_some()`
/// guard fired on their very first transfer. The refusal said "Resource already
/// exists", which reads as a conflict when the truth is the opposite: the
/// target can already see it, and there is nothing to do.
#[cfg(unix)]
#[test]
fn transfer_skill_to_master_reading_agent_first_use_is_idempotent() {
	let project = transfer_project("repo-helper");

	// Materialize the shared Master via a transfer to one agent.
	let seed = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--to",
			"opencode",
		])
		.output()
		.unwrap();
	assert!(
		seed.status.success(),
		"seed transfer must succeed: {}",
		String::from_utf8_lossy(&seed.stderr)
	);
	let master = project.path().join(".agents/skills/repo-helper/SKILL.md");
	let before = std::fs::read_to_string(&master)
		.expect("the seed transfer must have written the Master");

	// cursor has never been transferred to — but it reads that same Master.
	let out = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--to",
			"cursor",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"a Master-reading agent's first transfer must not fail: {} {}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["failed_count"], 0, "{json}");
	let row = json["results"]
		.as_array()
		.and_then(|a| a.iter().find(|r| r["agent"] == "cursor"))
		.expect("cursor row present");
	assert_eq!(row["success"], true, "{row}");
	assert_eq!(row["already_present"], true, "{row}");
	assert!(row["error"].is_null(), "{row}");

	assert_eq!(
		std::fs::read_to_string(&master).unwrap(),
		before,
		"an already-present transfer must rewrite nothing"
	);
}

#[test]
fn transfer_skill_empty_to_is_rejected() {
	// Finding #4: `transfer skill ... --json` with no --to must NOT print `[]`
	// and exit 0 — an empty destination list is an actionable error.
	let project = transfer_project("repo-helper");

	let out = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--json",
		])
		.output()
		.unwrap();

	assert!(
		!out.status.success(),
		"transfer with no --to must fail; stdout: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	// clap fails fast at parse (`--to` is `required`); the usage error names the
	// missing destination flag so the user knows what to add.
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("--to")
			|| stderr.contains("destination")
			|| stderr.contains("target"),
		"error must name the missing destination: {stderr}"
	);
}

#[test]
fn transfer_skill_project_without_root_errors() {
	// `-p` with no project marker anywhere up-tree: scope is Project but there is
	// no project_root. The Project-scoped source skill is then unresolvable, so
	// transfer fails before any copy — proving the no-root path is rejected, not
	// silently treated as global. (A destination-only missing root surfaces
	// `validate_target`'s "project root is required"; the CLI shares one scope
	// across source+targets, so the source-load failure fires first.)
	let empty = tempfile::TempDir::new().unwrap();

	let out = transfer_cli(empty.path())
		.args([
			"-p",
			"transfer",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--to",
			"opencode",
		])
		.output()
		.unwrap();

	assert!(
		!out.status.success(),
		"project transfer without a project root must fail"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("not found") || stderr.contains("project root"),
		"error must surface the no-root failure: {stderr}"
	);
}

#[test]
fn reconcile_skill_reports_batch_summary() {
	let project = transfer_project("repo-helper");

	let out = transfer_cli(project.path())
		.args([
			"-p",
			"reconcile",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"repo-helper",
			"--add",
			"opencode",
			"--json",
		])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let results = json["results"]
		.as_array()
		.expect("reconcile --json has results");
	assert!(
		results
			.iter()
			.any(|r| r["agent"] == "opencode" && r["success"] == true),
		"reconcile --add opencode must copy into OpenCode: {json}"
	);
	assert!(
		project
			.path()
			.join(".agents/skills/repo-helper/SKILL.md")
			.exists(),
		"reconcile --add must retain the Master OpenCode reads natively"
	);
}

// ── MCP + sub-agent transfer/reconcile arms (finding #2 coverage) ────────────
//
// The skill arm above is the tracer; these pin the OTHER two resource kinds the
// `transfer`/`reconcile` dispatch added, so a kind getting dropped from the
// match (or wired to the wrong core fn) is caught.

/// Seed a Claude project MCP via the CLI itself, then return the project dir.
fn mcp_project() -> tempfile::TempDir {
	let project = transfer_project("placeholder-skill");
	let add = transfer_cli(project.path())
		.args([
			"-p",
			"-a",
			"claude",
			"add",
			"mcp",
			"--name",
			"filesystem",
			"--command",
			"npx mcp-filesystem",
		])
		.output()
		.unwrap();
	assert!(
		add.status.success(),
		"seed add mcp failed: {}",
		String::from_utf8_lossy(&add.stderr)
	);
	project
}

#[test]
fn transfer_mcp_copies_claude_to_cursor_project() {
	let project = mcp_project();

	let out = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"mcp",
			"--from-agent",
			"claude",
			"--name",
			"filesystem",
			"--to",
			"cursor",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let row = json["results"]
		.as_array()
		.and_then(|a| a.iter().find(|r| r["agent"] == "cursor"))
		.expect("cursor row present");
	assert_eq!(row["success"], true, "mcp transfer must succeed: {json}");
	assert_eq!(row["action"], "copy");
}

#[test]
fn reconcile_mcp_add_then_remove() {
	let project = mcp_project();

	// --add cursor copies the MCP in.
	let add = transfer_cli(project.path())
		.args([
			"-p",
			"reconcile",
			"mcp",
			"--from-agent",
			"claude",
			"--name",
			"filesystem",
			"--add",
			"cursor",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		add.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&add.stderr)
	);
	let json: Value = serde_json::from_slice(&add.stdout).unwrap();
	assert!(
		json["results"]
			.as_array()
			.unwrap()
			.iter()
			.any(|r| r["agent"] == "cursor"
				&& r["action"] == "copy"
				&& r["success"] == true),
		"reconcile --add must copy into cursor: {json}"
	);

	// --remove without --yes is a dry-run: reports the plan, changes nothing.
	let dry = transfer_cli(project.path())
		.args([
			"-p",
			"reconcile",
			"mcp",
			"--from-agent",
			"claude",
			"--name",
			"filesystem",
			"--remove",
			"cursor",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		dry.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&dry.stderr)
	);
	let json: Value = serde_json::from_slice(&dry.stdout).unwrap();
	assert_eq!(json["dry_run"], true, "no --yes must dry-run: {json}");
	assert_eq!(json["remove"][0], "cursor");

	// --remove --yes deletes it back out (Delete action).
	let rm = transfer_cli(project.path())
		.args([
			"-p",
			"reconcile",
			"mcp",
			"--from-agent",
			"claude",
			"--name",
			"filesystem",
			"--remove",
			"cursor",
			"--yes",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		rm.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&rm.stderr)
	);
	let json: Value = serde_json::from_slice(&rm.stdout).unwrap();
	assert!(
		json["results"]
			.as_array()
			.unwrap()
			.iter()
			.any(|r| r["agent"] == "cursor" && r["action"] == "delete"),
		"reconcile --remove must delete from cursor: {json}"
	);
}

/// Seed a Claude project sub-agent file, then return the project dir.
fn sub_agent_project(name: &str) -> tempfile::TempDir {
	let project = transfer_project("placeholder-skill");
	let dir = project.path().join(".claude/agents");
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(
		dir.join(format!("{name}.md")),
		format!("---\nname: {name}\ndescription: d\n---\nYou are a {name}.\n"),
	)
	.unwrap();
	project
}

#[test]
fn transfer_sub_agent_copies_claude_to_opencode_project() {
	let project = sub_agent_project("coder");

	let out = transfer_cli(project.path())
		.args([
			"-p",
			"transfer",
			"sub-agent",
			"--from-agent",
			"claude",
			"--name",
			"coder",
			"--to",
			"opencode",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let row = json["results"]
		.as_array()
		.and_then(|a| a.iter().find(|r| r["agent"] == "opencode"))
		.expect("opencode row present");
	assert_eq!(
		row["success"], true,
		"sub-agent transfer must succeed: {json}"
	);
}

#[test]
fn reconcile_sub_agent_add_reports_copy() {
	let project = sub_agent_project("coder");

	let out = transfer_cli(project.path())
		.args([
			"-p",
			"reconcile",
			"sub-agent",
			"--from-agent",
			"claude",
			"--name",
			"coder",
			"--add",
			"opencode",
			"--json",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert!(
		json["results"]
			.as_array()
			.unwrap()
			.iter()
			.any(|r| r["agent"] == "opencode" && r["success"] == true),
		"reconcile --add opencode must copy the sub-agent: {json}"
	);
}

// ── `coverage` — read-only classify_all projection ──────────────────────────

/// An aghub-cli command with HOME redirected to a throwaway dir so the global
/// coverage classify reads `<tmp>/.agents/skills` (and per-agent global dirs)
/// instead of the developer's real home.
fn coverage_cli(home: &std::path::Path) -> Command {
	let mut cmd = Command::cargo_bin("aghub-cli").unwrap();
	cmd.env("HOME", home);
	cmd.env("USERPROFILE", home);
	cmd.env("APPDATA", home);
	cmd.current_dir(home);
	cmd
}

#[test]
fn coverage_json_uses_id_key_like_api_dto() {
	// Finding #3: the API's AgentSkillCoverageDto keys the agent as `id`. The
	// CLI must use the same key, not `agent`, so the two surfaces agree.
	let home = tempfile::TempDir::new().unwrap();

	let out = coverage_cli(home.path())
		.args(["-g", "coverage", "--json"])
		.output()
		.unwrap();
	assert!(out.status.success());

	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let row = &json.as_array().expect("coverage --json is an array")[0];
	assert!(
		row.get("id").and_then(Value::as_str).is_some(),
		"coverage row must key the agent as `id` (API DTO shape): {row}"
	);
	assert!(
		row.get("agent").is_none(),
		"coverage row must NOT carry the drifted `agent` key: {row}"
	);
}

#[test]
fn coverage_global_json_codex_native_claude_needs_link() {
	// Mirrors api `global_scope_buckets_codex_native_claude_needs_link`: codex
	// @global reads `~/.agents/skills` (the master) so it is auto_covered with no
	// link; claude @global has a private `~/.claude/skills` so it NeedsLink.
	let home = tempfile::TempDir::new().unwrap();

	let out = coverage_cli(home.path())
		.args(["-g", "coverage", "--json"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let arr = json.as_array().expect("coverage --json is an array");

	let codex = arr
		.iter()
		.find(|r| r["id"] == "codex")
		.expect("codex row present");
	assert_eq!(codex["scope"], "global");
	assert_eq!(
		codex["auto_covered"], true,
		"codex @global reads .agents/skills: {codex}"
	);
	assert_eq!(codex["needs_link"], false);
	assert_eq!(codex["supported"], true);
	assert_eq!(codex["reads_master"], true);

	let claude = arr
		.iter()
		.find(|r| r["id"] == "claude")
		.expect("claude row present");
	assert_eq!(
		claude["needs_link"], true,
		"claude @global has a private skills dir => NeedsLink: {claude}"
	);
	assert_eq!(claude["auto_covered"], false);
	assert_eq!(claude["supported"], true);
}

#[test]
fn coverage_project_json_classifies_against_project_master() {
	// Project scope with a real project_root: opencode reads `<root>/.agents/
	// skills` at project scope (auto_covered), claude still needs a link. The
	// `.claude/` dir makes `find_project_root` detect the project from cwd.
	let project = transfer_project("anything");

	let out = coverage_cli(project.path())
		.args(["-p", "coverage", "--json"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let arr = json.as_array().expect("coverage --json is an array");

	let opencode = arr
		.iter()
		.find(|r| r["id"] == "opencode")
		.expect("opencode row present");
	assert_eq!(opencode["scope"], "project");
	assert_eq!(
		opencode["auto_covered"], true,
		"opencode @project reads <root>/.agents/skills: {opencode}"
	);
	let claude = arr
		.iter()
		.find(|r| r["id"] == "claude")
		.expect("claude row present");
	assert_eq!(
		claude["needs_link"], true,
		"claude @project has a private skills dir => NeedsLink: {claude}"
	);
}

#[test]
fn coverage_project_without_root_errors() {
	// `-p` with no agent marker up-tree: scope is Project but there is no
	// project_root, so the master skills-dir is unresolvable and coverage must
	// fail with a clear message rather than silently classifying global.
	let empty = tempfile::TempDir::new().unwrap();

	let out = coverage_cli(empty.path())
		.args(["-p", "coverage", "--json"])
		.output()
		.unwrap();

	assert!(
		!out.status.success(),
		"project coverage without a project root must fail; stdout: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("project root"),
		"error must surface the missing project root: {stderr}"
	);
}

#[test]
fn coverage_rejects_all_scope() {
	// Coverage only supports global|project (mirrors api `coverage_rejects_
	// scope_all`); `--all` (ResourceScope::Both) must be rejected with a clear
	// error, not silently coerced.
	let home = tempfile::TempDir::new().unwrap();

	let out = coverage_cli(home.path())
		.args(["--all", "coverage", "--json"])
		.output()
		.unwrap();

	assert!(
		!out.status.success(),
		"coverage with --all must be rejected; stdout: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("global") && stderr.contains("project"),
		"error must name the supported scopes: {stderr}"
	);
}

#[test]
fn coverage_default_table_lists_agents() {
	// No -g/-p: defaults to global scope and prints a human table (no --json)
	// listing the registered agents with the coverage columns.
	let home = tempfile::TempDir::new().unwrap();

	let out = coverage_cli(home.path())
		.args(["coverage"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout.contains("AGENT") && stdout.contains("AUTO COVERED"),
		"table header must list the coverage columns: {stdout}"
	);
	assert!(
		stdout.contains("claude") && stdout.contains("codex"),
		"table must list registered agents: {stdout}"
	);
}

// ── Docs: '## CLI Command Surface' must enumerate the Phase-7 subcommands ─────

/// Slice the `## CLI Command Surface` section out of the repo-root AGENTS.md.
/// Keeps the assertion scoped to that block so a stray mention elsewhere in the
/// doc can't make the test pass.
fn cli_command_surface_block() -> String {
	let agents_md = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../AGENTS.md")
		.canonicalize()
		.unwrap();
	let text = std::fs::read_to_string(&agents_md).unwrap();
	let start = text
		.find("## CLI Command Surface")
		.expect("AGENTS.md must have a '## CLI Command Surface' section");
	let rest = &text[start..];
	// Section ends at the next top-level heading.
	let end = rest[1..].find("\n## ").map(|i| i + 1).unwrap_or(rest.len());
	rest[..end].to_string()
}

#[test]
fn agents_md_command_surface_lists_phase7_subcommands() {
	// Every Phase-7 subcommand dispatched in main.rs must be enumerated in the
	// CLI command-surface block so the doc stays in sync with the binary.
	let block = cli_command_surface_block();
	for cmd in ["inference", "transfer", "reconcile", "coverage"] {
		assert!(
			block.contains(cmd),
			"'## CLI Command Surface' must document the `{cmd}` subcommand; \
			 block:\n{block}"
		);
	}
}

/// `-p` selects `ResourceScope::ProjectOnly`. With no project root found
/// from cwd, every generic SingleWrite mutation (add/update/delete/enable/
/// disable) must fail before touching any config instead of silently
/// falling back to a write against the global Master.
#[cfg(unix)]
#[test]
fn project_scope_mutation_without_project_root_fails_before_writing() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let not_a_project = tempfile::TempDir::new().unwrap();

	// `find_project_root` walks the REAL ancestor chain of `not_a_project`,
	// not a sandboxed one. If TMPDIR happens to sit under a directory that
	// carries a project marker (e.g. `$HOME/tmp` on a box whose `$HOME` has
	// `.claude/`), the walk resolves a root, the guard below never fires,
	// and the mutation runs for real against the isolated-but-real-enough
	// HOME. Skip rather than let that happen; the point of this test is
	// that the guard fires, not that we can force TMPDIR's ancestry.
	if aghub_core::paths::find_project_root(not_a_project.path()).is_some() {
		eprintln!(
			"skipping project_scope_mutation_without_project_root_fails_before_writing: \
			 {} resolves to a project root via its real ancestor chain \
			 (TMPDIR likely sits under a directory with a project marker)",
			not_a_project.path().display()
		);
		return;
	}

	let cases: &[&[&str]] = &[
		&["-p", "add", "skills", "--name", "leaked"],
		&[
			"-p",
			"update",
			"skills",
			"leaked",
			"--description",
			"changed",
		],
		&["-p", "delete", "skills", "leaked"],
		&["-p", "enable", "mcps", "leaked"],
		&["-p", "disable", "mcps", "leaked"],
	];

	for args in cases {
		let out = isolated_cli(home.path(), state.path())
			.current_dir(not_a_project.path())
			.args(*args)
			.output()
			.unwrap();

		let stderr = String::from_utf8_lossy(&out.stderr);
		assert!(
			!out.status.success(),
			"{args:?} with no project root must fail: {stderr}"
		);
		// Assert the guard itself is what failed the command, so an
		// unrelated failure (e.g. a bad flag) can't make this pass for the
		// wrong reason.
		assert!(
			stderr.contains("no project root found"),
			"{args:?} must fail via the project-root guard: {stderr}"
		);
	}

	assert!(
		!home.path().join(".agents/skills/leaked").exists(),
		"a rejected -p mutation must not leak the global Master onto disk"
	);
}

/// Fix D: a `--all` (`ResourceScope::Both`) commit prune must scan BOTH scopes
/// before mutating either lock. Chmod the project's Claude skills dir to
/// `000` so `prune_lock_scanning(Project, ..)`'s disk scan fails with a real
/// permission error; before the fix, the GLOBAL lock had already been
/// committed (its orphan entry dropped) by the time that error surfaced, so
/// a failing command still left a silent partial mutation behind.
#[cfg(unix)]
#[test]
fn prune_lock_all_scope_project_scan_failure_leaves_global_lock_untouched() {
	use std::os::unix::fs::PermissionsExt;

	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();

	let lock_path = seed_global_lock(state.path());
	let before = std::fs::read(&lock_path).unwrap();

	// `.claude` is a project marker (so `--all` finds this as the project
	// root), and `.claude/skills` is Claude's project skill dir (so it's
	// part of the set `prune_lock_scanning(Project, ..)` scans).
	let project_skills = project.path().join(".claude/skills");
	std::fs::create_dir_all(&project_skills).unwrap();
	std::fs::set_permissions(
		&project_skills,
		std::fs::Permissions::from_mode(0o000),
	)
	.unwrap();

	// Root bypasses directory permission checks entirely, so the scan below
	// would succeed instead of failing; skip in that case (chmod doesn't
	// block root, matching `perms_enforced` above).
	if std::fs::read_dir(&project_skills).is_ok() {
		std::fs::set_permissions(
			&project_skills,
			std::fs::Permissions::from_mode(0o755),
		)
		.unwrap();
		eprintln!("skip: perms not enforced (root)");
		return;
	}

	let out = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["--all", "prune-lock", "--yes"])
		.output()
		.unwrap();

	// Restore before asserting so a failed assert never leaks the temp dir.
	std::fs::set_permissions(
		&project_skills,
		std::fs::Permissions::from_mode(0o755),
	)
	.unwrap();

	assert!(
		!out.status.success(),
		"a project-side scan error must abort the whole prune: stdout={} \
		 stderr={}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	);

	let after = std::fs::read(&lock_path).unwrap();
	assert_eq!(
		before, after,
		"a project scan failure must leave the GLOBAL lock untouched (no \
		 partial commit)"
	);
}

/// D2: a `--all` (`ResourceScope::Both`) commit prune where the GLOBAL lock
/// commits fine but the PROJECT lock WRITE then fails must not bail with
/// empty stdout. Chmod the PROJECT ROOT itself (not a skills dir) to `000`:
/// the preflight + commit scans still pass (listing/traversing a directory
/// only needs read+execute), but `skills-lock.json` lives directly under the
/// project root, so writing its replacement temp file there fails. Before the
/// fix this failure propagated via `?` with nothing printed, silently hiding
/// the fact that the global lock had already been pruned.
#[cfg(unix)]
#[test]
fn prune_lock_all_scope_project_write_failure_reports_global_prune() {
	use std::os::unix::fs::PermissionsExt;

	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();

	seed_global_lock(state.path());
	// `.claude` marks this dir as a project root for `--all` resolution.
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();
	// Seed a project-lock entry with no matching on-disk skill dir, so the
	// project prune actually finds a removal to write. Without a change,
	// `retain_local_locked_skills` skips the write entirely and the write
	// failure this test targets would never be reached.
	std::fs::write(
		project.path().join("skills-lock.json"),
		r#"{"version":1,"skills":{"porphan":{"source":"o/r","sourceType":"github","computedHash":"deadbeef"}}}"#,
	)
	.unwrap();

	// Root bypasses directory permission checks entirely; skip in that case
	// (chmod doesn't block root, matching `perms_enforced` elsewhere).
	if !perms_enforced(project.path()) {
		eprintln!("skip: perms not enforced (root)");
		return;
	}
	let orig = std::fs::metadata(project.path()).unwrap().permissions();
	std::fs::set_permissions(
		project.path(),
		std::fs::Permissions::from_mode(0o555),
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["--json", "--all", "prune-lock", "--yes"])
		.output()
		.unwrap();

	// Restore before asserting so a failed assert never leaks the temp dir.
	std::fs::set_permissions(project.path(), orig).unwrap();

	assert!(
		!out.status.success(),
		"a project lock WRITE failure must still exit non-zero: stdout={} \
		 stderr={}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	);

	// This is the assertion that fails without the fix: `?` on the project
	// write error propagated immediately, leaving stdout completely empty.
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		!stdout.trim().is_empty(),
		"the already-committed global prune must be reported on stdout, not \
		 swallowed by an early error return"
	);
	let json: Value = serde_json::from_str(&stdout).unwrap();
	assert!(
		json["pruned"]
			.as_array()
			.unwrap()
			.iter()
			.any(|n| n == "orphan"),
		"the global orphan entry pruned before the project write failed \
		 must be reported: {json}"
	);
	assert!(
		json["error"].is_string(),
		"the partial-mutation report must surface an error field: {json}"
	);
}

// ───────────────────────── CLI surface contract (2.11) ─────────────────────
//
// One test per fix in the 2.11 usability pass. Each asserts an OBSERVABLE
// outcome (stdout text, disk content) so reverting the fix turns it red.

/// Scope/agent/json/verbose are `global = true`, so they parse AFTER the
/// subcommand too. Before that, `get skills -a claude` died with clap's
/// "unexpected argument '-a' found" — the single most common trip-up, since
/// every other CLI accepts trailing flags.
#[test]
fn global_flags_parse_after_the_subcommand() {
	let dir = fixtures_dir();
	for args in [
		vec!["get", "skills", "-a", "claude", "--json"],
		vec!["get", "skills", "--json", "-g"],
		vec!["coverage", "--json", "-g"],
	] {
		let out = aghub_cli().current_dir(&dir).args(&args).output().unwrap();
		assert!(
			out.status.success(),
			"{args:?} must parse; stderr: {}",
			String::from_utf8_lossy(&out.stderr)
		);
		serde_json::from_slice::<Value>(&out.stdout)
			.unwrap_or_else(|e| panic!("{args:?} must emit JSON: {e}"));
	}
}

/// The scope guard survived the move to `global = true`: clap does NOT
/// propagate an ArgGroup into subcommands, so the exclusivity check had to
/// become a manual one — and it must fire in the trailing position too, which
/// the old ArgGroup could never have covered.
#[test]
fn scope_flags_stay_exclusive_in_the_trailing_position() {
	let dir = fixtures_dir();
	let out = aghub_cli()
		.current_dir(&dir)
		.args(["get", "skills", "-g", "-p"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "-g -p must still be rejected");
	assert!(
		String::from_utf8_lossy(&out.stderr).contains("mutually exclusive"),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}

/// Default output is human-readable; `--json` is what scripts ask for. `get`
/// used to have no `--json` at all and ALWAYS printed JSON, so a human had no
/// readable option and the flag's meaning differed per command.
#[cfg(unix)]
#[test]
fn get_skills_is_a_table_by_default_and_json_on_demand() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	write_claude_skill(home.path(), "tabled");

	let table = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "get", "skills"])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&table.stdout);
	assert!(table.status.success(), "get skills must succeed");
	assert!(
		text.contains("NAME") && text.contains("tabled"),
		"default output must be a table: {text}"
	);
	assert!(
		serde_json::from_str::<Value>(&text).is_err(),
		"default output must NOT be JSON: {text}"
	);

	let json = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "get", "skills", "--json"])
		.output()
		.unwrap();
	let parsed: Value = serde_json::from_slice(&json.stdout).unwrap();
	assert_eq!(parsed[0]["name"], "tabled");
}

/// A delete preview must SAY it is a preview and how to commit it. It used to
/// print only `{"success": true, "dry_run": true, ...}` — read as "removed".
#[cfg(unix)]
#[test]
fn delete_preview_tells_you_how_to_commit_it() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "previewed");

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "delete", "skills", "previewed"])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&out.stdout);
	assert!(out.status.success());
	assert!(
		text.contains("would remove") && text.contains("--yes"),
		"preview must name itself and say how to commit: {text}"
	);
	assert!(skill_dir.exists(), "preview must not delete");
}

/// A committed delete must disclose the `.agents/skills` Master it leaves
/// behind: `source sync` refuses to overwrite an existing Master, so a user who
/// believes the skill is gone cannot reinstall it from git. The JSON always
/// carried this in `skipped`; the human path printed nothing.
#[cfg(unix)]
#[test]
fn delete_discloses_the_master_it_leaves_behind() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "kept", "kept");
	let install = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"kept",
	);
	assert!(
		install.status.success(),
		"seed install: {}",
		String::from_utf8_lossy(&install.stderr)
	);

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "delete", "skills", "kept", "--yes"])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&out.stdout);
	assert!(out.status.success());
	// Pins the FACTS (the master path is named, and it is called out as kept),
	// not the heading's exact wording — a reword should not turn this red.
	assert!(
		text.contains(".agents/skills") && text.contains("NOT removed"),
		"a committed delete must name the surviving master: {text}"
	);
	assert!(
		home.path().join(".agents/skills/kept").exists(),
		"the master really does survive — that is why it must be reported"
	);
}

/// Re-adding an installed skill writes NOTHING. It used to report the freshly
/// parsed SOURCE file as if it had been installed, so an edited source printed
/// its new frontmatter while disk still held the old Master.
#[cfg(unix)]
#[test]
fn re_add_reports_the_installed_master_not_the_source_file() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(src.path().join("drifted")).unwrap();
	let source_md = src.path().join("drifted/SKILL.md");
	std::fs::write(
		&source_md,
		"---\nname: drifted\ndescription: original\n---\n",
	)
	.unwrap();

	let first = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "add", "skills", "--json", "--from"])
		.arg(src.path().join("drifted"))
		.output()
		.unwrap();
	assert!(
		first.status.success(),
		"first add: {}",
		String::from_utf8_lossy(&first.stderr)
	);

	// Edit the SOURCE only. The Master on disk still says "original".
	std::fs::write(
		&source_md,
		"---\nname: drifted\ndescription: EDITED\n---\n",
	)
	.unwrap();

	let second = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "add", "skills", "--json", "--from"])
		.arg(src.path().join("drifted"))
		.output()
		.unwrap();
	assert!(second.status.success());
	let payload: Value = serde_json::from_slice(&second.stdout).unwrap();
	assert_eq!(
		payload["description"], "original",
		"the re-add must report the UNTOUCHED master, not the edited source"
	);
	assert_eq!(
		payload["already_installed"], true,
		"the payload must mark the re-add as a no-op"
	);
	assert!(
		String::from_utf8_lossy(&second.stderr).contains("nothing was written"),
		"a write-nothing re-add must say so; stderr: {}",
		String::from_utf8_lossy(&second.stderr)
	);

	// And the HUMAN verb must match: "added" on a write-nothing run is the
	// exact misreport this whole change exists to remove.
	let human = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "add", "skills", "--from"])
		.arg(src.path().join("drifted"))
		.output()
		.unwrap();
	let verb = String::from_utf8_lossy(&human.stdout);
	assert!(
		verb.contains("already installed") && !verb.contains("added skill"),
		"a no-op re-add must not say 'added': {verb}"
	);
	let master = std::fs::read_to_string(
		home.path().join(".agents/skills/drifted/SKILL.md"),
	)
	.unwrap();
	assert!(
		master.contains("original") && !master.contains("EDITED"),
		"master really was left alone: {master}"
	);
}

/// A fetch failure must name its cause. `FetchError::Network` was payload-free,
/// so every failure — DNS, 404, TLS, a bad ref — printed the same
/// "Failed to fetch source repository '<url>'" with no way to tell them apart,
/// not even under `-v`.
#[test]
fn fetch_failure_reports_the_underlying_reason() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		// A path that is not a directory drives the debug fetch hook down its
		// failure arm without touching the network.
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", home.path().join("nope"))
		.args([
			"-g",
			"source",
			"sync",
			"owner/repo",
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(!out.status.success(), "a failed fetch must exit non-zero");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("Failed to fetch source repository"),
		"stderr: {stderr}"
	);
	assert!(
		stderr.contains("AGHUB_TEST_SOURCE_FETCH_ROOT"),
		"the message must carry the underlying reason, not just the url: \
		 {stderr}"
	);
}

/// `source sync` IS the install entry point (there is no `source add`), so it
/// answers to `install` and its help says so.
#[test]
fn source_sync_is_reachable_as_install_and_documents_it() {
	let dir = fixtures_dir();
	let help = aghub_cli()
		.current_dir(&dir)
		.args(["source", "install", "--help"])
		.output()
		.unwrap();
	assert!(
		help.status.success(),
		"`source install` must be a valid alias"
	);
	let text = String::from_utf8_lossy(&help.stdout);
	assert!(
		text.contains("--install-missing"),
		"install help must show the flag that actually installs: {text}"
	);
}

/// `--format` takes a closed set, but it is a `value_parser` (not a ValueEnum),
/// so clap prints no `[possible values]`. Both the help and the rejection must
/// carry the list themselves or the flag is unguessable.
#[test]
fn inference_format_lists_its_accepted_values() {
	let dir = fixtures_dir();
	let help = aghub_cli()
		.current_dir(&dir)
		.args(["inference", "add", "--help"])
		.output()
		.unwrap();
	let help_text = String::from_utf8_lossy(&help.stdout);
	for value in ["anthropic", "openai_completions", "openai_responses"] {
		assert!(
			help_text.contains(value),
			"help must list '{value}': {help_text}"
		);
	}

	let bad = aghub_cli()
		.current_dir(&dir)
		.args([
			"inference",
			"add",
			"--latin-name",
			"x",
			"--display-name",
			"X",
			"--api-base-url",
			"https://example.invalid",
			"--api-key",
			"k",
			"--format",
			"bogus",
		])
		.output()
		.unwrap();
	assert!(!bad.status.success());
	let stderr = String::from_utf8_lossy(&bad.stderr);
	assert!(
		stderr.contains("anthropic") && stderr.contains("openai_responses"),
		"the rejection must list what IS accepted: {stderr}"
	);
}

// ───────────── Findings from the Codex adversarial review (2.11) ───────────

/// `delete` must never claim a resource is absent. `RemovalView` cannot express
/// that: an MCP that EXISTS and one that does not serialize identically (MCP
/// removal rewrites shared config and deletes no path, so `paths` is always
/// empty). The first renderer used `paths.is_empty()` as "not installed" and so
/// told you an installed MCP was not there — while `--yes` really removed it.
#[cfg(unix)]
#[test]
fn delete_mcp_preview_never_claims_it_is_absent() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let add = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"claude",
			"add",
			"mcps",
			"-n",
			"live-mcp",
			"--url",
			"http://example.invalid",
		])
		.output()
		.unwrap();
	assert!(
		add.status.success(),
		"seed add: {}",
		String::from_utf8_lossy(&add.stderr)
	);

	let preview = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "delete", "mcps", "live-mcp"])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&preview.stdout);
	assert!(preview.status.success());
	assert!(
		!text.contains("not installed"),
		"the payload cannot prove absence, so the renderer must not claim it: \
		 {text}"
	);
	assert!(
		text.contains("would remove") && text.contains("--yes"),
		"a preview must still name itself and say how to commit: {text}"
	);

	// And the removal it previewed really does happen — which is exactly why
	// calling it "not installed" was dangerous.
	let commit = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "delete", "mcps", "live-mcp", "--yes"])
		.output()
		.unwrap();
	assert!(commit.status.success());
	assert!(
		!mcp_listed(home.path(), state.path(), "live-mcp"),
		"--yes must really remove the MCP the preview described"
	);
}

/// `add skills` must emit ONE schema. `--from` carries `already_installed`
/// because its install can no-op; a manual add never no-ops but must still
/// carry the key, or a `-a a,b` batch envelope mixes two shapes.
#[cfg(unix)]
#[test]
fn add_skill_json_schema_is_the_same_with_and_without_from() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(src.path().join("from-path")).unwrap();
	std::fs::write(
		src.path().join("from-path/SKILL.md"),
		"---\nname: from-path\ndescription: d\n---\n",
	)
	.unwrap();

	let manual = isolated_cli(home.path(), state.path())
		.args([
			"-g", "-a", "claude", "add", "skills", "--json", "-n", "manual",
		])
		.output()
		.unwrap();
	assert!(
		manual.status.success(),
		"manual add: {}",
		String::from_utf8_lossy(&manual.stderr)
	);
	let manual_json: Value = serde_json::from_slice(&manual.stdout).unwrap();

	let imported = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "add", "skills", "--json", "--from"])
		.arg(src.path().join("from-path"))
		.output()
		.unwrap();
	assert!(imported.status.success());
	let imported_json: Value =
		serde_json::from_slice(&imported.stdout).unwrap();

	assert_eq!(
		manual_json["already_installed"], false,
		"a manual add must still carry the discriminator: {manual_json}"
	);
	assert_eq!(
		imported_json["already_installed"], false,
		"a fresh --from import is not a no-op: {imported_json}"
	);
	// NOT a key-set comparison: both branches build the same `SkillView`, so
	// the key sets are structurally identical and such an assertion cannot
	// fail. The two value assertions above are what carry this test.
}

/// `--json` on a `plugin` action that has no JSON form must FAIL, not print
/// prose on a zero exit — a script would read that as success and break later.
#[test]
fn plugin_rejects_json_where_it_has_no_json_form() {
	let dir = fixtures_dir();
	let out = aghub_cli()
		.current_dir(&dir)
		.args(["--json", "plugin", "validate", "/nonexistent/path"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "--json must be refused, not ignored");
	assert!(
		String::from_utf8_lossy(&out.stderr).contains("--json is supported"),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}

/// `inference key` DOES have a JSON form, and neither form may carry the raw
/// key — only the masked preview and a presence bool.
#[test]
fn inference_key_supports_json_without_leaking_the_secret() {
	let data = tempfile::tempdir().unwrap();
	let id = add_provider(data.path(), "keyed");

	let out = inference_cli(data.path())
		.args(["inference", "key", &id, "--json"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"inference key --json must succeed; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let raw = String::from_utf8_lossy(&out.stdout);
	let json: Value = serde_json::from_str(&raw)
		.unwrap_or_else(|e| panic!("key --json must emit JSON ({e}): {raw}"));
	assert_eq!(json["id"], id);
	assert_eq!(json["stored"], true, "the key was stored by add_provider");
	assert!(
		!raw.contains("sk-test-secret-value"),
		"the raw api key must never be printed: {raw}"
	);

	// The default form stays the tab-separated line, and is equally secret-free.
	let text_out = inference_cli(data.path())
		.args(["inference", "key", &id])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&text_out.stdout);
	assert!(text.contains("stored=true"), "default form: {text}");
	assert!(
		!text.contains("sk-test-secret-value"),
		"the raw api key must never be printed: {text}"
	);
}

/// The scope help must not promise a guard that does not exist: `prune-lock`
/// accepts `--all` and writes BOTH locks, so the old blanket "rejected by every
/// command that writes" was false.
#[test]
fn scope_help_matches_what_prune_lock_actually_accepts() {
	let dir = fixtures_dir();
	let help = aghub_cli()
		.current_dir(&dir)
		.args(["--help"])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&help.stdout);
	assert!(
		text.contains("prune-lock"),
		"the --all paragraph must name the write command that DOES take it: \
		 {text}"
	);

	// And that is really what it does: --all is accepted, not rejected.
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args(["--all", "prune-lock"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"prune-lock --all must be accepted; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}

/// A `user:token@` a caller embedded in the source URL must not survive into
/// any error the CLI prints. `aghub_git` redacts what IT builds, but the CLI
/// echoes the raw `<SOURCE>` argument back in its own messages, and the fetch
/// detail is now surfaced too — both had to be routed through the redactor.
#[test]
fn embedded_credentials_never_survive_into_a_cli_error() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	for source in [
		"https://alice:SUPERSECRET@",
		"https://alice:SUPERSECRET@host.invalid/o/r",
		"ftp://alice:SUPERSECRET@host/x",
	] {
		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"source",
				"sync",
				source,
				"--install-missing",
				"--yes",
			])
			.output()
			.unwrap();
		assert!(!out.status.success(), "{source} must fail");
		let combined = format!(
			"{}{}",
			String::from_utf8_lossy(&out.stdout),
			String::from_utf8_lossy(&out.stderr)
		);
		assert!(
			!combined.contains("SUPERSECRET"),
			"the embedded secret leaked for {source}: {combined}"
		);
		// The redactor replaces the WHOLE `user:secret` segment, so the
		// username goes too — asserting that pins real redaction rather than a
		// message that merely happens to omit the password.
		assert!(
			!combined.contains("alice"),
			"userinfo must be redacted whole for {source}: {combined}"
		);
	}
}

// ────────── Findings from the three-way review round (2.11) ──────────

/// `--from X --name NEW` is import-then-rename. When X's OWN name is already
/// installed the import no-ops, so the rename would remove that master and
/// re-add its OLD content as NEW — writing content the user never pointed at
/// and deleting a skill they never asked to touch. It must refuse instead.
#[cfg(unix)]
#[test]
fn rename_import_refuses_when_the_source_name_is_already_installed() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src1 = tempfile::TempDir::new().unwrap();
	let src2 = tempfile::TempDir::new().unwrap();
	for (dir, body) in [(&src1, "ORIGINAL"), (&src2, "NEWCONTENT")] {
		std::fs::create_dir_all(dir.path().join("foo")).unwrap();
		std::fs::write(
			dir.path().join("foo/SKILL.md"),
			format!("---\nname: foo\ndescription: {body}\n---\n\n{body}\n"),
		)
		.unwrap();
	}

	let seed = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "add", "skills", "--from"])
		.arg(src1.path().join("foo"))
		.output()
		.unwrap();
	assert!(seed.status.success(), "seed install must succeed");

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g", "-a", "claude", "add", "skills", "--name", "bar", "--from",
		])
		.arg(src2.path().join("foo"))
		.output()
		.unwrap();
	assert!(!out.status.success(), "the rename import must be refused");

	// Nothing was written and nothing was destroyed.
	assert!(
		!home.path().join(".agents/skills/bar").exists(),
		"a refused import must not create the renamed skill"
	);
	let foo = std::fs::read_to_string(
		home.path().join(".agents/skills/foo/SKILL.md"),
	)
	.expect("the pre-existing skill must survive");
	assert!(
		foo.contains("ORIGINAL"),
		"the pre-existing skill must be untouched: {foo}"
	);
}

/// The same flags on a source whose name is NOT installed must still work —
/// the refusal above must not have broken the feature.
#[cfg(unix)]
#[test]
fn rename_import_still_works_when_the_source_name_is_free() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(src.path().join("fresh")).unwrap();
	std::fs::write(
		src.path().join("fresh/SKILL.md"),
		"---\nname: fresh\ndescription: BODY\n---\n\nBODY\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g", "-a", "claude", "add", "skills", "--json", "--name",
			"renamed", "--from",
		])
		.arg(src.path().join("fresh"))
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["name"], "renamed");
	assert_eq!(
		json["already_installed"], false,
		"a rename WRITES, so it is never a no-op: {json}"
	);
	// The paths must name the RENAMED skill, not the source's own name.
	for key in ["source_path", "canonical_path"] {
		let path = json[key].as_str().unwrap_or_default();
		assert!(
			path.contains("renamed") && !path.contains("fresh"),
			"{key} must point at the renamed skill, got {path}"
		);
	}
	let body = std::fs::read_to_string(
		home.path().join(".agents/skills/renamed/SKILL.md"),
	)
	.unwrap();
	assert!(
		body.contains("BODY"),
		"the source content must land: {body}"
	);
}

/// `delete --yes` on something already gone must not tell you to re-run with
/// `--yes`. A script retrying on that hint would never terminate.
#[cfg(unix)]
#[test]
fn delete_yes_on_an_absent_resource_does_not_ask_for_yes_again() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "delete", "mcps", "ghost", "--yes"])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&out.stdout);
	assert!(out.status.success(), "delete stays idempotent");
	assert!(
		!text.contains("--yes"),
		"--yes was already given; asking for it again is a loop: {text}"
	);
	assert!(
		text.contains("nothing to remove"),
		"it must say what happened: {text}"
	);
}

/// A fresh multi-agent install must not print the "nothing was written" drift
/// warning. A NativeReader no-ops as soon as ANY agent has the skill —
/// including a sibling row of the same run — so that warning told users to
/// delete a skill that had just installed correctly.
#[cfg(unix)]
#[test]
fn fresh_multi_agent_install_does_not_warn_about_drift() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(src.path().join("shared")).unwrap();
	std::fs::write(
		src.path().join("shared/SKILL.md"),
		"---\nname: shared\ndescription: d\n---\n\nbody\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude,codex", "add", "skills", "--from"])
		.arg(src.path().join("shared"))
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		!stderr.contains("nothing was written"),
		"a fresh install must not claim its content did not land: {stderr}"
	);
	assert!(
		home.path().join(".agents/skills/shared").exists(),
		"and it really did install"
	);
}

/// `describe` does no install, so the install advisories are always false —
/// printing "already_installed false" for an installed skill reads as a
/// contradiction. They stay in `--json` so the wire shape is unchanged.
#[cfg(unix)]
#[test]
fn describe_hides_install_advisories_from_the_human_block() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	write_claude_skill(home.path(), "described");

	let text_out = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "describe", "skills", "described"])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&text_out.stdout);
	assert!(text_out.status.success());
	assert!(text.contains("described"), "sanity: {text}");
	assert!(
		!text.contains("already_installed") && !text.contains("native_reader"),
		"install advisories are noise on a read: {text}"
	);

	let json_out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"claude",
			"describe",
			"skills",
			"described",
			"--json",
		])
		.output()
		.unwrap();
	let json: Value = serde_json::from_slice(&json_out.stdout).unwrap();
	assert_eq!(
		json["already_installed"], false,
		"--json keeps the full wire shape"
	);
	assert_eq!(json["native_reader"], false);
}

/// The multi-agent batch's HUMAN output. All six batch tests assert the JSON
/// envelope; without this the per-row rendering and the ok/failed tally could
/// regress to nothing and still read as success.
#[cfg(unix)]
#[test]
fn agent_list_human_output_renders_each_row_and_a_tally() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"claude,opencode",
			"add",
			"mcps",
			"-n",
			"batched",
			"--url",
			"http://example.invalid",
		])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let text = String::from_utf8_lossy(&out.stdout);
	assert!(
		text.contains("claude: added mcp 'batched'"),
		"each agent needs its own row: {text}"
	);
	assert!(
		text.contains("opencode: added mcp 'batched'"),
		"each agent needs its own row: {text}"
	);
	assert!(text.contains("2 ok, 0 failed"), "tally missing: {text}");
	assert!(
		serde_json::from_str::<Value>(&text).is_err(),
		"the default must not be JSON: {text}"
	);
}

/// A batch row that FAILS must say so by name, and the tally must count it —
/// a silently dropped failure row alongside "2 ok" would read as a clean run.
#[cfg(unix)]
#[test]
fn agent_list_human_output_names_the_failing_agent() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	// claude already has `dup`, so its row fails while opencode's succeeds —
	// the same setup the JSON-envelope partial-failure test uses.
	seed_mcp(home.path(), state.path(), "dup");

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"claude,opencode",
			"add",
			"mcps",
			"--name",
			"dup",
			"--url",
			"http://h",
		])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&out.stdout);
	assert!(
		!out.status.success(),
		"a failing row must exit non-zero: {text}"
	);
	assert!(
		text.contains("claude: FAILED"),
		"the failing agent must be named as failed: {text}"
	);
	assert!(
		text.contains("opencode: added mcp 'dup'"),
		"the surviving agent must still be attempted and reported: {text}"
	);
	assert!(
		text.contains("1 ok, 1 failed"),
		"the tally must count the failure: {text}"
	);
}

/// `check skills`' default table. `check` is what a human runs to decide
/// whether to `apply-update`; if the row or the offline hint regressed they
/// would conclude nothing needs updating.
#[cfg(unix)]
#[test]
fn check_skills_default_output_is_a_table_with_the_offline_hint() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "locked-one", "locked-one");
	let install = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"locked-one",
	);
	assert!(
		install.status.success(),
		"seed install: {}",
		String::from_utf8_lossy(&install.stderr)
	);

	let out = isolated_cli(home.path(), state.path())
		.args(["-g", "check", "skills"])
		.output()
		.unwrap();
	assert!(out.status.success());
	let text = String::from_utf8_lossy(&out.stdout);
	assert!(
		text.contains("SKILL") && text.contains("STATUS"),
		"default must be a table: {text}"
	);
	assert!(
		text.contains("locked-one") && text.contains("uncheckable"),
		"the locked skill's row must be there: {text}"
	);
	assert!(
		text.contains("--online"),
		"offline default must point at --online: {text}"
	);
	assert!(
		serde_json::from_str::<Value>(&text).is_err(),
		"the default must not be JSON: {text}"
	);
}

/// Every place the CLI echoes a SOURCE back must redact URL userinfo. `<SOURCE>`
/// comes straight from argv, so `https://user:token@host/repo` would otherwise
/// replay the token into stderr, CI logs, and shell scrollback. `aghub_git`
/// redacts the URLs it BUILDS; these are the strings the CLI prints itself, on
/// both halves of the line — the `'{source}'` quote AND the `{detail}` that
/// `SourceError` fills with the same raw input.
#[test]
fn source_errors_never_replay_url_userinfo() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	const SECRET: &str = "hunter2SUPERSECRET";

	// One case per error path that quotes SOURCE: an unreachable https host
	// (fetch failure), an unsupported scheme (early refusal), and the fetch
	// hook's own failure arm.
	let cases: [(&str, Vec<&str>); 4] = [
		(
			"fetch failure",
			vec![
				"-g",
				"source",
				"sync",
				"https://alice:hunter2SUPERSECRET@no-such-host.invalid/o/r",
				"--install-missing",
				"--yes",
			],
		),
		(
			"unsupported scheme",
			vec![
				"-g",
				"source",
				"sync",
				"ftp://alice:hunter2SUPERSECRET@host/x",
				"--install-missing",
				"--yes",
			],
		),
		(
			"diff",
			vec![
				"-g",
				"source",
				"diff",
				"https://alice:hunter2SUPERSECRET@no-such-host.invalid/o/r",
			],
		),
		(
			// Scheme-less scp-like: git accepts it, so people type it, and the
			// url-anchored redactor alone let it through verbatim.
			"scp-like",
			vec![
				"-g",
				"source",
				"sync",
				"alice:hunter2SUPERSECRET@no-such-host.invalid/o/r",
				"--install-missing",
				"--yes",
			],
		),
	];

	for (label, args) in cases {
		let out = isolated_cli(home.path(), state.path())
			.args(&args)
			.output()
			.unwrap();
		let combined = format!(
			"{}{}",
			String::from_utf8_lossy(&out.stdout),
			String::from_utf8_lossy(&out.stderr)
		);
		assert!(
			!out.status.success(),
			"{label}: expected a failure to inspect, got success: {combined}"
		);
		assert!(
			!combined.contains(SECRET),
			"{label}: the CLI replayed the URL password: {combined}"
		);
		assert!(
			combined.contains("***"),
			"{label}: userinfo must be redacted, not dropped silently, so the \
			 user can still recognise the source: {combined}"
		);
	}
}

/// The CRUD verbs print a human line by default. Only `get`/`delete` had that
/// pinned; the rest were covered exclusively by tests that pass `--json`, so a
/// regression back to raw JSON on the default path would have gone unnoticed.
#[cfg(unix)]
#[test]
fn crud_verbs_print_human_lines_by_default() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let run = |args: &[&str]| {
		let out = isolated_cli(home.path(), state.path())
			.args(args)
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"{args:?} failed: {}",
			String::from_utf8_lossy(&out.stderr)
		);
		String::from_utf8_lossy(&out.stdout).to_string()
	};

	for (args, expected) in [
		(
			vec!["-g", "-a", "claude", "add", "skills", "-n", "verbs"],
			"added skill 'verbs'",
		),
		(
			vec![
				"-g", "-a", "claude", "update", "skills", "verbs", "-d", "new",
			],
			"updated skill 'verbs'",
		),
	] {
		let text = run(&args);
		assert!(
			text.contains(expected),
			"{args:?} must print '{expected}', got: {text}"
		);
		assert!(
			serde_json::from_str::<Value>(&text).is_err(),
			"{args:?} must NOT emit JSON by default: {text}"
		);
	}

	// `describe` is a key/value block, not JSON, and drops null fields.
	let described = run(&["-g", "-a", "claude", "describe", "skills", "verbs"]);
	assert!(
		described.contains("name") && described.contains("verbs"),
		"describe must print a key/value block: {described}"
	);
	assert!(
		!described.contains("author"),
		"describe must omit null fields from the human form: {described}"
	);
	assert!(
		serde_json::from_str::<Value>(&described).is_err(),
		"describe must not emit JSON by default: {described}"
	);
}

/// `disable`/`enable skills` must REFUSE, not print a success line.
///
/// Nothing persists a skill's enabled flag — `save()` serializes MCPs only — so
/// the old success line was a lie that also rewrote the agent's MCP config as a
/// side effect. Exit status alone is not enough to pin: a refusal that still
/// wrote would satisfy it, so this also proves `.mcp.json` is byte-identical.
#[cfg(unix)]
#[test]
fn skill_enable_disable_refuse_and_leave_mcp_config_alone() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let added = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "add", "skills", "-n", "verbs"])
		.output()
		.unwrap();
	assert!(added.status.success(), "fixture skill must be added");

	// `customField` is the canary: aghub does not model it, so any rewrite of
	// the agent's MCP config drops it.
	let mcp_path = home.path().join(".claude.json");
	let original = "{\n  \"mcpServers\": {\n    \"demo\": {\n      \"command\": \"echo\",\n      \"args\": [\"hi\"],\n      \"customField\": \"keepme\"\n    }\n  }\n}\n";
	std::fs::write(&mcp_path, original).unwrap();

	// The refusal now happens EARLIER than it used to: `enable`/`disable` take
	// the narrowed `McpResource`, so clap rejects `skills` at parse time with an
	// exact `[possible values: mcps]` instead of letting the request reach core
	// for an "Unsupported operation" bail. Core's refusal is still the one that
	// protects `.mcp.json` and is still reachable through the API routes —
	// `disable_skill_refuses_and_leaves_the_mcp_config_untouched` in
	// `core/src/manager/skill.rs` pins it. What this test now pins is that the
	// CLI never advertises the dead form at all, and still touches nothing.
	for verb in ["disable", "enable"] {
		let out = isolated_cli(home.path(), state.path())
			.args(["-g", "-a", "claude", verb, "skills", "verbs"])
			.output()
			.unwrap();
		let stderr = String::from_utf8_lossy(&out.stderr).to_string();
		assert!(
			!out.status.success(),
			"`{verb} skills` must fail, not report a success it cannot deliver"
		);
		assert_eq!(
			out.status.code(),
			Some(2),
			"it must be a clap usage error, caught before any work: {stderr}"
		);
		assert!(
			stderr.contains("invalid value 'skills'")
				&& stderr.contains("[possible values: mcps]"),
			"`{verb}` must not advertise a resource no agent supports: \
			 {stderr}"
		);
		assert_eq!(
			std::fs::read_to_string(&mcp_path).unwrap(),
			original,
			"`{verb} skills` must not touch the agent's MCP config"
		);
	}
}

/// `check` and `prune-lock` are the two read paths a human runs to decide
/// whether to mutate, so their default form must be readable — and `check` must
/// only suggest `--online` when it is actually offline.
#[cfg(unix)]
#[test]
fn check_and_prune_lock_print_human_output_by_default() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "locked", "locked");
	let install = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"locked",
	);
	assert!(
		install.status.success(),
		"seed install: {}",
		String::from_utf8_lossy(&install.stderr)
	);

	let check = isolated_cli(home.path(), state.path())
		.args(["-g", "check", "skills"])
		.output()
		.unwrap();
	let text = String::from_utf8_lossy(&check.stdout);
	assert!(check.status.success());
	assert!(
		text.contains("SKILL") && text.contains("locked"),
		"check must render a table by default: {text}"
	);
	assert!(
		serde_json::from_str::<Value>(&text).is_err(),
		"check must not emit JSON by default: {text}"
	);
	assert!(
		text.contains("pass --online"),
		"the offline default must say how to get a real answer: {text}"
	);

	let pruned = isolated_cli(home.path(), state.path())
		.args(["-g", "prune-lock"])
		.output()
		.unwrap();
	let ptext = String::from_utf8_lossy(&pruned.stdout);
	assert!(pruned.status.success());
	assert!(
		ptext.contains("No orphaned lock entries."),
		"prune-lock must say so in words, not as {{\"pruned\": []}}: {ptext}"
	);
}

// ==================== agent-operability regressions (2026-08-27 audit) ====

/// `add skills` on a name that is already installed must report the SKILL AS
/// IT IS ON DISK, not echo the request back.
///
/// The regression: two branches of `add_skill_universal` were idempotent no-ops
/// that returned a bare `Ok(())`, so the CLI built its payload from the request
/// and hard-coded `already_installed: false`. Re-running `add` with a corrected
/// `--description` printed "added skill", serialized the NEW description, and
/// exited 0 while the Master on disk kept the OLD one. Re-running `add` with
/// fixed metadata is a standard scripted repair, so an agent was told its write
/// had landed when nothing was written.
#[cfg(unix)]
#[test]
fn readd_existing_skill_reports_already_installed_and_keeps_disk_content() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	let first = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "skills", "--name", "dup", "-d", "first desc"])
		.output()
		.unwrap();
	assert!(
		first.status.success(),
		"first add must succeed: {}",
		String::from_utf8_lossy(&first.stderr)
	);

	let second = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args([
			"-p",
			"--json",
			"add",
			"skills",
			"--name",
			"dup",
			"-d",
			"SECOND desc",
		])
		.output()
		.unwrap();
	assert!(
		second.status.success(),
		"an idempotent re-add stays a success: {}",
		String::from_utf8_lossy(&second.stderr)
	);

	let json: Value = serde_json::from_slice(&second.stdout)
		.expect("add --json must emit JSON");
	assert_eq!(
		json["already_installed"], true,
		"a re-add that wrote nothing must say so: {json}"
	);
	assert_eq!(
		json["description"], "first desc",
		"the payload must carry the on-disk description, not the requested \
		 one: {json}"
	);

	let master = project.path().join(".agents/skills/dup/SKILL.md");
	let body = std::fs::read_to_string(&master).unwrap();
	assert!(
		body.contains("description: first desc"),
		"the Master must be untouched: {body}"
	);
	assert!(
		!body.contains("SECOND desc"),
		"the re-add must not have rewritten the Master: {body}"
	);

	let stderr = String::from_utf8_lossy(&second.stderr);
	assert!(
		stderr.contains("nothing was written"),
		"a human must be told the add was a no-op: {stderr}"
	);
}

/// A config file that EXISTS but does not parse must fail every command, not be
/// treated as an absent one.
///
/// The regression: `main.rs` picked which load errors to tolerate by matching
/// on the COMMAND (`Add`/`Check`/`PruneLock`/`Delete`) and ignored the error
/// KIND. `delete --yes` then found `config().is_none()`, mapped it to the
/// idempotent-delete no-op, and printed `{"success":true,"executed":false}` on
/// exit 0 — while the entry it claimed to have handled was still in the file.
#[cfg(unix)]
#[test]
fn malformed_agent_config_fails_delete_instead_of_reporting_success() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	// Valid JSON followed by a stray token: parses far enough to be obviously
	// a real config, then fails.
	let mcp_path = project.path().join(".mcp.json");
	let malformed =
		r#"{ "mcpServers": { "keepme": { "command": "echo" } } } OOPS"#;
	std::fs::write(&mcp_path, malformed).unwrap();

	let out = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "--json", "delete", "mcps", "keepme", "--yes"])
		.output()
		.unwrap();

	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		!out.status.success(),
		"a delete against an unparseable config must fail, not report a \
		 no-op success: stdout={} stderr={stderr}",
		String::from_utf8_lossy(&out.stdout)
	);
	assert!(
		stderr.contains("parse"),
		"the failure must name the parse problem: {stderr}"
	);
	assert_eq!(
		std::fs::read_to_string(&mcp_path).unwrap(),
		malformed,
		"the malformed config must be left exactly as it was"
	);

	// `add` shares the same tolerate-missing path.
	let added = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "mcps", "--name", "other", "-c", "echo"])
		.output()
		.unwrap();
	assert!(
		!added.status.success(),
		"add must not silently start from a blank config either: {}",
		String::from_utf8_lossy(&added.stdout)
	);
}

/// `-p` with no project root must fail on READ commands too, not answer `[]`.
///
/// The regression: the project-root guard only ran for `ScopePolicy::
/// SingleWrite`, so `-p get skills --json` from a non-project directory printed
/// `[]` on exit 0 with an empty stderr — byte-identical, on all three channels,
/// to a real project holding no skills. `coverage`, `doctor`, `source list` and
/// `prune-lock` already bailed, and `describe` blamed the resource name for a
/// missing project.
#[cfg(unix)]
#[test]
fn project_scope_without_project_root_fails_on_read_commands_too() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	// No agent marker anywhere in this tree.
	let nowhere = tempfile::TempDir::new().unwrap();

	let cases: &[&[&str]] = &[
		&["-p", "--json", "get", "skills"],
		&["-p", "--json", "get", "mcps"],
		&["-p", "--json", "check", "skills"],
		&["-p", "describe", "skills", "anything"],
	];

	for args in cases {
		let out = isolated_cli(home.path(), state.path())
			.current_dir(nowhere.path())
			.args(*args)
			.output()
			.unwrap();
		let stdout = String::from_utf8_lossy(&out.stdout);
		let stderr = String::from_utf8_lossy(&out.stderr);
		assert!(
			!out.status.success(),
			"{args:?} must not report an empty world when there is no \
			 project: stdout={stdout}"
		);
		assert!(
			stderr.contains("no project root found"),
			"{args:?} must blame the missing project, not the resource: \
			 {stderr}"
		);
	}

	// The read-only both-scopes path must still work with no project root.
	let both = isolated_cli(home.path(), state.path())
		.current_dir(nowhere.path())
		.args(["--all", "--json", "get", "skills"])
		.output()
		.unwrap();
	assert!(
		both.status.success(),
		"--all must still span whatever scopes exist: {}",
		String::from_utf8_lossy(&both.stderr)
	);
}

/// `reconcile` must reject an empty target set, and its preview must validate
/// the source instead of echoing argv.
///
/// The regression: with neither `--add` nor `--remove`, it fell through to an
/// empty batch and printed `{"success_count":0,"failed_count":0,"results":[]}`
/// on exit 0 — and `--agent` is exactly where a caller lands, because clap's
/// own "a similar argument exists: '--agent'" tip for a mistyped `--agents`
/// points there while the usage line never mentions `--add`. Separately, the
/// dry-run was a pure echo, so a `--name` that does not exist was green-lit and
/// only failed on the `--yes` run.
#[cfg(unix)]
#[test]
fn reconcile_rejects_empty_target_set_and_validates_source_in_preview() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	let seeded = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "skills", "--name", "present", "-d", "d"])
		.output()
		.unwrap();
	assert!(seeded.status.success());

	// No --add / --remove: a usage error, not a successful no-op.
	let empty = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args([
			"-p",
			"--json",
			"reconcile",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"present",
			"--agent",
			"opencode",
		])
		.output()
		.unwrap();
	let stderr = String::from_utf8_lossy(&empty.stderr);
	assert!(
		!empty.status.success(),
		"a reconcile with no target must fail: stdout={}",
		String::from_utf8_lossy(&empty.stdout)
	);
	assert!(
		stderr.contains("--add") && stderr.contains("--remove"),
		"the error must name the flags that actually pick targets: {stderr}"
	);

	// A preview for a source that does not exist must fail HERE.
	let absent = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args([
			"-p",
			"--json",
			"reconcile",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"totally-absent",
			"--remove",
			"opencode",
		])
		.output()
		.unwrap();
	assert!(
		!absent.status.success(),
		"a preview must not green-light a missing source: stdout={}",
		String::from_utf8_lossy(&absent.stdout)
	);
	assert!(
		String::from_utf8_lossy(&absent.stderr).contains("not found"),
		"the preview must report the missing resource: {}",
		String::from_utf8_lossy(&absent.stderr)
	);

	// A preview for a source that DOES exist still previews (no over-reach).
	let ok = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args([
			"-p",
			"--json",
			"reconcile",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"present",
			"--remove",
			"opencode",
		])
		.output()
		.unwrap();
	assert!(
		ok.status.success(),
		"an existing source must still preview: {}",
		String::from_utf8_lossy(&ok.stderr)
	);
	let json: Value = serde_json::from_slice(&ok.stdout).unwrap();
	assert_eq!(json["dry_run"], true, "the preview must say so: {json}");
}

/// `source accept-rename` without `--yes` must emit JSON under `--json`, and
/// must validate the lock BEFORE reporting a plan.
///
/// The regression: the dry-run branch was `println!` prose + `return Ok(())`
/// and never read `args.json` — a destructive command's DEFAULT path emitting
/// unparseable text on exit 0. A strict parser sees a crash on the success
/// path; a lenient one concludes the rename was committed. The lock read and
/// the degenerate-name guard also sat AFTER that return, so the preview green-lit
/// a name that is not in the lock at all.
///
/// The SUCCESS case below is load-bearing and was missing at first: with only
/// the two failing cases, both returned before ever reaching the renderer, the
/// JSON on stdout came from the global `report_failure`, and reverting the
/// renderer fix back to prose left the test GREEN. A seeded lock entry is what
/// makes the preview actually render.
#[cfg(unix)]
#[test]
fn accept_rename_preview_validates_lock_and_honours_json() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();

	// A real lock entry, so the preview reaches its renderer instead of
	// bailing early.
	write_source_skill(src.path(), "renamable", "renamable");
	let installed = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"renamable",
	);
	assert!(
		installed.status.success(),
		"fixture install must succeed: {}",
		String::from_utf8_lossy(&installed.stderr)
	);

	let preview = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"--json",
			"source",
			"accept-rename",
			"renamable",
			"renamable-new",
		])
		.output()
		.unwrap();
	assert!(
		preview.status.success(),
		"a preview of a LOCKED skill must succeed: {}",
		String::from_utf8_lossy(&preview.stderr)
	);
	let pj: Value = serde_json::from_slice(&preview.stdout).expect(
		"the preview MUST emit JSON under --json — this is the assertion the \
		 two failing cases below cannot make, because they never reach the \
		 renderer",
	);
	assert_eq!(pj["dryRun"], true, "{pj}");
	assert_eq!(pj["applied"], false, "{pj}");
	assert_eq!(pj["oldName"], "renamable", "{pj}");
	assert_eq!(pj["newName"], "renamable-new", "{pj}");
	// And it must not have written anything.
	let after = isolated_cli(home.path(), state.path())
		.args(["-g", "--json", "get", "skills"])
		.output()
		.unwrap();
	let listed: Value = serde_json::from_slice(&after.stdout).unwrap();
	assert!(
		listed
			.as_array()
			.unwrap()
			.iter()
			.any(|s| s["name"] == "renamable"),
		"a preview must leave the original name in place: {listed}"
	);

	// Nothing is in the lock, so the preview must fail — not print a plan.
	let absent = isolated_cli(home.path(), state.path())
		.args(["--json", "source", "accept-rename", "ghost", "ghost-new"])
		.output()
		.unwrap();
	assert!(
		!absent.status.success(),
		"the preview must not plan a rename of a name that is not locked: \
		 stdout={}",
		String::from_utf8_lossy(&absent.stdout)
	);

	// A degenerate rename must also be refused before any plan is printed.
	let same = isolated_cli(home.path(), state.path())
		.args(["--json", "source", "accept-rename", "same", "same"])
		.output()
		.unwrap();
	assert!(
		!same.status.success(),
		"a degenerate rename must be refused in the preview: stdout={}",
		String::from_utf8_lossy(&same.stdout)
	);

	// Whatever the outcome, the preview must never print prose under --json.
	for out in [&absent, &same] {
		let stdout = String::from_utf8_lossy(&out.stdout);
		assert!(
			stdout.trim().is_empty()
				|| serde_json::from_str::<Value>(&stdout).is_ok(),
			"accept-rename must not emit prose on stdout under --json: \
			 {stdout}"
		);
	}
}

/// An unreadable skill lock must fail the read-only commands that REPORT its
/// contents, not read as an empty one.
///
/// The regression: both lock read paths fail OPEN to an empty lock (deliberate —
/// one corrupt file must not break every query) and announced it only through
/// `log::warn!`. Nothing in this workspace installed a logger outside its own
/// tests, so the warning went to the no-op logger. `check skills`, `source list`
/// and `doctor` therefore answered `[]` on exit 0 with an EMPTY stderr for a
/// lock full of entries they could not parse — and `doctor` classified the
/// still-present skills `untracked` and printed remediation saying to delete
/// them.
#[cfg(unix)]
#[test]
fn unreadable_lock_fails_the_commands_that_report_it() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	let seeded = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "skills", "--name", "alpha", "-d", "d"])
		.output()
		.unwrap();
	assert!(seeded.status.success());

	let lock = project.path().join("skills-lock.json");
	let corrupt = "not json at all";
	std::fs::write(&lock, corrupt).unwrap();

	let reporters: &[&[&str]] = &[
		&["-p", "--json", "check", "skills"],
		&["-p", "--json", "source", "list"],
		&["-p", "--json", "doctor"],
	];
	for args in reporters {
		let out = isolated_cli(home.path(), state.path())
			.current_dir(project.path())
			.args(*args)
			.output()
			.unwrap();
		let stdout = String::from_utf8_lossy(&out.stdout);
		let stderr = String::from_utf8_lossy(&out.stderr);
		assert!(
			!out.status.success(),
			"{args:?} must not report a clean world for an unreadable lock: \
			 stdout={stdout}"
		);
		assert!(
			stderr.contains("could not be read"),
			"{args:?} must say the lock is unreadable: {stderr}"
		);
		// A read-only command must NOT claim it was about to write.
		assert!(
			!stderr.contains("refusing to overwrite"),
			"{args:?} is read-only; its error must not imply a write: {stderr}"
		);
	}

	assert_eq!(
		std::fs::read_to_string(&lock).unwrap(),
		corrupt,
		"none of the read-only commands may touch the lock"
	);

	// An ABSENT lock is still a legitimate empty answer, not an error.
	std::fs::remove_file(&lock).unwrap();
	let absent = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "--json", "check", "skills"])
		.output()
		.unwrap();
	assert!(
		absent.status.success(),
		"a missing lock must stay an empty success: {}",
		String::from_utf8_lossy(&absent.stderr)
	);
}

/// Under `--json`, a failure must be machine-readable too — and must never
/// append a SECOND JSON document after a payload that already reported it.
///
/// The regression: `fn main() -> anyhow::Result<()>` let anyhow print every
/// error, so `--json` failures put nothing on stdout and one line of English on
/// stderr. Policy refusals, missing resources, invalid agent ids and real write
/// failures were all exit 1, separable only by matching prose that is neither
/// stable nor consistent (`describe` said `Skill 'x' not found` where every
/// other command said `Resource not found: skill 'x'`).
#[cfg(unix)]
#[test]
fn json_mode_reports_failures_as_json_with_a_shared_code() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	// A core ConfigError must carry the SHARED code the HTTP API sends.
	let missing = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "--json", "describe", "skills", "ghost"])
		.output()
		.unwrap();
	assert!(!missing.status.success());
	let json: Value = serde_json::from_slice(&missing.stdout).expect(
		"a --json failure must put a parseable document on stdout, not just \
		 prose on stderr",
	);
	assert_eq!(
		json["error"]["code"], "RESOURCE_NOT_FOUND",
		"the code must come from the shared vocabulary: {json}"
	);
	assert_eq!(json["error"]["retryable"], false, "{json}");
	assert!(
		json["error"]["message"].as_str().unwrap().contains("ghost"),
		"the message must name the resource: {json}"
	);

	// Same condition through a different command: same code AND same wording.
	let seeded = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "skills", "--name", "real", "-d", "d"])
		.output()
		.unwrap();
	assert!(seeded.status.success());
	let via_disable = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		// codex supports MCP enable/disable, so this reaches the
		// not-found path rather than an unsupported-operation refusal.
		.args(["-p", "-a", "codex", "--json", "disable", "mcps", "ghost"])
		.output()
		.unwrap();
	let dj: Value = serde_json::from_slice(&via_disable.stdout).unwrap();
	assert_eq!(dj["error"]["code"], json["error"]["code"]);
	// Same shape of wording ("Resource not found: <kind> '<name>'") for the
	// same condition; the kind differs because the commands take different
	// resources. `describe` used to say "Skill 'x' not found" instead, so a
	// matcher tuned to one missed the other entirely.
	let msg = dj["error"]["message"].as_str().unwrap();
	assert!(
		msg.starts_with("Resource not found:") && msg.contains("ghost"),
		"one condition must have one wording shape across commands: {msg}"
	);

	// A CLI-authored refusal is still machine-readable, and honest about having
	// no finer classification.
	let refused = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "--json", "apply-update", "skills", "real"])
		.output()
		.unwrap();
	assert!(!refused.status.success());
	let rj: Value = serde_json::from_slice(&refused.stdout)
		.expect("a policy refusal must be JSON too");
	assert_eq!(rj["error"]["code"], "CLI_ERROR", "{rj}");

	// Without --json the failure stays prose on stderr, stdout untouched.
	let prose = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "describe", "skills", "ghost"])
		.output()
		.unwrap();
	assert!(prose.stdout.is_empty(), "no JSON without --json");
	assert!(String::from_utf8_lossy(&prose.stderr).contains("ghost"));
}

/// An invalid `--agent` must fail the SAME way on every command.
///
/// The regression: `AgentSelection::parse` ran after eight early dispatches, so
/// `-a bogus` exited 1 on `get`/`check`/`prune-lock`/`delete` and exited 0 —
/// silently ignoring the typo — on `coverage`/`doctor`/`source list`/
/// `skill-usage`. `doctor` and `doctor --verify-links`, the same subcommand,
/// disagreed. No command could be used to validate an id before a write.
#[cfg(unix)]
#[test]
fn invalid_agent_id_fails_consistently_across_commands() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	let commands: &[&[&str]] = &[
		&["get", "skills"],
		&["check", "skills"],
		&["prune-lock"],
		&["coverage"],
		&["doctor"],
		&["doctor", "--verify-links"],
		&["source", "list"],
		&["skill-usage"],
	];

	for args in commands {
		let mut argv = vec!["-a", "definitely-not-an-agent"];
		argv.extend_from_slice(args);
		let out = isolated_cli(home.path(), state.path())
			.args(&argv)
			.output()
			.unwrap();
		let stderr = String::from_utf8_lossy(&out.stderr);
		assert!(
			!out.status.success(),
			"{args:?} must reject an unknown agent id instead of ignoring it: \
			 stdout={}",
			String::from_utf8_lossy(&out.stdout)
		);
		assert!(
			stderr.contains("unknown agent"),
			"{args:?} must name the problem: {stderr}"
		);
	}

	// A VALID id must still be accepted (and ignored where it has no meaning).
	for args in commands {
		let mut argv = vec!["-a", "codex"];
		argv.extend_from_slice(args);
		let out = isolated_cli(home.path(), state.path())
			.args(&argv)
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"{args:?} must still accept a valid agent id: {}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
}

/// A preview, a real removal and an already-absent resource must be three
/// distinguishable answers in the JSON.
///
/// The regression: `RemovalView::dry_run` was `!outcome.executed`, so
/// `delete skills nope -y --json` and `delete skills nope --json` produced
/// BYTE-IDENTICAL documents — both `{success:true, dry_run:true,
/// executed:false, …}`. The human renderer told them apart perfectly ("nothing
/// to remove" vs "would remove … re-run with --yes"); only the machine shape
/// could not. A caller that passed `--yes` and read `dry_run: true` can only
/// conclude its confirmation was ignored, so it retries — and nothing ever
/// contradicts that, because the world is already in the requested state.
#[cfg(unix)]
#[test]
fn delete_json_distinguishes_preview_removed_and_absent() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	let delete = |args: &[&str]| -> Value {
		let out = isolated_cli(home.path(), state.path())
			.current_dir(project.path())
			.args(args)
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"{args:?} must succeed: {}",
			String::from_utf8_lossy(&out.stderr)
		);
		serde_json::from_slice(&out.stdout)
			.unwrap_or_else(|e| panic!("{args:?} must emit JSON: {e}"))
	};

	// ABSENT is absent whether or not the caller confirmed. `absent` outranks
	// the caller's intent on purpose: an unconfirmed delete of something that
	// does not exist is not a preview of any change, and calling it `preview`
	// invites a `--yes` retry that also does nothing, forever.
	let absent_confirmed =
		delete(&["-p", "--json", "delete", "skills", "ghost", "--yes"]);
	let absent_preview = delete(&["-p", "--json", "delete", "skills", "ghost"]);
	assert_eq!(absent_confirmed["outcome"], "absent", "{absent_confirmed}");
	assert_eq!(absent_preview["outcome"], "absent", "{absent_preview}");
	assert_eq!(
		absent_confirmed["dry_run"], false,
		"a confirmed request is not a dry-run just because there was nothing \
		 to do: {absent_confirmed}"
	);

	// A REAL preview needs something that actually exists — and this is the
	// pair that used to serialize identically (same bytes, same md5) for
	// `delete X -y` and `delete X`.
	let seeded = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "skills", "--name", "doomed", "-d", "d"])
		.output()
		.unwrap();
	assert!(seeded.status.success());
	let preview = delete(&["-p", "--json", "delete", "skills", "doomed"]);
	assert_eq!(preview["outcome"], "preview", "{preview}");
	assert_eq!(preview["dry_run"], true, "{preview}");

	let removed =
		delete(&["-p", "--json", "delete", "skills", "doomed", "--yes"]);
	assert_eq!(removed["outcome"], "removed", "{removed}");
	assert_eq!(removed["executed"], true, "{removed}");
	assert_eq!(removed["dry_run"], false, "{removed}");

	// All three answers must be mutually distinguishable.
	for (a, b) in [
		(&preview, &removed),
		(&preview, &absent_confirmed),
		(&removed, &absent_confirmed),
	] {
		assert_ne!(a["outcome"], b["outcome"], "{a} vs {b}");
	}

	// MCP delete shares the seam, and its `paths` is deliberately always empty
	// (root AGENTS.md MCP removal contract) — so `outcome` is the ONLY thing
	// that can tell its three cases apart.
	let seeded_mcp = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "mcps", "-n", "doomed-mcp", "-c", "echo"])
		.output()
		.unwrap();
	assert!(seeded_mcp.status.success());
	let mcp_absent =
		delete(&["-p", "--json", "delete", "mcps", "ghost", "--yes"]);
	let mcp_preview = delete(&["-p", "--json", "delete", "mcps", "doomed-mcp"]);
	assert_eq!(mcp_absent["outcome"], "absent", "{mcp_absent}");
	assert_eq!(mcp_preview["outcome"], "preview", "{mcp_preview}");
	assert_ne!(mcp_absent["outcome"], mcp_preview["outcome"]);
}

/// A batch of agent-facing fixes that each turned a wrong or unusable answer
/// into an honest one. Grouped because each is a one-line assertion over a
/// distinct command; splitting them would add five near-identical fixtures.
#[cfg(unix)]
#[test]
fn agent_facing_message_and_flag_fixes() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	// A8: `source sync --yes` with no action flag must REFUSE, before any
	// network work. It used to fall through to the no-action overview: exit 0,
	// no `dryRun` key, and a third payload shape, so a consumer keyed on
	// `dryRun == false` read it as "the install was applied".
	let no_action = isolated_cli(home.path(), state.path())
		.args(["-g", "--json", "source", "sync", "owner/repo", "--yes"])
		.output()
		.unwrap();
	let stderr = String::from_utf8_lossy(&no_action.stderr);
	assert!(
		!no_action.status.success(),
		"`source sync --yes` with no action flag must refuse: stdout={}",
		String::from_utf8_lossy(&no_action.stdout)
	);
	assert!(
		stderr.contains("--install-missing") && stderr.contains("--update"),
		"the refusal must name the flags that make it a write: {stderr}"
	);
	assert!(
		!stderr.contains("credential"),
		"it must fail on the flag combination, not after a network attempt: \
		 {stderr}"
	);

	// C1: the `-a all` rejection must match the `-a` long help. It used to say
	// "supports only 'get'", contradicting both the help and the behaviour, and
	// its suggested remedy (a comma list) is itself rejected by this command.
	let all_rejected = isolated_cli(home.path(), state.path())
		.args(["-a", "all", "check", "skills"])
		.output()
		.unwrap();
	let msg = String::from_utf8_lossy(&all_rejected.stderr);
	assert!(!all_rejected.status.success());
	assert!(
		msg.contains("doctor --verify-links") && msg.contains("source sync"),
		"the message must list every command that DOES accept `all`: {msg}"
	);
	assert!(
		!msg.contains("comma-separated list"),
		"it must not suggest a list this command also rejects: {msg}"
	);

	// B10: `source diff --online` must parse. It exists only because
	// `check --online` does, so a caller reasonably tries it here; clap's
	// exit-2 "to pass '--online' as a value" tip reads like a quoting problem.
	let diff_online = isolated_cli(home.path(), state.path())
		.args(["source", "diff", "owner/repo", "--online"])
		.output()
		.unwrap();
	assert_ne!(
		diff_online.status.code(),
		Some(2),
		"--online must not be a clap usage error: {}",
		String::from_utf8_lossy(&diff_online.stderr)
	);

	// B9: stdin is read ONLY for `--api-key -`. It used to be read whenever
	// stdin was not a tty, which blocked to EOF on the open, idle pipe a
	// non-interactive harness leaves behind — no prompt, no output, no
	// diagnostic. (assert_cmd cannot hold a pipe open without writing, so the
	// hang itself is not reproducible here; what IS pinned is the contract that
	// replaced it: no `-`, no stdin read.)
	let add_args = [
		"--json",
		"inference",
		"add",
		"--latin-name",
		"pp",
		"--display-name",
		"PP",
		"--format",
		"anthropic",
		"--api-base-url",
		"https://example.invalid/v1",
	];

	// Without `-`, a key on stdin must be IGNORED, and the run must fail fast
	// with the actionable missing-key error rather than silently consuming it.
	let ignored = isolated_cli(home.path(), state.path())
		.env("AGHUB_DATA_DIR", state.path().join("data"))
		.args(add_args)
		.write_stdin("would-have-been-swallowed\n")
		.output()
		.unwrap();
	assert!(
		!ignored.status.success(),
		"stdin must not be consulted without `--api-key -`: stdout={}",
		String::from_utf8_lossy(&ignored.stdout)
	);
	let combined = format!(
		"{}{}",
		String::from_utf8_lossy(&ignored.stdout),
		String::from_utf8_lossy(&ignored.stderr)
	);
	assert!(
		combined.contains("api key"),
		"the failure must name the missing key and how to supply it: \
		 {combined}"
	);
}

/// GOLDEN CONTRACT: the top-level `--json` key set of every command an agent
/// parses, pinned exactly.
///
/// This is the test the audit found missing, and the reason the wire shape could
/// drift freely: the only key-level assertions that existed pinned individual
/// commands in OPPOSITE directions (`cli_tests` requires `delete` to use
/// `dry_run` and to NOT contain `dryRun`, while requiring `prune-lock` to use
/// `dryRun`), so the divergence was locked in from both sides and nothing
/// watched the surface as a whole.
///
/// The casing split is DELIBERATE and documented in root `--help`: each command
/// mirrors the API/desktop DTO it shares a wire shape with, so unifying it means
/// changing those DTOs and the frontend, not the CLI. What must not happen is
/// silent drift — a key renamed, added or dropped without anyone deciding to.
///
/// When you intentionally change a payload, update the expectation here in the
/// same commit. If that feels annoying, that is the point: it is a wire
/// contract that agents and the desktop both parse.
#[cfg(unix)]
#[test]
fn json_payload_key_sets_are_pinned() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	let run = |args: &[&str]| -> std::process::Output {
		isolated_cli(home.path(), state.path())
			.current_dir(project.path())
			.args(args)
			.output()
			.unwrap()
	};

	// Seed one skill and one MCP so the list/describe payloads are non-empty.
	assert!(run(&["-p", "add", "skills", "--name", "pinned", "-d", "d"])
		.status
		.success());
	assert!(
		run(&["-p", "add", "mcps", "-n", "pinned-mcp", "-c", "echo"])
			.status
			.success()
	);

	/// Sorted top-level keys of a JSON object, or of the FIRST element when the
	/// payload is an array (every array payload here is homogeneous).
	fn keys(stdout: &[u8]) -> Vec<String> {
		let json: Value = serde_json::from_slice(stdout).unwrap_or_else(|e| {
			panic!(
				"payload must be JSON: {e}\n{}",
				String::from_utf8_lossy(stdout)
			)
		});
		let obj = match &json {
			Value::Array(items) => match items.first() {
				Some(first) => first.clone(),
				// An empty array pins nothing; the cases below all produce rows.
				None => panic!("payload array was empty"),
			},
			other => other.clone(),
		};
		let mut ks: Vec<String> = obj
			.as_object()
			.expect("payload must be an object (or array of objects)")
			.keys()
			.cloned()
			.collect();
		ks.sort();
		ks
	}

	let cases: &[(&[&str], &[&str])] = &[
		(
			&["-p", "--json", "get", "skills"],
			&[
				"already_installed",
				"author",
				"canonical_path",
				"description",
				"enabled",
				"name",
				"native_reader",
				"source_path",
				"tools",
				"version",
			],
		),
		(
			&["-p", "--json", "describe", "skills", "pinned"],
			&[
				"already_installed",
				"author",
				"canonical_path",
				"description",
				"enabled",
				"name",
				"native_reader",
				"source_path",
				"tools",
				"version",
			],
		),
		(
			// snake_case + `outcome` + `would_prune_lock_entries`.
			//
			// A preview now DISCLOSES the scope-wide lock prune a committed
			// delete performs, under its own key — it used to be invisible
			// until after the fact, so the caller could not see which OTHER
			// skills' provenance the commit was about to discard. The committed
			// counterpart is `pruned_lock_entries`, pinned separately below;
			// two keys, because a preview must not claim entries were dropped.
			&["-p", "--json", "delete", "skills", "pinned"],
			&[
				"deleted_path",
				"dry_run",
				"executed",
				"name",
				"needs_confirm",
				"outcome",
				"paths",
				"skipped",
				"success",
				"type",
				"would_prune_lock_entries",
			],
		),
		(
			// camelCase — mirrors the api `PruneLockResponse` the desktop reads.
			&["-p", "--json", "prune-lock"],
			&["dryRun", "pruned"],
		),
		(
			// snake_case, and every row must be self-describing about agent.
			&["-p", "--json", "coverage"],
			&[
				"auto_covered",
				"id",
				"needs_link",
				"reads_master",
				"scope",
				"supported",
				"writes_master",
			],
		),
		(
			// A failure payload: one shape for every command.
			&["-p", "--json", "describe", "skills", "no-such-skill"],
			&["error"],
		),
	];

	for (args, expected) in cases {
		let out = run(args);
		let got = keys(&out.stdout);
		assert_eq!(
			got,
			expected.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
			"{args:?} top-level keys drifted"
		);
	}

	// The COMMITTED delete shape: same keys plus `pruned_lock_entries`, which
	// only a real removal emits (see the preview note above).
	assert!(run(&["-p", "add", "skills", "--name", "doomed", "-d", "d"])
		.status
		.success());
	let committed =
		run(&["-p", "--json", "delete", "skills", "doomed", "--yes"]);
	assert!(committed.status.success());
	assert_eq!(
		keys(&committed.stdout),
		[
			"deleted_path",
			"dry_run",
			"executed",
			"name",
			"needs_confirm",
			"outcome",
			"paths",
			"pruned_lock_entries",
			"skipped",
			"success",
			"type",
		]
		.iter()
		.map(|k| k.to_string())
		.collect::<Vec<_>>(),
		"a committed delete additionally reports the lock keys it pruned"
	);

	// The failure object's own keys are part of the contract.
	let failure = run(&["-p", "--json", "describe", "skills", "no-such-skill"]);
	let json: Value = serde_json::from_slice(&failure.stdout).unwrap();
	let mut err_keys: Vec<&str> = json["error"]
		.as_object()
		.unwrap()
		.keys()
		.map(|k| k.as_str())
		.collect();
	err_keys.sort();
	assert_eq!(err_keys, vec!["code", "message", "retryable"]);

	// The multi-agent envelope, and the row carrying BOTH success predicates.
	let batch = run(&[
		"-p",
		"-a",
		"claude,codex",
		"--json",
		"add",
		"skills",
		"--name",
		"fanned",
		"-d",
		"d",
	]);
	assert!(
		batch.status.success(),
		"fan-out must succeed: {}",
		String::from_utf8_lossy(&batch.stderr)
	);
	let env: Value = serde_json::from_slice(&batch.stdout).unwrap();
	let mut env_keys: Vec<&str> = env
		.as_object()
		.unwrap()
		.keys()
		.map(|k| k.as_str())
		.collect();
	env_keys.sort();
	assert_eq!(env_keys, vec!["failed_count", "results", "success_count"]);
	let row = &env["results"][0];
	assert!(row["agent"].is_string(), "every row names its agent: {row}");
	assert!(row["ok"].is_boolean(), "row must carry `ok`: {row}");

	// transfer/reconcile share the envelope but a DIFFERENT row struct, whose
	// success predicate was `success` only — a parser written against `ok`
	// scored every successful row as a failure. Both names now appear.
	let reconciled = run(&[
		"-p",
		"--json",
		"reconcile",
		"skill",
		"--from-agent",
		"claude",
		"--name",
		"fanned",
		"--add",
		"opencode",
	]);
	assert!(
		reconciled.status.success(),
		"reconcile must succeed: {}",
		String::from_utf8_lossy(&reconciled.stderr)
	);
	let rv: Value = serde_json::from_slice(&reconciled.stdout).unwrap();
	let rrow = &rv["results"][0];
	assert_eq!(
		rrow["ok"], rrow["success"],
		"both success predicates must be present and agree: {rrow}"
	);
}

/// Regressions found by an independent adversarial review of the fixes above,
/// each one a case the original round missed.
///
/// Grouped deliberately: they are one-assertion checks over four different
/// commands, and the point of the test is the SET — every one of these was a
/// place where a fix was believed complete and was not.
#[cfg(unix)]
#[test]
fn review_found_gaps_stay_fixed() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	// --- `check` / `prune-lock` answer from the LOCK, so a broken agent config
	// must not block them. Tightening `tolerate_missing` to only absorb
	// NotFound (so a corrupt config stops reading as an absent one) made a
	// malformed `.mcp.json` fail both — commands that neither read nor write
	// it. Fixed by dispatching them before the ConfigManager exists, the same
	// way `source`/`doctor`/`coverage` already were.
	let mcp_path = project.path().join(".mcp.json");
	std::fs::write(&mcp_path, r#"{ "mcpServers": {} } OOPS"#).unwrap();

	for args in [
		["-p", "--json", "check", "skills"].as_slice(),
		["-p", "--json", "prune-lock"].as_slice(),
	] {
		let out = isolated_cli(home.path(), state.path())
			.current_dir(project.path())
			.args(args)
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"{args:?} reads only the lock; a malformed agent config must not \
			 block it: {}",
			String::from_utf8_lossy(&out.stderr)
		);
	}

	// ...while the commands that DO read that config still fail loudly.
	let reads_config = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "--json", "get", "mcps"])
		.output()
		.unwrap();
	assert!(
		!reads_config.status.success(),
		"a malformed config must still fail the commands that read it"
	);
	// And with a typed code, not the `CLI_ERROR` fallback — the load error used
	// to be stringified through `anyhow!("… {}", e)`, which erased the type the
	// shared code vocabulary is derived from.
	let cj: Value = serde_json::from_slice(&reads_config.stdout).unwrap();
	assert_eq!(cj["error"]["code"], "INVALID_CONFIG", "{cj}");
	// The message must be self-contained: `to_string()` gave only the outermost
	// context ("Failed to load config"), stranding the actual cause in the
	// `Caused by:` block that only stderr receives.
	let msg = cj["error"]["message"].as_str().unwrap();
	assert!(
		msg.contains("parse"),
		"the JSON message must carry the whole cause chain: {msg}"
	);

	// --- `plugin` ignores the scope flags (root --help says so outright), so
	// the now-unconditional project-root guard must not reach it. It did: `-p
	// plugin list` failed with "no project root found" for a command that never
	// wanted a scope.
	let nowhere = tempfile::TempDir::new().unwrap();
	for scope in [["-p"].as_slice(), ["-g"].as_slice(), [].as_slice()] {
		let mut argv = scope.to_vec();
		argv.extend_from_slice(&["--json", "plugin", "list"]);
		let out = isolated_cli(home.path(), state.path())
			.current_dir(nowhere.path())
			.args(&argv)
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"plugin ignores scope flags, so {argv:?} must not fail: {}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
	// Claude-only is still enforced, and still names the fix.
	let wrong_agent = isolated_cli(home.path(), state.path())
		.current_dir(nowhere.path())
		.args(["-a", "codex", "--json", "plugin", "list"])
		.output()
		.unwrap();
	assert!(!wrong_agent.status.success());
	assert!(
		String::from_utf8_lossy(&wrong_agent.stderr).contains("-a claude"),
		"the refusal must name the fix"
	);

	// --- An already-ABSENT resource is `absent` even without `--yes`. It used
	// to report `preview`, whose contract promises that re-running with `--yes`
	// changes something — it does not, so that reading invites an endless
	// retry. `absent` outranks the caller's intent.
	// A CLEAN project: the malformed `.mcp.json` above belongs to the earlier
	// case, and `delete` legitimately fails on it (it reads that config).
	let clean = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(clean.path().join(".claude")).unwrap();
	let unconfirmed = isolated_cli(home.path(), state.path())
		.current_dir(clean.path())
		.args(["-p", "--json", "delete", "skills", "never-existed"])
		.output()
		.unwrap();
	assert!(unconfirmed.status.success());
	let uj: Value = serde_json::from_slice(&unconfirmed.stdout).unwrap();
	assert_eq!(
		uj["outcome"], "absent",
		"nothing to delete is `absent`, not a preview of a change: {uj}"
	);

	// --- A `reconcile` PREVIEW must run every read-only preflight the commit
	// would, not just the source-exists one: an agent in BOTH --add and
	// --remove was approved by the preview and refused by the commit.
	let seeded = isolated_cli(home.path(), state.path())
		.current_dir(clean.path())
		.args(["-p", "add", "skills", "--name", "both-sets", "-d", "d"])
		.output()
		.unwrap();
	assert!(seeded.status.success());
	let overlap = isolated_cli(home.path(), state.path())
		.current_dir(clean.path())
		.args([
			"-p",
			"--json",
			"reconcile",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"both-sets",
			"--add",
			"opencode",
			"--remove",
			"opencode",
		])
		.output()
		.unwrap();
	assert!(
		!overlap.status.success(),
		"the preview must refuse what the commit refuses: stdout={}",
		String::from_utf8_lossy(&overlap.stdout)
	);
	assert!(
		String::from_utf8_lossy(&overlap.stderr).contains("both"),
		"the error must say the agent is in both sets: {}",
		String::from_utf8_lossy(&overlap.stderr)
	);
}

/// Any `--json` command that can PARTIALLY fail must leave exactly ONE JSON
/// document on stdout.
///
/// A behavioural check, deliberately not a hand-maintained list of bail sites:
/// the first round enumerated those by hand and missed `source sync`, whose
/// action-error path printed the full outcome view and then let the global
/// failure renderer append a second document. Two concatenated documents fail
/// every parser.
#[cfg(unix)]
#[test]
fn partial_failure_leaves_exactly_one_json_document_on_stdout() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "shared", "shared");

	let seeded = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "skills", "--name", "conflicted", "-d", "d"])
		.output()
		.unwrap();
	assert!(seeded.status.success());

	// Each case is a command that reports per-row verdicts in its payload and
	// then returns Err purely to set the exit code.
	let cases: &[(&str, Vec<&str>)] = &[
		(
			// transfer into an agent that already has it: a failed row inside a
			// successful-shaped envelope.
			"transfer",
			vec![
				"-p",
				"--json",
				"transfer",
				"skill",
				"--from-agent",
				"claude",
				"--name",
				"conflicted",
				"--to",
				"claude",
			],
		),
		(
			// source sync with a source that cannot be reached: the path the
			// hand-maintained list missed.
			"source sync",
			vec![
				"-p",
				"--json",
				"source",
				"sync",
				"owner/repo",
				"--install-missing",
				"--yes",
			],
		),
	];

	for (label, argv) in cases {
		let out = isolated_cli(home.path(), state.path())
			.current_dir(project.path())
			.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
			.args(argv)
			.output()
			.unwrap();
		let stdout = String::from_utf8_lossy(&out.stdout);
		if stdout.trim().is_empty() {
			// Nothing printed is fine — the contract is about not printing
			// TWO documents.
			continue;
		}
		serde_json::from_str::<Value>(&stdout).unwrap_or_else(|e| {
			panic!(
				"{label}: stdout must hold exactly ONE JSON document, got \
				 {e}\n{stdout}"
			)
		});
	}
}

/// A `delete` PREVIEW discloses the scope-wide lock prune the commit would run.
///
/// A committed delete reconciles the whole scope's lock against disk, dropping
/// entries for OTHER skills that are already gone — invisibly, until after the
/// fact. `prune-lock` gates the same GC behind its own `--yes`; `delete` did it
/// as an undisclosed side effect, so the preview could not tell you whose
/// provenance was about to be discarded.
#[cfg(unix)]
#[test]
fn delete_preview_discloses_the_lock_entries_it_would_prune() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();

	// Two skills from a source, so both get real lock entries.
	write_source_skill(src.path(), "keeper", "keeper");
	write_source_skill(src.path(), "ghosted", "ghosted");
	for name in ["keeper", "ghosted"] {
		let out = run_sync_install(
			home.path(),
			state.path(),
			src.path(),
			"claude",
			name,
		);
		assert!(
			out.status.success(),
			"install {name} must succeed: {}",
			String::from_utf8_lossy(&out.stderr)
		);
	}

	// `ghosted` disappears from disk WITHOUT going through aghub — the everyday
	// way an orphan lock entry appears.
	std::fs::remove_dir_all(home.path().join(".agents/skills/ghosted"))
		.unwrap();
	std::fs::remove_dir_all(home.path().join(".claude/skills/ghosted")).ok();

	let preview = isolated_cli(home.path(), state.path())
		.args(["-g", "--json", "delete", "skills", "keeper"])
		.output()
		.unwrap();
	assert!(
		preview.status.success(),
		"preview must succeed: {}",
		String::from_utf8_lossy(&preview.stderr)
	);
	let pj: Value = serde_json::from_slice(&preview.stdout).unwrap();
	assert_eq!(pj["outcome"], "preview", "{pj}");

	let would: Vec<&str> = pj["would_prune_lock_entries"]
		.as_array()
		.unwrap_or_else(|| panic!("preview must disclose the prune: {pj}"))
		.iter()
		.map(|v| v.as_str().unwrap())
		.collect();
	assert!(
		would.contains(&"ghosted"),
		"the OTHER skill's orphaned entry is what the caller cannot otherwise \
		 see: {pj}"
	);
	// And NOT the target's own key: this single-agent delete removes the agent
	// link but KEEPS the shared `.agents/skills` Master (see `skipped`), so the
	// skill is still on disk and its lock entry survives. This is why exclusion
	// is by PATH and not by folder name — excluding the name would have
	// promised to drop an entry the commit keeps.
	assert!(
		!would.contains(&"keeper"),
		"the Master is kept, so the target's own entry must NOT be listed: {pj}"
	);

	// With `--all-agents` the Master goes too, so now the target's own key IS
	// certain to be dropped — and a naive preview would omit it, because the
	// folder is still on disk when the preview runs.
	let all_agents = isolated_cli(home.path(), state.path())
		.args(["-g", "--json", "delete", "skills", "keeper", "--all-agents"])
		.output()
		.unwrap();
	assert!(
		all_agents.status.success(),
		"all-agents preview must succeed: {}",
		String::from_utf8_lossy(&all_agents.stderr)
	);
	let aj: Value = serde_json::from_slice(&all_agents.stdout).unwrap();
	let would_all: Vec<&str> = aj["would_prune_lock_entries"]
		.as_array()
		.unwrap_or_else(|| panic!("preview must disclose the prune: {aj}"))
		.iter()
		.map(|v| v.as_str().unwrap())
		.collect();
	assert!(
		would_all.contains(&"keeper"),
		"an all-agents delete takes the Master too, so the target's own key \
		 is certain to be dropped: {aj}"
	);

	// A preview writes NOTHING.
	assert!(
		home.path().join(".agents/skills/keeper").exists(),
		"a preview must not delete"
	);
	let after_preview = isolated_cli(home.path(), state.path())
		.args(["-g", "--json", "source", "list"])
		.output()
		.unwrap();
	assert!(after_preview.status.success());

	// The commit reports the same keys under the COMMITTED key, never the
	// preview one.
	let committed = isolated_cli(home.path(), state.path())
		.args(["-g", "--json", "delete", "skills", "keeper", "--yes"])
		.output()
		.unwrap();
	assert!(committed.status.success());
	let cj: Value = serde_json::from_slice(&committed.stdout).unwrap();
	assert_eq!(cj["outcome"], "removed", "{cj}");
	assert!(
		cj["would_prune_lock_entries"].is_null(),
		"a COMMIT must not use the preview key: {cj}"
	);
	let pruned: Vec<&str> = cj["pruned_lock_entries"]
		.as_array()
		.expect("commit reports what it pruned")
		.iter()
		.map(|v| v.as_str().unwrap())
		.collect();
	assert!(pruned.contains(&"ghosted"), "{cj}");
}

/// A KEPT shared Master must not promise a prune the commit will never run.
///
/// Found by cross-checking two independently-written plans that each looked
/// correct alone: one classified this state `kept`, the other attached
/// `would_prune_lock_entries` to the same preview. Together they describe a
/// commit that cannot happen — re-running with `--yes` hits
/// `unsupported_operation` and prunes nothing, so the disclosure would be the
/// exact never-terminating hint `absent` and `kept` were introduced to kill.
#[cfg(unix)]
#[test]
fn a_kept_shared_master_preview_promises_no_prune() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "shared-one", "shared-one");

	// Install for an agent that NEEDS a link, then for one that reads the
	// Master directly — so removing it from the Master-reader alone cannot
	// express anything.
	for agent in ["claude", "cursor"] {
		let out = run_sync_install(
			home.path(),
			state.path(),
			src.path(),
			agent,
			"shared-one",
		);
		assert!(
			out.status.success(),
			"install for {agent} must succeed: {}",
			String::from_utf8_lossy(&out.stderr)
		);
	}

	// cursor reads `.agents/skills` directly: there is no cursor-only artifact
	// to remove, so this delete keeps the Master.
	let preview = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"cursor",
			"--json",
			"delete",
			"skills",
			"shared-one",
		])
		.output()
		.unwrap();
	assert!(
		preview.status.success(),
		"preview must succeed: {}",
		String::from_utf8_lossy(&preview.stderr)
	);
	let pj: Value = serde_json::from_slice(&preview.stdout).unwrap();

	// Only assert the pairing when this really is the kept state — if the
	// fixture stops producing it, say so instead of passing vacuously.
	assert_eq!(
		pj["outcome"], "kept",
		"fixture must produce the shared-master-kept state, or this test \
		 proves nothing: {pj}"
	);
	assert!(
		pj["would_prune_lock_entries"].is_null(),
		"a commit that will REFUSE must not be previewed as one that prunes: \
		 {pj}"
	);
}

/// `check`'s default spans BOTH scopes, like the other read-only diagnostics.
///
/// It used to follow the plain global default, so run inside a project it
/// answered from the global lock alone — reporting that the project's skills
/// needed no update, because it never looked at them. Nothing tested that
/// default, in either direction.
///
/// The fixture installs through `source sync`, not `add skills`: only the
/// former writes a lock entry, and `check` reports from the LOCK. Seeding with
/// `add` would make this pass or fail for the wrong reason.
#[cfg(unix)]
#[test]
fn check_defaults_to_both_scopes() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "global-one", "global-one");
	write_source_skill(src.path(), "project-one", "project-one");

	// One skill in each scope.
	let g = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"global-one",
	);
	assert!(g.status.success(), "{}", String::from_utf8_lossy(&g.stderr));
	let p = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-p",
			"-a",
			"claude",
			"source",
			"sync",
			"owner/repo",
			"--skill",
			"project-one",
			"--install-missing",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(p.status.success(), "{}", String::from_utf8_lossy(&p.stderr));

	let names = |args: &[&str]| -> Vec<String> {
		let out = isolated_cli(home.path(), state.path())
			.current_dir(project.path())
			.args(args)
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"{args:?}: {}",
			String::from_utf8_lossy(&out.stderr)
		);
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		let mut n: Vec<String> = json
			.as_array()
			.unwrap()
			.iter()
			.map(|r| r["name"].as_str().unwrap().to_string())
			.collect();
		n.sort();
		n
	};

	// The DEFAULT sees both.
	assert_eq!(
		names(&["--json", "check", "skills"]),
		vec!["global-one".to_string(), "project-one".to_string()],
		"the no-flag default must span both scopes, like doctor and source list"
	);

	// Explicit flags are untouched.
	assert_eq!(
		names(&["-g", "--json", "check", "skills"]),
		vec!["global-one".to_string()]
	);
	assert_eq!(
		names(&["-p", "--json", "check", "skills"]),
		vec!["project-one".to_string()]
	);

	// And with no project root the default degrades to global-only rather than
	// failing — an implicit default never asks for ProjectOnly, so the
	// unconditional project-root guard must not fire.
	let nowhere = tempfile::TempDir::new().unwrap();
	let out = isolated_cli(home.path(), state.path())
		.current_dir(nowhere.path())
		.args(["--json", "check", "skills"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"the default must not require a project root: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert!(
		json.as_array()
			.unwrap()
			.iter()
			.all(|r| r["scope"] == "global"),
		"outside a project the default is global-only: {json}"
	);
}

/// `doctor` distinguishes an orphan master from a missing link, and can gate.
///
/// Two defects, one fixture. (1) `delete --yes` deliberately KEEPS
/// `.agents/skills/<name>` when another agent still reads it, and doctor
/// classified that leftover as a repairable referrer issue whose remediation
/// was `source sync --skill <name> --install-missing` — reinstalling the skill
/// the user had just deleted, while a second note two lines up said to delete
/// it. (2) `linkAudit.state` reported `verified` while its own agent rows said
/// `missing`, and the exit code was 0 either way, so
/// `doctor --verify-links && echo healthy` printed healthy over a broken tree.
#[cfg(unix)]
#[test]
fn doctor_separates_orphan_masters_and_can_gate_on_issues() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	// A master with NO lock entry and no agent slot: exactly what a manual
	// `.agents/skills/<name>` or a kept-master delete leaves behind.
	let master = home.path().join(".agents/skills/orphaned");
	std::fs::create_dir_all(&master).unwrap();
	std::fs::write(
		master.join("SKILL.md"),
		"---\nname: orphaned\ndescription: left behind\n---\n\nbody\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "--json", "doctor", "--verify-links"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"the default exit code must be unchanged: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let row = json
		.as_array()
		.unwrap()
		.iter()
		.find(|r| r["skill"] == "orphaned")
		.expect("the orphaned master must be reported");

	assert_eq!(
		row["linkAudit"]["state"], "issues",
		"the summary must not say `verified` while its own rows do not: {row}"
	);
	let claude = row["linkAudit"]["agents"]
		.as_array()
		.unwrap()
		.iter()
		.find(|a| a["agent"] == "claude")
		.expect("claude row present");
	assert_eq!(
		claude["state"], "orphanMaster",
		"an untracked master with no slot is a LEFTOVER, not a missing link — \
		 they have opposite remedies: {row}"
	);

	// The remediation notes are the HUMAN output — the `--json` path returns
	// before they are printed, so ask for them without it.
	let human = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "claude", "doctor", "--verify-links"])
		.output()
		.unwrap();
	assert!(human.status.success());
	let stderr = String::from_utf8_lossy(&human.stderr);
	assert!(
		stderr.contains("orphan master"),
		"orphan masters need their own note: {stderr}"
	);
	let orphan_note = stderr
		.lines()
		.find(|l| l.contains("orphan master"))
		.unwrap();
	assert!(
		!orphan_note.contains("--install-missing"),
		"the orphan-master note must not point at reinstalling it: \
		 {orphan_note}"
	);
	// And it must hedge: an untracked master can still have a live referrer
	// from another agent, so deleting it blindly dangles that link.
	assert!(
		orphan_note.contains("other agents"),
		"the note must warn that another agent may still link to it: \
		 {orphan_note}"
	);

	// Opt-in gating.
	let gated = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"-a",
			"claude",
			"--json",
			"doctor",
			"--verify-links",
			"--fail-on-issues",
		])
		.output()
		.unwrap();
	assert!(
		!gated.status.success(),
		"--fail-on-issues must exit non-zero when there are issues"
	);
	// stdout must still hold exactly ONE JSON document — the report — not the
	// report plus a failure document.
	serde_json::from_slice::<Value>(&gated.stdout).expect(
		"the report is the answer; a second error document would break every \
		 parser",
	);

	// A clean tree gates green.
	let clean_home = tempfile::TempDir::new().unwrap();
	let clean_state = tempfile::TempDir::new().unwrap();
	let clean = isolated_cli(clean_home.path(), clean_state.path())
		.args([
			"-g",
			"-a",
			"claude",
			"--json",
			"doctor",
			"--verify-links",
			"--fail-on-issues",
		])
		.output()
		.unwrap();
	assert!(
		clean.status.success(),
		"--fail-on-issues must not fire on a clean tree: {}",
		String::from_utf8_lossy(&clean.stderr)
	);
}

/// An agent holding an UNRELATED skill of the same name is a conflict, and a
/// paired `--remove` must not run.
///
/// The data-loss path this pins: making `transfer` idempotent moved the
/// already-present decision into `add_skill_from_path`, whose NativeReader
/// branch returned on the NAME alone. A Master-reading agent's read paths
/// include its OWN private dir, so an unrelated `<own-dir>/<name>` matched, the
/// copy reported success having written nothing, and
/// `run_staged_multi_target_mutation`'s gate — which only asks whether the copy
/// ERRORED — let the paired delete proceed. Source content gone, exit 0,
/// "2 succeeded, 0 failed".
#[cfg(unix)]
#[test]
fn reconcile_does_not_delete_the_source_when_the_target_holds_a_different_skill(
) {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	// claude's skill, and an UNRELATED cursor skill that merely shares the
	// name — a real directory, not a link to any Master.
	let claude_skill = home.path().join(".claude/skills/collide");
	std::fs::create_dir_all(&claude_skill).unwrap();
	std::fs::write(
		claude_skill.join("SKILL.md"),
		"---\nname: collide\ndescription: the one that matters\n---\n\nKEEP-ME\n",
	)
	.unwrap();
	let cursor_skill = home.path().join(".cursor/skills/collide");
	std::fs::create_dir_all(&cursor_skill).unwrap();
	std::fs::write(
		cursor_skill.join("SKILL.md"),
		"---\nname: collide\ndescription: unrelated\n---\n\nCURSOR-PRIVATE\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"--json",
			"reconcile",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"collide",
			"--add",
			"cursor",
			"--remove",
			"claude",
			"--yes",
		])
		.output()
		.unwrap();

	// THE assertion. Everything else is diagnosis.
	let survived = std::fs::read_to_string(claude_skill.join("SKILL.md"))
		.expect("the source skill must still exist");
	assert!(
		survived.contains("KEEP-ME"),
		"the source content must survive a copy that wrote nothing: {survived}"
	);

	assert!(
		!out.status.success(),
		"a copy that could not happen must not report success: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let copy = json["results"]
		.as_array()
		.unwrap()
		.iter()
		.find(|r| r["action"] == "copy")
		.expect("copy row present");
	assert_eq!(copy["success"], false, "{copy}");
	assert_eq!(
		copy["already_present"], false,
		"an unrelated same-named skill is NOT `already_present`: {copy}"
	);

	// The target keeps its own content — nothing was overwritten either.
	assert!(std::fs::read_to_string(cursor_skill.join("SKILL.md"))
		.unwrap()
		.contains("CURSOR-PRIVATE"));
}

/// Gaps found by two independent adversarial reviews of the six changes above.
/// Each was a place where a fix was believed complete and was not.
#[cfg(unix)]
#[test]
fn second_review_found_gaps_stay_fixed() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let project = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(project.path().join(".claude")).unwrap();

	// --- Two agents can share ONE backing file: Claude's project MCP config is
	// `.mcp.json`, and Copilot uses that same file when it exists. "Add to
	// copilot, remove from claude" is then not a state the world can be in —
	// and left unchecked it DESTROYED: the copy truthfully reported
	// `already_present` (same file!), the staged gate only asks whether the
	// copy errored, and the delete rewrote that one file without the entry.
	// Both rows reported success and the server was gone from both agents.
	let seeded = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "add", "mcps", "-n", "shared", "-c", "echo"])
		.output()
		.unwrap();
	assert!(seeded.status.success());

	let collide = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args([
			"-p",
			"--json",
			"reconcile",
			"mcp",
			"--from-agent",
			"claude",
			"--name",
			"shared",
			"--add",
			"copilot",
			"--remove",
			"claude",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(
		!collide.status.success(),
		"a reconcile that cannot be expressed must be refused: {}",
		String::from_utf8_lossy(&collide.stdout)
	);
	// THE assertion: the server survived.
	let still_there = isolated_cli(home.path(), state.path())
		.current_dir(project.path())
		.args(["-p", "-a", "copilot", "--json", "get", "mcps"])
		.output()
		.unwrap();
	let listed: Value = serde_json::from_slice(&still_there.stdout).unwrap();
	assert!(
		listed
			.as_array()
			.unwrap()
			.iter()
			.any(|m| m["name"] == "shared"),
		"the refused reconcile must not have deleted anything: {listed}"
	);

	// --- `--fail-on-issues` must count the `health` axis too. Derived from
	// `link_audit` alone it was inert without `--verify-links` — every row is
	// `notRequested` then — so it exited 0 over a lock entry whose master is
	// gone, the same false green the flag exists to remove.
	//
	// `orphan-lock`, NOT `untracked`: a hand-written skill with no lock entry is
	// a SUPPORTED layout (this repo is one), and gating on it made the flag
	// permanently red for anyone authoring in place. See `DoctorRow::is_issue`.
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "will-vanish", "will-vanish");
	let installed = run_sync_install(
		home.path(),
		state.path(),
		src.path(),
		"claude",
		"will-vanish",
	);
	assert!(
		installed.status.success(),
		"{}",
		String::from_utf8_lossy(&installed.stderr)
	);
	// The master disappears WITHOUT going through aghub: the lock entry is now
	// an orphan, which is genuinely actionable.
	std::fs::remove_dir_all(home.path().join(".agents/skills/will-vanish"))
		.unwrap();
	std::fs::remove_dir_all(home.path().join(".claude/skills/will-vanish"))
		.ok();

	let gated = isolated_cli(home.path(), state.path())
		.args(["-g", "--json", "doctor", "--fail-on-issues"])
		.output()
		.unwrap();
	assert!(
		!gated.status.success(),
		"an orphaned lock entry is an issue even without --verify-links: {}",
		String::from_utf8_lossy(&gated.stdout)
	);
	// The message must name the axis that actually ran — it used to say
	// "agent referrer issue(s)" while the link audit had never been requested.
	let stderr = String::from_utf8_lossy(&gated.stderr);
	assert!(
		stderr.contains("skill health"),
		"the failure must point at the axis that was checked: {stderr}"
	);

	// --- A `kept` delete must not tell the human to re-run with --yes: that
	// run fails with `Unsupported operation`. The JSON and the desktop were
	// fixed; the CLI's own human renderer still printed the dead-end hint.
	let src = tempfile::TempDir::new().unwrap();
	write_source_skill(src.path(), "shared-skill", "shared-skill");
	for agent in ["claude", "cursor"] {
		let out = run_sync_install(
			home.path(),
			state.path(),
			src.path(),
			agent,
			"shared-skill",
		);
		assert!(
			out.status.success(),
			"{}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
	let kept = isolated_cli(home.path(), state.path())
		.args(["-g", "-a", "cursor", "delete", "skills", "shared-skill"])
		.output()
		.unwrap();
	assert!(kept.status.success());
	let text = String::from_utf8_lossy(&kept.stdout);
	assert!(
		!text.contains("re-run with --yes"),
		"`--yes` fails for a kept master; pointing at it is a dead end: {text}"
	);
	assert!(
		text.contains("NOT removed") && text.contains("shared"),
		"it must say plainly that nothing was removed and why: {text}"
	);
}

/// The fixes for the two data-loss bugs only closed the shape each review
/// NAMED. These are the sibling shapes a third review found on top of them.
#[cfg(unix)]
#[test]
fn third_review_sibling_shapes_stay_fixed() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();

	// --- The worst one: the copy does not even claim `already_present`.
	//
	// `materialize_universal_master` preserves a pre-existing Master rather
	// than overwriting it. So a target with NO skill at all gets linked to an
	// existing Master holding DIFFERENT content: the call succeeds,
	// `already_present` is false — it really did install something — and not
	// one byte of the source was written. The paired delete then removed the
	// source. Strengthening `already_present`, or the staged gate, closes
	// neither: the copy did not lie about being a no-op, it lied about having
	// carried the content over.
	let master = home.path().join(".agents/skills/collide2");
	std::fs::create_dir_all(&master).unwrap();
	std::fs::write(
		master.join("SKILL.md"),
		"---\nname: collide2\ndescription: master\n---\n\nMASTER-CONTENT\n",
	)
	.unwrap();
	let claude_skill = home.path().join(".claude/skills/collide2");
	std::fs::create_dir_all(&claude_skill).unwrap();
	std::fs::write(
		claude_skill.join("SKILL.md"),
		"---\nname: collide2\ndescription: claude\n---\n\nONLY-COPY-OF-THIS\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"--json",
			"reconcile",
			"skill",
			"--from-agent",
			"claude",
			"--name",
			"collide2",
			// gemini has nothing at all — this is the `already_present: false`
			// shape.
			"--add",
			"gemini",
			"--remove",
			"claude",
			"--yes",
		])
		.output()
		.unwrap();

	// THE assertion.
	let survived = std::fs::read_to_string(claude_skill.join("SKILL.md"))
		.expect("the source must still exist");
	assert!(
		survived.contains("ONLY-COPY-OF-THIS"),
		"a copy that linked the target to a DIFFERENT master must not let the \
		 source be deleted: {survived}"
	);
	assert!(!out.status.success());
	assert!(
		String::from_utf8_lossy(&out.stdout)
			.contains("did not carry the source content over")
			|| String::from_utf8_lossy(&out.stderr)
				.contains("did not carry the source content over"),
		"the error must say WHY: stdout={} stderr={}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	);

	// --- `--fail-on-issues` must not fire on layouts that are working as
	// designed. A hand-written skill in `.agents/skills` with no lock entry is
	// `untracked` — this very repo is that layout — and gating on it made the
	// flag permanently red for anyone authoring skills in place.
	let handwritten = home.path().join(".agents/skills/authored-here");
	std::fs::create_dir_all(&handwritten).unwrap();
	std::fs::write(
		handwritten.join("SKILL.md"),
		"---\nname: authored-here\ndescription: written in place\n---\n\nx\n",
	)
	.unwrap();
	// (collide2 above is also untracked; both must be tolerated.)
	let tolerated = isolated_cli(home.path(), state.path())
		.args(["-g", "--json", "doctor", "--fail-on-issues"])
		.output()
		.unwrap();
	assert!(
		tolerated.status.success(),
		"an untracked, hand-written skill is a supported layout, not a CI \
		 failure: {}",
		String::from_utf8_lossy(&tolerated.stderr)
	);

	// --- A DANGLING master symlink must not be certified. `master_state` uses
	// `symlink_metadata`, so a broken link still reports `link`; accepting
	// anything that is not `Missing` certified a tree where `get skills`
	// returns [] as `autoCovered` / `verified`.
	let target_dir = tempfile::TempDir::new().unwrap();
	let live = target_dir.path().join("live");
	std::fs::create_dir_all(&live).unwrap();
	std::fs::write(
		live.join("SKILL.md"),
		"---\nname: live\ndescription: d\n---\n\nx\n",
	)
	.unwrap();
	let link = home.path().join(".agents/skills/live");
	std::os::unix::fs::symlink(&live, &link).unwrap();

	let audit = |expect_state: &str| {
		let out = isolated_cli(home.path(), state.path())
			.args(["-g", "-a", "cursor", "--json", "doctor", "--verify-links"])
			.output()
			.unwrap();
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		let row = json
			.as_array()
			.unwrap()
			.iter()
			.find(|r| r["skill"] == "live")
			.unwrap_or_else(|| panic!("live row present: {json}"))
			.clone();
		assert_eq!(
			row["linkAudit"]["agents"][0]["state"], expect_state,
			"{row}"
		);
	};
	// A working symlink master is a SUPPORTED layout.
	audit("autoCovered");
	// A broken one is not covered, however it is spelled on disk.
	std::fs::remove_dir_all(&live).unwrap();
	audit("missing");
}

/// Fourth review. The two halves each earlier round left open.
///
/// Both are the same mistake in two places: a decision made on the skill's
/// NAME when only its resolved PATH (or content) answers the question.
#[cfg(unix)]
#[test]
fn fourth_review_name_only_decisions_stay_closed() {
	// --- (1) The content proof must not skip `already_installed`.
	//
	// The third round proved the copy landed the source content before a
	// paired `--remove` ran, but exempted `already_installed`. A NativeReader
	// that already reads a Master reports exactly that — truthfully, it does
	// hold a skill by that name — and the name is all the two share. The
	// exemption therefore reopened the hole for the one shape most likely to
	// hit it: a Master that already exists.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();

		// cursor reads `.agents/skills` directly, so this Master IS cursor's
		// `foo` — and it is not claude's `foo`.
		let master = home.path().join(".agents/skills/foo");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(
			master.join("SKILL.md"),
			"---\nname: foo\ndescription: master\n---\n\nMASTER-C\n",
		)
		.unwrap();
		let claude_skill = home.path().join(".claude/skills/foo");
		std::fs::create_dir_all(&claude_skill).unwrap();
		std::fs::write(
			claude_skill.join("SKILL.md"),
			"---\nname: foo\ndescription: claude\n---\n\nCLAUDE-A\n",
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"reconcile",
				"skill",
				"--from-agent",
				"claude",
				"--name",
				"foo",
				"--add",
				"cursor",
				"--remove",
				"claude",
				"--yes",
			])
			.output()
			.unwrap();

		// THE assertion.
		let survived = std::fs::read_to_string(claude_skill.join("SKILL.md"))
			.expect("the source skill must still exist");
		assert!(
			survived.contains("CLAUDE-A"),
			"the source must survive a copy that only found a same-NAMED \
			 master: {survived}"
		);
		assert!(
			!out.status.success(),
			"a copy that carried nothing over must not succeed: {}",
			String::from_utf8_lossy(&out.stdout)
		);
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		let copy = json["results"]
			.as_array()
			.unwrap()
			.iter()
			.find(|r| r["action"] == "copy")
			.expect("copy row present");
		assert_eq!(copy["success"], false, "{copy}");
		let delete = json["results"]
			.as_array()
			.unwrap()
			.iter()
			.find(|r| r["action"] == "delete")
			.expect("delete row present");
		assert_eq!(
			delete["success"], false,
			"the delete must be skipped, not merely survived: {delete}"
		);
		// And the Master is untouched — nothing was overwritten either way.
		assert!(std::fs::read_to_string(master.join("SKILL.md"))
			.unwrap()
			.contains("MASTER-C"));
	}

	// --- (2) The two universal install entry points must agree.
	//
	// `add --from <path>` verified the resolved path; plain `add -n <name>`
	// still returned on the name. For a NativeReader holding a FOREIGN
	// same-named skill that meant exit 0, `already_installed: true`, nothing
	// written, no master created — and a hint pointing at `update skills foo`,
	// which would have edited the foreign skill.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		let foreign = home.path().join(".cursor/skills/foo");
		std::fs::create_dir_all(&foreign).unwrap();
		std::fs::write(
			foreign.join("SKILL.md"),
			"---\nname: foo\ndescription: foreign\n---\n\nCURSOR-PRIVATE\n",
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"-a",
				"cursor",
				"add",
				"skills",
				"-n",
				"foo",
				"-d",
				"brand new",
			])
			.output()
			.unwrap();
		assert!(
			!out.status.success(),
			"a foreign same-named skill is a conflict, not a no-op: {}",
			String::from_utf8_lossy(&out.stdout)
		);
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		assert_eq!(json["error"]["code"], "RESOURCE_EXISTS", "{json}");
		assert!(
			!home.path().join(".agents/skills/foo").exists(),
			"the refused add must not leave a master behind"
		);
		assert!(std::fs::read_to_string(foreign.join("SKILL.md"))
			.unwrap()
			.contains("CURSOR-PRIVATE"));
	}

	// --- (3) …and the honest no-op still IS one: the same command against a
	// Master the agent really reads stays idempotent. Tightening (2) by
	// erroring on everything would pass (2) and break every re-add.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		let master = home.path().join(".agents/skills/foo");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(
			master.join("SKILL.md"),
			"---\nname: foo\ndescription: master\n---\n\nMASTER\n",
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"-a",
				"cursor",
				"add",
				"skills",
				"-n",
				"foo",
				"-d",
				"brand new",
			])
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"re-adding a skill the agent already reads must stay a no-op: {}",
			String::from_utf8_lossy(&out.stderr)
		);
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		assert_eq!(json["already_installed"], true, "{json}");
		// The Master is reported as it is on disk, not as it was requested.
		assert_eq!(json["description"], "master", "{json}");
	}
}

/// Two agents can resolve to ONE sub-agent file — and then a reconcile that
/// adds to one while removing from the other deletes it from both.
///
/// The reason the earlier rounds missed this is worth keeping: enumerating the
/// four descriptors that implement sub-agents shows four distinct directories,
/// which looks like a proof. It is not. `agents/src/sub_agents.rs` deliberately
/// refuses only symlinked LEAVES, so a symlinked ancestor (or an agent-home env
/// override) makes two of those "distinct" directories the same directory.
/// The copy then finds its own file, reports `already_present` truthfully, the
/// staged gate only asks whether the copy ERRORED, and the delete removes the
/// single file. exit 0, "2 succeeded", nothing left.
#[cfg(unix)]
#[test]
fn reconcile_sub_agent_refuses_when_both_targets_are_one_file() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let agents = home.path().join(".claude/agents");
	std::fs::create_dir_all(&agents).unwrap();
	// grok's home IS claude's home here.
	std::os::unix::fs::symlink(
		home.path().join(".claude"),
		home.path().join(".grok"),
	)
	.unwrap();
	let file = agents.join("reviewer.md");
	std::fs::write(
		&file,
		"---\nname: reviewer\ndescription: the only copy\n---\n\nREVIEWER-BODY\n",
	)
	.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g",
			"--json",
			"reconcile",
			"sub-agent",
			"--from-agent",
			"claude",
			"--name",
			"reviewer",
			"--add",
			"grok",
			"--remove",
			"claude",
			"--yes",
		])
		.output()
		.unwrap();

	// THE assertion.
	let survived = std::fs::read_to_string(&file)
		.expect("the one file both agents read must still exist");
	assert!(survived.contains("REVIEWER-BODY"), "{survived}");

	assert!(
		!out.status.success(),
		"a reconcile that would empty both agents must not report success: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	// Refused as a batch PREFLIGHT — no row ran, so there is nothing to undo.
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["error"]["code"], "INVALID_CONFIG", "{json}");

	// --- The preflight above is only a SNAPSHOT, and for sub-agents it barely
	// guards at all: the backing IS the resource file, so when the shared
	// directory does not yet hold it BOTH targets resolve to `None` and the
	// preflight sees no collision. One instruction later the copy has written
	// the file, and the delete removes it.
	//
	// The worst shape is the natural "move it" invocation, which also deletes
	// the pre-existing original: without the delete-time re-check this exits 0
	// with "3 succeeded, 0 failed" and `find $HOME` returns nothing at all.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(home.path().join(".claude/agents")).unwrap();
		std::fs::create_dir_all(home.path().join(".codex/agents")).unwrap();
		// grok's home IS claude's home — but neither holds `reviewer` yet.
		std::os::unix::fs::symlink(
			home.path().join(".claude"),
			home.path().join(".grok"),
		)
		.unwrap();
		let original = home.path().join(".codex/agents/reviewer.toml");
		std::fs::write(
			&original,
			"name = \"reviewer\"\ndescription = \"d\"\n\
			 developer_instructions = \"ORIGINAL-BODY\"\n",
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"reconcile",
				"sub-agent",
				"--from-agent",
				"codex",
				"--name",
				"reviewer",
				"--add",
				"grok",
				"--remove",
				"codex",
				"--remove",
				"claude",
				"--yes",
			])
			.output()
			.unwrap();

		// THE assertion: the sub-agent still exists SOMEWHERE.
		let shared = home.path().join(".claude/agents/reviewer.md");
		assert!(
			shared.is_file() || original.is_file(),
			"the sub-agent must survive somewhere: stdout={}",
			String::from_utf8_lossy(&out.stdout)
		);
		// The removal that would have emptied the shared dir is the one refused.
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		let claude_delete = json["results"]
			.as_array()
			.unwrap()
			.iter()
			.find(|r| r["action"] == "delete" && r["agent"] == "claude")
			.expect("claude delete row present");
		assert_eq!(claude_delete["success"], false, "{claude_delete}");
		assert!(!out.status.success());
	}

	// The same reconcile between genuinely separate directories still works —
	// a guard that refused every sub-agent reconcile would also pass the
	// assertion above.
	let home2 = tempfile::TempDir::new().unwrap();
	let state2 = tempfile::TempDir::new().unwrap();
	std::fs::create_dir_all(home2.path().join(".claude/agents")).unwrap();
	std::fs::write(
		home2.path().join(".claude/agents/reviewer.md"),
		"---\nname: reviewer\ndescription: moves\n---\n\nBODY\n",
	)
	.unwrap();
	let moved = isolated_cli(home2.path(), state2.path())
		.args([
			"-g",
			"--json",
			"reconcile",
			"sub-agent",
			"--from-agent",
			"claude",
			"--name",
			"reviewer",
			"--add",
			"grok",
			"--remove",
			"claude",
			"--yes",
		])
		.output()
		.unwrap();
	assert!(
		moved.status.success(),
		"distinct dirs must still reconcile: {}",
		String::from_utf8_lossy(&moved.stdout)
	);
	assert!(home2.path().join(".grok/agents/reviewer.md").is_file());
	assert!(!home2.path().join(".claude/agents/reviewer.md").exists());
}

/// A removal must spare the SOURCE, not merely the copy targets.
///
/// The shared-backing guard compared copies against deletes and keyed
/// "is this the source?" on `AgentType` equality. Two agent IDS are one
/// directory whenever a home is symlinked or an agent-home env var is
/// redirected, so `--remove grok` with `~/.grok -> ~/.claude` deleted the
/// source's only copy while every row reported success — the source was never
/// named, so nothing compared it to anything.
///
/// One guard, all three resource kinds; each previously destroyed on its own
/// route.
#[cfg(unix)]
#[test]
fn a_removal_that_would_take_it_from_the_source_is_refused() {
	// --- sub-agent: remove-only reconcile, no copies at all.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(home.path().join(".claude/agents")).unwrap();
		std::os::unix::fs::symlink(
			home.path().join(".claude"),
			home.path().join(".grok"),
		)
		.unwrap();
		let file = home.path().join(".claude/agents/reviewer.md");
		std::fs::write(
			&file,
			"---\nname: reviewer\ndescription: d\n---\n\nONLY-COPY\n",
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"reconcile",
				"sub-agent",
				"--from-agent",
				"claude",
				"--name",
				"reviewer",
				"--remove",
				"grok",
				"--yes",
			])
			.output()
			.unwrap();
		assert!(
			std::fs::read_to_string(&file)
				.is_ok_and(|s| s.contains("ONLY-COPY")),
			"the source's only copy must survive"
		);
		assert!(!out.status.success());
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		assert_eq!(json["error"]["code"], "INVALID_CONFIG", "{json}");
	}

	// --- skill: the delete target's own skills DIR is the source's.
	//
	// Note what is NOT guarded here: several agents linking to one shared
	// `.agents/skills` Master is the normal supported state, and removing one
	// agent's link is exactly what `remove_skill_planned` does. The guard is
	// on the agent's own directory, not on the Master.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(home.path().join(".claude/skills/foo"))
			.unwrap();
		std::fs::create_dir_all(home.path().join(".agents/skills/foo"))
			.unwrap();
		std::os::unix::fs::symlink(
			home.path().join(".claude"),
			home.path().join(".gemini"),
		)
		.unwrap();
		let private = home.path().join(".claude/skills/foo/SKILL.md");
		std::fs::write(
			&private,
			"---\nname: foo\ndescription: A\n---\n\nCLAUDE-A\n",
		)
		.unwrap();
		std::fs::write(
			home.path().join(".agents/skills/foo/SKILL.md"),
			"---\nname: foo\ndescription: C\n---\n\nMASTER-C\n",
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"reconcile",
				"skill",
				"--from-agent",
				"claude",
				"--name",
				"foo",
				"--add",
				"cursor",
				"--remove",
				"gemini",
				"--yes",
			])
			.output()
			.unwrap();
		assert!(
			std::fs::read_to_string(&private)
				.is_ok_and(|s| s.contains("CLAUDE-A")),
			"the source's private skill must survive a reconcile that never \
			 named it"
		);
		assert!(!out.status.success());
	}

	// --- MCP: claude and copilot share the project `.mcp.json`.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		let project = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(project.path().join(".claude")).unwrap();
		let shared = project.path().join(".mcp.json");
		std::fs::write(&shared, r#"{"mcpServers":{"only":{"command":"x"}}}"#)
			.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.current_dir(project.path())
			.args([
				"-p",
				"--json",
				"reconcile",
				"mcp",
				"--from-agent",
				"claude",
				"--name",
				"only",
				"--remove",
				"copilot",
				"--yes",
			])
			.output()
			.unwrap();
		assert!(
			std::fs::read_to_string(&shared).unwrap().contains("only"),
			"the source's only MCP entry must survive"
		);
		assert!(!out.status.success());
	}

	// --- …and the ordinary move between separate directories still works.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(home.path().join(".claude/agents")).unwrap();
		std::fs::write(
			home.path().join(".claude/agents/reviewer.md"),
			"---\nname: reviewer\ndescription: d\n---\n\nBODY\n",
		)
		.unwrap();
		let moved = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"reconcile",
				"sub-agent",
				"--from-agent",
				"claude",
				"--name",
				"reviewer",
				"--add",
				"grok",
				"--remove",
				"claude",
				"--yes",
			])
			.output()
			.unwrap();
		assert!(
			moved.status.success(),
			"a move between separate dirs must still work: {}",
			String::from_utf8_lossy(&moved.stdout)
		);
	}
}

/// A `delete` where every path failed must not exit 0 saying success.
///
/// `RemovalKind::Partial`'s own doc says "Do not read this as success", and the
/// wire view hard-coded `success: true` two screens below it. `delete --yes` on
/// a directory it cannot remove printed `"success": true, "outcome": "partial"`,
/// exited 0, listed the failed path under "kept (shared with other agents)",
/// and left the skill entirely in place.
#[cfg(unix)]
#[test]
fn a_delete_that_removed_nothing_does_not_report_success() {
	use std::os::unix::fs::PermissionsExt;

	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill = home.path().join(".claude/skills/foo");
	std::fs::create_dir_all(&skill).unwrap();
	std::fs::write(
		skill.join("SKILL.md"),
		"---\nname: foo\ndescription: d\n---\n\nBODY\n",
	)
	.unwrap();
	// Read+execute only: the entry cannot be unlinked from it.
	std::fs::set_permissions(&skill, std::fs::Permissions::from_mode(0o500))
		.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args([
			"-g", "-a", "claude", "--json", "delete", "skills", "foo", "--yes",
		])
		.output()
		.unwrap();

	// Restore before any assertion can abort the test and leak the mode.
	std::fs::set_permissions(&skill, std::fs::Permissions::from_mode(0o700))
		.unwrap();

	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["outcome"], "partial", "{json}");
	assert_eq!(json["success"], false, "{json}");
	assert!(
		!out.status.success(),
		"a delete that removed nothing must not exit 0: {json}"
	);
	assert!(skill.join("SKILL.md").is_file(), "the skill is still there");

	// Exactly ONE JSON document on stdout — the failure renderer must not
	// append a second one.
	assert_eq!(
		String::from_utf8_lossy(&out.stdout)
			.matches("\"outcome\"")
			.count(),
		1,
		"stdout={}",
		String::from_utf8_lossy(&out.stdout)
	);
}

/// Three ways the tool said something that was not so, all found by the same
/// question: does the report match what is on disk?
#[cfg(unix)]
#[test]
fn what_the_tool_reports_matches_what_is_on_disk() {
	// --- (1) A refused copy must leave nothing behind.
	//
	// The content-landing guard returns Err AFTER `add_skill_from_path` has
	// already linked the target to the master, so `failed_count: 2` plus
	// "nothing was removed" read as "no state changed" while the target had
	// gained a skill it never had, holding content nobody asked to copy.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(home.path().join(".claude/skills/foo"))
			.unwrap();
		std::fs::create_dir_all(home.path().join(".agents/skills/foo"))
			.unwrap();
		std::fs::write(
			home.path().join(".claude/skills/foo/SKILL.md"),
			"---\nname: foo\ndescription: A\n---\n\nCLAUDE-A\n",
		)
		.unwrap();
		std::fs::write(
			home.path().join(".agents/skills/foo/SKILL.md"),
			"---\nname: foo\ndescription: C\n---\n\nMASTER-C\n",
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"reconcile",
				"skill",
				"--from-agent",
				"claude",
				"--name",
				"foo",
				"--add",
				"gemini",
				"--remove",
				"claude",
				"--yes",
			])
			.output()
			.unwrap();
		assert!(!out.status.success());
		assert!(
			!home.path().join(".gemini/skills/foo").exists(),
			"a refused copy must not leave its referrer behind"
		);
		let sees = isolated_cli(home.path(), state.path())
			.args(["-g", "--json", "-a", "gemini", "get", "skills"])
			.output()
			.unwrap();
		let json: Value = serde_json::from_slice(&sees.stdout).unwrap();
		assert_eq!(json.as_array().unwrap().len(), 0, "{json}");
	}

	// --- (2) The preview must refuse what `--yes` refuses.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(home.path().join(".claude/agents")).unwrap();
		std::os::unix::fs::symlink(
			home.path().join(".claude"),
			home.path().join(".grok"),
		)
		.unwrap();
		std::fs::write(
			home.path().join(".claude/agents/reviewer.md"),
			"---\nname: reviewer\ndescription: d\n---\n\nB\n",
		)
		.unwrap();
		let preview = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"reconcile",
				"sub-agent",
				"--from-agent",
				"claude",
				"--name",
				"reviewer",
				"--add",
				"grok",
				"--remove",
				"claude",
			])
			.output()
			.unwrap();
		assert!(
			!preview.status.success(),
			"the preview green-lit a plan --yes refuses: {}",
			String::from_utf8_lossy(&preview.stdout)
		);
	}

	// --- (3) `add` must report the skill ON DISK, not the one it just parsed.
	//
	// The materializer preserves a pre-existing Master rather than overwriting
	// it, so re-adding an edited source for a second agent wrote nothing — and
	// the response echoed the SOURCE's description and version back, telling
	// the user their edit had landed. `add` said B while `get` said A.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		let src_a = home.path().join("srcA/shared");
		let src_b = home.path().join("srcB/shared");
		std::fs::create_dir_all(&src_a).unwrap();
		std::fs::create_dir_all(&src_b).unwrap();
		std::fs::write(
			src_a.join("SKILL.md"),
			"---\nname: shared\ndescription: FROM-A\nversion: A.0.0\n---\n\nA\n",
		)
		.unwrap();
		std::fs::write(
			src_b.join("SKILL.md"),
			"---\nname: shared\ndescription: FROM-B\nversion: B.0.0\n---\n\nB\n",
		)
		.unwrap();

		let first = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"-a",
				"cursor",
				"--json",
				"add",
				"skills",
				"--from",
				src_a.to_str().unwrap(),
			])
			.output()
			.unwrap();
		assert!(first.status.success());

		let second = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"-a",
				"claude",
				"--json",
				"add",
				"skills",
				"--from",
				src_b.to_str().unwrap(),
			])
			.output()
			.unwrap();
		assert!(second.status.success());
		let added: Value = serde_json::from_slice(&second.stdout).unwrap();
		assert_eq!(
			added["description"], "FROM-A",
			"add must report the master that is on disk, not the source it \
			 just parsed: {added}"
		);
		assert_eq!(added["version"], "A.0.0", "{added}");

		// …and `get`, seconds later, must give the same answer.
		let got = isolated_cli(home.path(), state.path())
			.args(["-g", "-a", "claude", "--json", "get", "skills"])
			.output()
			.unwrap();
		let rows: Value = serde_json::from_slice(&got.stdout).unwrap();
		assert_eq!(rows[0]["description"], added["description"], "{rows}");
	}
}

/// Round-5 delta: four places where a fix closed only the shape it was aimed at.
#[cfg(unix)]
#[test]
fn round_five_delta_fixes_stay_closed() {
	use std::os::unix::fs::PermissionsExt;

	// --- (1) The fan-out path must honour a nested `success: false` too.
	//
	// `RemovalKind::Partial` was taught to exit non-zero on the SINGLE-agent
	// path. The batch envelope only knows what its closure tells it, and the
	// closure said every `Ok(_)` was a success — so the same delete across a
	// comma list reported "2 succeeded, 0 failed" and exited 0.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		let skill = home.path().join(".claude/skills/foo");
		std::fs::create_dir_all(&skill).unwrap();
		std::fs::create_dir_all(home.path().join(".agents/skills/foo"))
			.unwrap();
		std::fs::write(
			skill.join("SKILL.md"),
			"---\nname: foo\ndescription: d\n---\n\nB\n",
		)
		.unwrap();
		std::fs::write(
			home.path().join(".agents/skills/foo/SKILL.md"),
			"---\nname: foo\ndescription: d\n---\n\nB\n",
		)
		.unwrap();
		std::fs::set_permissions(
			&skill,
			std::fs::Permissions::from_mode(0o500),
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"-a",
				"claude,cursor",
				"--json",
				"delete",
				"skills",
				"foo",
				"--yes",
			])
			.output()
			.unwrap();
		std::fs::set_permissions(
			&skill,
			std::fs::Permissions::from_mode(0o700),
		)
		.unwrap();

		assert!(
			!out.status.success(),
			"a fan-out delete that removed nothing must not exit 0: {}",
			String::from_utf8_lossy(&out.stdout)
		);
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		// Assert on the CLAUDE row specifically. `failed_count > 0` alone is a
		// false green: cursor reads the shared master, so its row fails on
		// every run whatever claude's row says.
		let claude = json["results"]
			.as_array()
			.unwrap()
			.iter()
			.find(|r| r["agent"] == "claude")
			.expect("claude row present");
		// The BATCH row's own flag (`ok`), not the nested payload's `success`
		// — the whole defect was `ok: true` wrapping `success: false`.
		assert_eq!(
			claude["ok"], false,
			"the partial removal must be a FAILED row: {claude}"
		);
		assert!(skill.join("SKILL.md").is_file());
	}

	// --- (2) The content proof must not certify what the hash cannot see.
	//
	// `skill::hash` skips symlinks by design (npx compatibility), so two trees
	// differing ONLY in a symlink hash EQUAL — and the removal that equality
	// authorised then destroyed the difference.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		let private = home.path().join(".claude/skills/foo");
		std::fs::create_dir_all(&private).unwrap();
		std::fs::create_dir_all(home.path().join(".agents/skills/foo"))
			.unwrap();
		std::fs::create_dir_all(home.path().join("ext")).unwrap();
		let body = "---\nname: foo\ndescription: d\n---\n\nSAME\n";
		std::fs::write(private.join("SKILL.md"), body).unwrap();
		std::fs::write(home.path().join(".agents/skills/foo/SKILL.md"), body)
			.unwrap();
		std::fs::write(home.path().join("ext/real.txt"), "payload").unwrap();
		std::os::unix::fs::symlink(
			home.path().join("ext/real.txt"),
			private.join("extra"),
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"--json",
				"reconcile",
				"skill",
				"--from-agent",
				"claude",
				"--name",
				"foo",
				"--add",
				"cursor",
				"--remove",
				"claude",
				"--yes",
			])
			.output()
			.unwrap();
		assert!(
			private.join("extra").exists(),
			"the source's symlink must survive a proof that cannot see it"
		);
		assert!(!out.status.success());
		// And the refusal must say it could not PROVE, not that content differs
		// — sending someone to reconcile a difference that may not exist is its
		// own wrong answer.
		let stdout = String::from_utf8_lossy(&out.stdout);
		assert!(stdout.contains("cannot PROVE"), "stdout={stdout}");
	}

	// --- (3) An unreadable preserved master is an error, not a reason to echo
	// the caller's own input back as if it were installed.
	{
		let home = tempfile::TempDir::new().unwrap();
		let state = tempfile::TempDir::new().unwrap();
		std::fs::create_dir_all(home.path().join(".agents/skills/foo"))
			.unwrap();
		std::fs::create_dir_all(home.path().join("import/foo")).unwrap();
		std::fs::write(
			home.path().join(".agents/skills/foo/SKILL.md"),
			"not frontmatter at all\n",
		)
		.unwrap();
		std::fs::write(
			home.path().join("import/foo/SKILL.md"),
			"---\nname: foo\ndescription: SOURCE CLAIM\n---\n\nX\n",
		)
		.unwrap();

		let out = isolated_cli(home.path(), state.path())
			.args([
				"-g",
				"-a",
				"gemini",
				"--json",
				"add",
				"skills",
				"--from",
				home.path().join("import/foo").to_str().unwrap(),
			])
			.output()
			.unwrap();
		assert!(!out.status.success());
		let json: Value = serde_json::from_slice(&out.stdout).unwrap();
		assert_eq!(json["error"]["code"], "INVALID_CONFIG", "{json}");
		assert!(
			!home.path().join(".gemini/skills/foo").exists(),
			"the refused add must not leave its referrer behind"
		);
	}
}
