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
		.args(["-a", "claude", "get", "mcps"])
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

#[cfg(unix)] // Windows: global MCP config not HOME-isolated
#[test]
fn cli_delete_mcp_dry_run_default() {
	// No --yes => dry-run: JSON reports dry_run:true/executed:false and the
	// MCP is still listed afterward (non-executed branch leaves state intact).
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	seed_mcp(home.path(), state.path(), "m");

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "delete", "mcps", "m"])
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
		.args(["-a", "claude", "delete", "mcps", "goner", "--yes"])
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
		.args(["-a", "claude", "delete", "mcps", "ghost", "--yes"])
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
		.args(["-a", "claude", "delete", "mcps", "ghost", "--yes"])
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
		.args(["-a", "claude", "get", "skills"])
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
		.args(["--agent", "all", "get", "skills"])
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
		.args(["-a", "claude", "describe", "skills", "mytool"])
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
	let orig = std::fs::metadata(&lock_dir).unwrap().permissions();
	std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(0o555))
		.unwrap();

	let out = isolated_cli(home.path(), state.path())
		.args(["-a", "claude", "delete", "skills", "goner", "--yes"])
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
		stderr.contains("either -g or -p"),
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

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-g",
			"-a",
			"claude",
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

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-g",
			"source",
			"sync",
			"owner/repo",
			"--skill",
			"nope",
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
		])
		.output()
		.unwrap();
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
	let out1 = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-g",
			"-a",
			"claude",
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
	assert!(out1.status.success());
	assert_eq!(
		count_symlinks_named(home.path(), "alpha"),
		1,
		"single-agent install must link exactly one agent"
	);

	// Step 2: `-a all` re-run must repair — link the rest despite the lock.
	let out2 = isolated_cli(home.path(), state.path())
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
		])
		.output()
		.unwrap();
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

	let out = isolated_cli(home.path(), state.path())
		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
		.args([
			"-g",
			"-a",
			"claude",
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
		isolated_cli(home.path(), state.path())
			.env("AGHUB_TEST_SOURCE_FETCH_ROOT", src.path())
			.args([
				"-g",
				"-a",
				"claude",
				"source",
				"sync",
				"owner/repo",
				"--skill",
				"alpha",
				"--install-missing",
				"--yes",
			])
			.output()
			.unwrap()
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
	assert!(
		stderr.contains("choose either -g or -p"),
		"stderr: {stderr}"
	);
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
		.args(["-a", "claude", "update", "mcps", "m", "--timeout", "45"])
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
		.args(["-a", "claude", "delete", "skills", "snaketool"])
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
	// With no --api-key and no env, a piped stdin key is used. Mirrors the
	// resolve_api_key stdin branch.
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
			.join(".opencode/skills/repo-helper/SKILL.md")
			.exists(),
		"transfer must copy the skill into OpenCode's project skills dir"
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

#[test]
fn transfer_skill_second_run_fails_resource_exists() {
	let project = transfer_project("repo-helper");

	// First transfer succeeds.
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
		])
		.output()
		.unwrap();
	assert!(first.status.success());

	// Second transfer of the same skill into the same target fails (the
	// destination already exists) and the CLI exits non-zero on a failed batch.
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
		!out.status.success(),
		"a failed batch must exit non-zero; stdout: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	let json: Value = serde_json::from_slice(&out.stdout).unwrap();
	let row = json["results"]
		.as_array()
		.and_then(|a| a.iter().find(|r| r["agent"] == "opencode"))
		.expect("opencode row present");
	assert_eq!(row["success"], false);
	assert!(
		row["error"].as_str().is_some(),
		"a failed row must carry an error string: {row}"
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
			.join(".opencode/skills/repo-helper/SKILL.md")
			.exists(),
		"reconcile --add must materialize the OpenCode skill"
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
