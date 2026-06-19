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
	cmd.current_dir(home);
	cmd
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

#[cfg(unix)]
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
