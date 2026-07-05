//! Clone and remote inspection via the system `git` binary.
//!
//! gix cannot use the platform credential helpers (Windows Credential Manager,
//! Git Credential Manager, NTLM/Kerberos for on-prem hosts). When no explicit
//! token is supplied we shell out to `git` so those helpers resolve
//! credentials for private hosts such as Azure DevOps Server / TFS.

use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::TempDir;

use crate::error::{GitError, Result};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn git_command() -> Command {
	let mut cmd = Command::new("git");
	// Include the full repo path in the credential-helper query. On-prem Azure
	// DevOps Server / TFS commonly store credentials scoped to the collection
	// path (e.g. .../IVTLXITP01-ITP/_git/repo), not just the host, so a
	// host-only lookup misses them. This MUST match `probe_credential` so the
	// pre-flight check and the real clone resolve the same credential.
	cmd.args(["-c", "credential.useHttpPath=true"]);
	// Fail fast instead of blocking on a terminal username/password prompt in a
	// GUI/headless context. GUI credential helpers (e.g. Git Credential
	// Manager) are unaffected and still resolve or prompt as configured.
	cmd.env("GIT_TERMINAL_PROMPT", "0");
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		cmd.creation_flags(CREATE_NO_WINDOW);
	}
	cmd
}

/// Whether the system git credential helpers can return a credential for
/// `url` **without any interactive prompt** — i.e. whether an unattended
/// background clone would authenticate. A successful interactive `git
/// ls-remote` (which may have popped a GCM login) does NOT imply this.
///
/// Never triggers UI: terminal prompts are disabled and `GCM_INTERACTIVE` is
/// forced off, so a missing credential fails fast instead of opening a dialog.
pub fn probe_credential(url: &str) -> bool {
	// The credential protocol is line-based: a CR/LF/NUL in the URL could
	// inject extra fields (host=, username=, ...) into the helper request.
	// A URL with control characters is malformed anyway — treat as no match.
	if url.contains(|c: char| c.is_control()) {
		return false;
	}
	let mut cmd = git_command();
	cmd.env("GCM_INTERACTIVE", "Never")
		.args(["-c", "credential.interactive=false", "credential", "fill"])
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::null());

	let Ok(mut child) = cmd.spawn() else {
		return false;
	};
	if let Some(mut stdin) = child.stdin.take() {
		// git parses `url=` into protocol/host/path; with useHttpPath=true
		// (set in git_command) the path is included in the helper query. The
		// blank line terminates the request.
		let _ = write!(stdin, "url={url}\n\n");
	}
	match child.wait_with_output() {
		Ok(output) => {
			output.status.success()
				&& String::from_utf8_lossy(&output.stdout).lines().any(|line| {
					line.strip_prefix("password=")
						.is_some_and(|v| !v.is_empty())
				})
		}
		Err(_) => false,
	}
}

/// Whether a usable `git` binary is available on PATH.
pub fn system_git_available() -> bool {
	git_command()
		.arg("--version")
		.output()
		.map(|o| o.status.success())
		.unwrap_or(false)
}

/// Clone a repository into a temporary directory using the system `git`
/// binary, letting git's configured credential helpers handle authentication.
pub fn clone_to_temp_system_git(
	url: &str,
	branch: Option<&str>,
) -> Result<TempDir> {
	let temp_dir =
		TempDir::new().map_err(|e| GitError::TempDirFailed(e.to_string()))?;
	let mut cmd = git_command();
	cmd.args(["clone", "--depth", "1"]);
	if let Some(branch) = branch {
		cmd.args(["--branch", branch]);
	}
	cmd.arg("--").arg(url).arg(temp_dir.path());

	let output = cmd.output().map_err(|e| {
		GitError::clone_failed(format!("Failed to run git: {e}"))
	})?;
	if !output.status.success() {
		return Err(GitError::clone_failed(format!(
			"Fetch failed: {}",
			String::from_utf8_lossy(&output.stderr).trim()
		)));
	}
	Ok(temp_dir)
}

/// List remote branch names via the system `git` binary.
pub fn list_remote_branches_system_git(url: &str) -> Result<Vec<String>> {
	let output = git_command()
		.args(["ls-remote", "--heads", "--", url])
		.output()
		.map_err(|e| {
			GitError::clone_failed(format!("Failed to run git: {e}"))
		})?;
	if !output.status.success() {
		return Err(GitError::clone_failed(format!(
			"Fetch failed: {}",
			String::from_utf8_lossy(&output.stderr).trim()
		)));
	}
	Ok(parse_ls_remote_heads(&String::from_utf8_lossy(
		&output.stdout,
	)))
}

/// Parse `git ls-remote --heads` output into sorted, de-duplicated branch
/// names. Each line is `<sha>\trefs/heads/<branch>`.
fn parse_ls_remote_heads(stdout: &str) -> Vec<String> {
	let mut branches: Vec<String> = stdout
		.lines()
		.filter_map(|line| line.split_whitespace().nth(1))
		.filter_map(|r| r.strip_prefix("refs/heads/"))
		.map(|s| s.to_string())
		.collect();
	branches.sort();
	branches.dedup();
	branches
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_ls_remote_heads() {
		let stdout = "\
deadbeef\trefs/heads/main
cafebabe\trefs/heads/develop
0badf00d\trefs/heads/feature/x
";
		assert_eq!(
			parse_ls_remote_heads(stdout),
			vec![
				"develop".to_string(),
				"feature/x".to_string(),
				"main".to_string(),
			],
		);
	}

	#[test]
	fn ignores_non_head_refs() {
		// `--heads` only emits heads, but be defensive against stray refs.
		let stdout = "deadbeef\trefs/tags/v1.0\ncafebabe\trefs/heads/main\n";
		assert_eq!(parse_ls_remote_heads(stdout), vec!["main".to_string()]);
	}
}
