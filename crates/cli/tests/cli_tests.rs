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
	cmd
}

#[test]
fn test_agent_all_get_skills_is_valid_json_array() {
	let dir = fixtures_dir();
	let out = aghub_cli()
		.current_dir(&dir)
		.args(["--agent", "all", "--all", "get", "skills"])
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

	// Cline has universal_skills + project_skills_path = root/.agents/skills
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
		.args(["--agent", "all", "--all", "get", "mcps"])
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

	// Each entry is an MCP with an agent field
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
	cmd.current_dir(home);
	cmd
}

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

#[test]
fn delete_skill_dry_run_is_default_and_lists_paths() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "mytool");

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "delete", "skills", "mytool"])
		.output()
		.unwrap();

	assert!(
		out.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(json["dryRun"], true);
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

#[test]
fn delete_skill_yes_removes_copy() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let skill_dir = write_claude_skill(home.path(), "goner");

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "delete", "skills", "goner", "--yes"])
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

#[test]
fn prune_lock_default_dry_run_reports_orphan_without_mutating() {
	let home = tempfile::TempDir::new().unwrap();
	let state = tempfile::TempDir::new().unwrap();
	let lock_path = seed_global_lock(state.path());
	let before = std::fs::read(&lock_path).unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "prune-lock"])
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
