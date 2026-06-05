//! Remote `aghub-api` bring-up state machine.
//!
//! Pure, `tauri`-free orchestration over the [`crate::ssh`] foundation. Every
//! function is generic over `<R: CommandRunner>` so the whole bring-up sequence
//! (probe → deploy decision → start + bounded log poll → cleanup) can be
//! exercised under the test `MockRunner` with **no real `ssh`**.

use std::fmt;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ssh::{
	build_remote_cargo_install_cmd, build_remote_cat_cmd,
	build_remote_finish_upload_cmd, build_remote_kill_cmd,
	build_remote_prepare_upload_cmd, build_remote_probe_cmd,
	build_remote_start_cmd, build_scp_args, build_ssh_args,
	is_version_compatible, parse_api_version, parse_logpath, parse_pid,
	parse_remote_port, remote_api_upload_path, CommandRunner, Connection,
};

// ---------------------------------------------------------------------------
// IPC types (camelCase to match the W2 `Connection` convention)
// ---------------------------------------------------------------------------

/// Lifecycle of a single remote connection's bring-up. Projected by the desktop
/// provider to a coarser 4-state FE status (idle/connecting/connected/error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnState {
	Disconnected,
	Probing,
	Deploying,
	Starting,
	Tunneling,
	Connected,
	Error(String),
}

/// Result of probing a remote for a compatible `aghub-api`. Never mutates the
/// remote; safe to call from a "Test connection" button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
	/// The host answered our SSH transport at all (auth + connectivity OK).
	pub reachable: bool,
	/// `aghub-api` exists on the remote `PATH`/at the resolved path.
	pub api_present: bool,
	/// Parsed remote `aghub-api` semver, when present.
	pub api_version: Option<String>,
	/// Remote version is `major.minor`-compatible with the local desktop.
	pub compatible: bool,
	/// Human-facing summary (carries ssh stderr on failure).
	pub message: String,
	/// The probe attempted to install `aghub-api` automatically.
	#[serde(default)]
	pub install_attempted: bool,
	/// Automatic installation completed and the post-install probe found the
	/// binary.
	#[serde(default)]
	pub install_succeeded: bool,
	/// Human-facing install detail, when an install was attempted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub install_message: Option<String>,
}

impl TestResult {
	fn new(
		reachable: bool,
		api_present: bool,
		api_version: Option<String>,
		compatible: bool,
		message: String,
	) -> Self {
		Self {
			reachable,
			api_present,
			api_version,
			compatible,
			message,
			install_attempted: false,
			install_succeeded: false,
			install_message: None,
		}
	}

	fn with_install_result(mut self, succeeded: bool, message: String) -> Self {
		self.install_attempted = true;
		self.install_succeeded = succeeded;
		self.install_message = Some(message);
		self
	}
}

/// Outcome of [`decide_deploy`]: what to do about a (possibly missing) remote
/// binary before starting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeployDecision {
	/// A compatible binary is already present; do nothing.
	Skip,
	/// Same platform + a bundled binary is available; `scp` it over.
	Scp,
	/// Cannot auto-deploy; instruct the user (carries the install hint).
	InstructInstall(String),
}

/// Structured bring-up failure. Crosses IPC, so it derives serde; it is also a
/// real [`std::error::Error`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectError {
	/// No compatible `aghub-api` on the remote and we can't deploy one.
	#[serde(rename_all = "camelCase")]
	RemoteApiMissing { install_hint: String },
	/// SSH transport failed (auth / connectivity); carries ssh stderr.
	Unreachable { stderr: String },
	/// The server never reported its port within the poll budget.
	StartTimeout,
	/// The tunnel child failed to establish the port-forward.
	TunnelFailed(String),
	/// Automatic remote installation failed.
	DeployFailed(String),
}

impl fmt::Display for ConnectError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			ConnectError::RemoteApiMissing { install_hint } => {
				write!(f, "remote aghub-api is missing: {install_hint}")
			}
			ConnectError::Unreachable { stderr } => {
				write!(f, "remote is unreachable: {stderr}")
			}
			ConnectError::StartTimeout => {
				write!(f, "remote aghub-api did not report a port in time")
			}
			ConnectError::TunnelFailed(msg) => {
				write!(f, "ssh tunnel failed: {msg}")
			}
			ConnectError::DeployFailed(msg) => {
				write!(f, "remote install failed: {msg}")
			}
		}
	}
}

impl std::error::Error for ConnectError {}

/// Where an automatic remote install should source `aghub-api` from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteInstallSource {
	/// Upload this explicit local binary with scp, then install it remotely.
	LocalBinary(PathBuf),
	/// Build/install on the VM using cargo from a git repository.
	CargoGit { url: String, branch: Option<String> },
}

/// A successfully started remote server: its pid, the VM-side port it bound, and
/// the remote log path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedServer {
	pub remote_pid: u32,
	pub remote_port: u16,
	pub log_path: String,
}

// ---------------------------------------------------------------------------
// Bring-up steps (generic over the command runner)
// ---------------------------------------------------------------------------

/// Resolve the remote binary path for a connection: the user-configured
/// `remoteAghubPath` or the default `aghub-api` (resolved on the remote `PATH`).
fn resolved_path(conn: &Connection) -> String {
	conn.remote_aghub_path
		.clone()
		.unwrap_or_else(|| "aghub-api".to_string())
}

/// Did this ssh invocation fail at the *transport* level (host unreachable, auth
/// refused, BatchMode failure, unknown/changed host key) rather than at the
/// *remote command* level?
///
/// OpenSSH reports its OWN failures with exit code 255; any other code means the
/// remote command actually ran, so its relayed stderr must NOT be read as a
/// transport failure (e.g. a non-executable binary exits 126 with "permission
/// denied"). A missing code (ssh killed by signal) is treated as transport-level.
fn is_transport_failure(status_code: Option<i32>) -> bool {
	matches!(status_code, Some(255) | None)
}

/// Probe a remote for a compatible `aghub-api`. Pure orchestration over the
/// command runner; never mutates the remote.
pub fn probe_connection<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	local_version: &str,
) -> TestResult {
	let bin = resolved_path(conn);
	let remote_cmd = build_remote_probe_cmd(&bin);
	let args = build_ssh_args(conn, &remote_cmd);

	match runner.run("ssh", &args) {
		Err(e) => TestResult::new(false, false, None, false, e.to_string()),
		Ok(out) => {
			// SSH transport failure → unreachable.
			if is_transport_failure(out.status_code) {
				return TestResult::new(false, false, None, false, out.stderr);
			}
			// Reachable but the remote command itself was not found.
			let cmd_not_found = out.status_code == Some(127)
				|| out
					.stderr
					.to_ascii_lowercase()
					.contains("command not found")
				|| out.stderr.to_ascii_lowercase().contains("no such file");
			if cmd_not_found {
				return TestResult::new(
					true,
					false,
					None,
					false,
					if out.stderr.is_empty() {
						format!("{bin}: command not found")
					} else {
						out.stderr
					},
				);
			}
			// status 0 (or any other non-transport success): parse version.
			if out.status_code == Some(0) {
				let version = parse_api_version(&out.stdout);
				let compatible = version
					.as_deref()
					.map(|v| is_version_compatible(local_version, v))
					.unwrap_or(false);
				let present = version.is_some();
				let message = match &version {
					Some(v) if compatible => {
						format!("aghub-api {v} (compatible)")
					}
					Some(v) => {
						format!(
							"aghub-api {v} (incompatible with {local_version})"
						)
					}
					None => "aghub-api responded without a parseable version"
						.to_string(),
				};
				return TestResult::new(
					true, present, version, compatible, message,
				);
			}
			// Reachable, non-zero, not a recognized "not found": treat as a
			// present-but-failed binary.
			TestResult::new(
				true,
				false,
				None,
				false,
				if out.stderr.is_empty() {
					format!("probe exited with status {:?}", out.status_code)
				} else {
					out.stderr
				},
			)
		}
	}
}

/// Ensure the remote has an `aghub-api` binary available.
///
/// This probes first. If the binary is absent and an install source is provided,
/// it installs over ssh/scp and probes again. The final [`TestResult`] is
/// returned so callers can still reject incompatible versions.
pub fn ensure_remote_api<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	local_version: &str,
	source: Option<&RemoteInstallSource>,
) -> Result<TestResult, ConnectError> {
	let first = probe_connection(runner, conn, local_version);
	if !first.reachable {
		return Err(ConnectError::Unreachable {
			stderr: first.message,
		});
	}
	if first.api_present {
		return Ok(first);
	}

	let Some(source) = source else {
		return Err(ConnectError::RemoteApiMissing {
			install_hint: install_hint(),
		});
	};

	let bin = resolved_path(conn);
	install_remote_api(runner, conn, &bin, source)?;

	let second = probe_connection(runner, conn, local_version)
		.with_install_result(true, "aghub-api installed".to_string());
	if !second.reachable {
		return Err(ConnectError::Unreachable {
			stderr: second.message,
		});
	}
	if !second.api_present {
		return Err(ConnectError::DeployFailed(format!(
			"Automatic install ran, but aghub-api is still unavailable: {}",
			second.message
		)));
	}
	Ok(second)
}

/// Install `aghub-api` on the remote by uploading a binary or running cargo.
pub fn install_remote_api<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	resolved_path: &str,
	source: &RemoteInstallSource,
) -> Result<(), ConnectError> {
	match source {
		RemoteInstallSource::LocalBinary(local) => {
			let prepare_cmd = build_remote_prepare_upload_cmd();
			run_remote_install_step(runner, conn, &prepare_cmd)?;

			let local = local.to_string_lossy().into_owned();
			let scp_args =
				build_scp_args(conn, &local, remote_api_upload_path());
			let scp_out = runner
				.run("scp", &scp_args)
				.map_err(|e| ConnectError::DeployFailed(e.to_string()))?;
			if is_transport_failure(scp_out.status_code) {
				return Err(ConnectError::Unreachable {
					stderr: scp_out.stderr,
				});
			}
			if scp_out.status_code != Some(0) {
				return Err(ConnectError::DeployFailed(nonzero_message(
					"scp upload",
					&scp_out,
				)));
			}

			let finish_cmd = build_remote_finish_upload_cmd(resolved_path);
			run_remote_install_step(runner, conn, &finish_cmd)
		}
		RemoteInstallSource::CargoGit { url, branch } => {
			let install_cmd =
				build_remote_cargo_install_cmd(url, branch.as_deref());
			run_remote_install_step(runner, conn, &install_cmd)
		}
	}
}

/// Force-redeploy a version-locked local `aghub-api` over a present-but-
/// incompatible remote one. Best-effort kills the running process first (avoids
/// `ETXTBSY` on the overwrite + orphaned servers), overwrites the binary
/// (prepare → scp → atomic mv+chmod), then re-probes and returns the fresh
/// result. The caller proceeds only when `compatible`.
pub fn force_redeploy_remote_api<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	local_version: &str,
	source: &RemoteInstallSource,
) -> Result<TestResult, ConnectError> {
	let bin = resolved_path(conn);
	let pkill = build_ssh_args(conn, &crate::ssh::build_remote_pkill_cmd());
	let _ = runner.run("ssh", &pkill);
	install_remote_api(runner, conn, &bin, source)?;
	let probe = probe_connection(runner, conn, local_version);
	if !probe.reachable {
		return Err(ConnectError::Unreachable {
			stderr: probe.message,
		});
	}
	Ok(probe.with_install_result(true, "aghub-api redeployed".to_string()))
}

fn run_remote_install_step<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	remote_cmd: &str,
) -> Result<(), ConnectError> {
	let args = build_ssh_args(conn, remote_cmd);
	let out = runner
		.run("ssh", &args)
		.map_err(|e| ConnectError::DeployFailed(e.to_string()))?;
	if is_transport_failure(out.status_code) {
		return Err(ConnectError::Unreachable { stderr: out.stderr });
	}
	if out.status_code == Some(0) {
		return Ok(());
	}
	Err(ConnectError::DeployFailed(nonzero_message(
		"remote install step",
		&out,
	)))
}

fn nonzero_message(step: &str, out: &crate::ssh::CommandOutput) -> String {
	let detail = if out.stderr.trim().is_empty() {
		out.stdout.trim()
	} else {
		out.stderr.trim()
	};
	if detail.is_empty() {
		format!("{step} exited with status {:?}", out.status_code)
	} else {
		format!("{step} exited with status {:?}: {detail}", out.status_code)
	}
}

/// Decide what to do about the remote binary before starting it.
pub fn decide_deploy(
	test: &TestResult,
	same_platform: bool,
	has_bundled_binary: bool,
) -> DeployDecision {
	if test.api_present && test.compatible {
		return DeployDecision::Skip;
	}
	if !test.api_present && same_platform && has_bundled_binary {
		return DeployDecision::Scp;
	}
	DeployDecision::InstructInstall(install_hint())
}

/// The user-facing install instruction returned when we cannot auto-deploy.
fn install_hint() -> String {
	"aghub-api is not installed on the remote. Install it on the VM with \
     `cargo install --path crates/api` (or `just install`) and ensure it is on \
     your PATH, or set a remoteAghubPath for this connection."
		.to_string()
}

/// Start `aghub-api` on the remote and poll its log for the bound port.
///
/// Issues the detached start command, parses the pid + log path it echoes, then
/// polls `cat <log>` up to `poll_attempts` times (sleeping `delay` between
/// tries) for an `AGHUB_API_PORT=<n>` line. On exhaustion it issues a guarded
/// remote kill and returns [`ConnectError::StartTimeout`]. Tests pass
/// `delay = Duration::ZERO` for instant runs.
pub fn start_remote<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	resolved_path: &str,
	poll_attempts: u32,
	delay: Duration,
) -> Result<StartedServer, ConnectError> {
	let start_cmd = build_remote_start_cmd(resolved_path, &conn.id);
	let start_args = build_ssh_args(conn, &start_cmd);
	let start_out = runner
		.run("ssh", &start_args)
		.map_err(|e| ConnectError::TunnelFailed(e.to_string()))?;

	if is_transport_failure(start_out.status_code) {
		return Err(ConnectError::Unreachable {
			stderr: start_out.stderr,
		});
	}

	let pid = match parse_pid(&start_out.stdout) {
		Some(pid) => pid,
		None => return Err(ConnectError::StartTimeout),
	};
	let log_path = match parse_logpath(&start_out.stdout) {
		Some(path) => path,
		None => return Err(ConnectError::StartTimeout),
	};

	let cat_cmd = build_remote_cat_cmd(&log_path);
	let cat_args = build_ssh_args(conn, &cat_cmd);

	for attempt in 0..poll_attempts {
		if let Ok(out) = runner.run("ssh", &cat_args) {
			if let Some(remote_port) = parse_remote_port(&out.stdout) {
				return Ok(StartedServer {
					remote_pid: pid,
					remote_port,
					log_path,
				});
			}
		}
		// Don't sleep after the final attempt.
		if attempt + 1 < poll_attempts {
			sleep(delay);
		}
	}

	// Exhausted: clean up the orphaned server, then report the timeout.
	cleanup_remote(runner, conn, pid);
	Err(ConnectError::StartTimeout)
}

/// Issue the guarded remote kill for `pid` over ssh. Best-effort: errors are
/// swallowed (cleanup must not panic / propagate during teardown).
pub fn cleanup_remote<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	pid: u32,
) {
	let kill_cmd = build_remote_kill_cmd(pid);
	let kill_args = build_ssh_args(conn, &kill_cmd);
	let _ = runner.run("ssh", &kill_args);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ssh::{
		build_remote_cargo_install_cmd, build_remote_finish_upload_cmd,
		build_remote_prepare_upload_cmd, build_scp_args, CommandOutput,
	};
	use crate::test_support::MockRunner;

	const LOCAL: &str = "1.1.1";

	fn conn() -> Connection {
		Connection {
			id: "vm-1".to_string(),
			label: "My VM".to_string(),
			ssh_target: "my-vm".to_string(),
			user: Some("alice".to_string()),
			port: Some(2222),
			remote_aghub_path: None,
		}
	}

	/// The exact ssh argv the probe step builds for [`conn`].
	fn probe_args() -> Vec<String> {
		let remote_cmd = build_remote_probe_cmd("aghub-api");
		build_ssh_args(&conn(), &remote_cmd)
	}

	fn args_as_str(args: &[String]) -> Vec<&str> {
		args.iter().map(|s| s.as_str()).collect()
	}

	// --- probe_connection --------------------------------------------------

	#[test]
	fn probe_ok_reports_present_compatible() {
		let args = probe_args();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: Some(0),
				stdout: "aghub-api 1.1.1".to_string(),
				stderr: String::new(),
			},
		);
		let res = probe_connection(&runner, &conn(), LOCAL);
		assert!(res.reachable);
		assert!(res.api_present);
		assert_eq!(res.api_version.as_deref(), Some("1.1.1"));
		assert!(res.compatible);
	}

	#[test]
	fn probe_unreachable_carries_stderr() {
		let args = probe_args();
		let stderr = "Host key verification failed.\n\
                      BatchMode is set and authentication failed.";
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: Some(255),
				stdout: String::new(),
				stderr: stderr.to_string(),
			},
		);
		let res = probe_connection(&runner, &conn(), LOCAL);
		assert!(!res.reachable);
		assert!(!res.api_present);
		assert!(!res.compatible);
		assert_eq!(res.message, stderr);
	}

	#[test]
	fn probe_remote_permission_denied_is_reachable_not_transport_failure() {
		// A non-executable remote binary exits 126 with "permission denied" on
		// the REMOTE side. SSH transport actually succeeded, so the host is
		// reachable; only the binary is unusable. (Regression: the old phrase
		// match treated this as unreachable.)
		let args = probe_args();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: Some(126),
				stdout: String::new(),
				stderr: "bash: /usr/bin/aghub-api: Permission denied"
					.to_string(),
			},
		);
		let res = probe_connection(&runner, &conn(), LOCAL);
		assert!(res.reachable, "ssh succeeded; host is reachable");
		assert!(!res.api_present);
		assert!(!res.compatible);
	}

	#[test]
	fn probe_signal_killed_ssh_is_transport_failure() {
		// No exit code (ssh killed by signal) is a local/transport problem.
		let args = probe_args();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: None,
				stdout: String::new(),
				stderr: String::new(),
			},
		);
		let res = probe_connection(&runner, &conn(), LOCAL);
		assert!(!res.reachable);
	}

	#[test]
	fn probe_command_not_found_reports_absent_but_reachable() {
		let args = probe_args();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: Some(127),
				stdout: String::new(),
				stderr: "bash: aghub-api: command not found".to_string(),
			},
		);
		let res = probe_connection(&runner, &conn(), LOCAL);
		assert!(res.reachable);
		assert!(!res.api_present);
		assert!(res.api_version.is_none());
		assert!(!res.compatible);
	}

	#[test]
	fn probe_present_but_incompatible_minor() {
		let args = probe_args();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: Some(0),
				stdout: "aghub-api 1.2.0".to_string(),
				stderr: String::new(),
			},
		);
		let res = probe_connection(&runner, &conn(), LOCAL);
		assert!(res.reachable);
		assert!(res.api_present);
		assert_eq!(res.api_version.as_deref(), Some("1.2.0"));
		assert!(!res.compatible);
	}

	// --- decide_deploy -----------------------------------------------------

	fn present_compatible() -> TestResult {
		TestResult::new(
			true,
			true,
			Some("1.1.1".to_string()),
			true,
			String::new(),
		)
	}

	fn absent() -> TestResult {
		TestResult::new(true, false, None, false, String::new())
	}

	#[test]
	fn decide_deploy_skip_when_present_and_compatible() {
		let d = decide_deploy(&present_compatible(), false, false);
		assert_eq!(d, DeployDecision::Skip);
	}

	#[test]
	fn decide_deploy_scp_when_absent_same_platform_with_binary() {
		let d = decide_deploy(&absent(), true, true);
		assert_eq!(d, DeployDecision::Scp);
	}

	#[test]
	fn decide_deploy_instruct_otherwise() {
		// Absent, cross-platform → instruct.
		match decide_deploy(&absent(), false, true) {
			DeployDecision::InstructInstall(hint) => {
				assert!(hint.contains("cargo install --path crates/api"));
			}
			other => panic!("expected InstructInstall, got {other:?}"),
		}
		// Absent, same platform but no bundled binary → instruct.
		match decide_deploy(&absent(), true, false) {
			DeployDecision::InstructInstall(hint) => {
				assert!(hint.contains("install"));
			}
			other => panic!("expected InstructInstall, got {other:?}"),
		}
	}

	// --- install_remote_api -----------------------------------------------

	#[test]
	fn install_remote_api_via_cargo_git_runs_remote_cargo_install() {
		let source = RemoteInstallSource::CargoGit {
			url: "https://github.com/audichuang/aghub.git".to_string(),
			branch: Some("feat/remote-ssh-management".to_string()),
		};
		let install_cmd = build_remote_cargo_install_cmd(
			"https://github.com/audichuang/aghub.git",
			Some("feat/remote-ssh-management"),
		);
		let install_args = build_ssh_args(&conn(), &install_cmd);
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&install_args),
			CommandOutput {
				status_code: Some(0),
				stdout: String::new(),
				stderr: String::new(),
			},
		);

		install_remote_api(&runner, &conn(), "aghub-api", &source)
			.expect("cargo-git install should succeed");

		let calls = runner.calls();
		assert_eq!(calls.len(), 1);
		assert_eq!(calls[0].program, "ssh");
		assert_eq!(calls[0].args, install_args);
	}

	#[test]
	fn force_redeploy_pkills_overwrites_then_reprobes() {
		let source = RemoteInstallSource::LocalBinary("/tmp/aghub-api".into());
		let pkill_args =
			build_ssh_args(&conn(), &crate::ssh::build_remote_pkill_cmd());
		let prepare_args =
			build_ssh_args(&conn(), &build_remote_prepare_upload_cmd());
		let scp_args = build_scp_args(
			&conn(),
			"/tmp/aghub-api",
			crate::ssh::remote_api_upload_path(),
		);
		let finish_args = build_ssh_args(
			&conn(),
			&build_remote_finish_upload_cmd("aghub-api"),
		);
		let probe_args = build_ssh_args(
			&conn(),
			&crate::ssh::build_remote_probe_cmd("aghub-api"),
		);
		let ok = || CommandOutput {
			status_code: Some(0),
			stdout: String::new(),
			stderr: String::new(),
		};
		let ver = || CommandOutput {
			status_code: Some(0),
			stdout: "aghub-api 1.1.1".to_string(),
			stderr: String::new(),
		};
		let runner = MockRunner::new()
			.script("ssh", &args_as_str(&pkill_args), ok())
			.script("ssh", &args_as_str(&prepare_args), ok())
			.script("scp", &args_as_str(&scp_args), ok())
			.script("ssh", &args_as_str(&finish_args), ver())
			.script("ssh", &args_as_str(&probe_args), ver());

		let result =
			force_redeploy_remote_api(&runner, &conn(), "1.1.1", &source)
				.unwrap();

		assert!(result.compatible);
		assert!(result.api_present);
		// The stale process is killed BEFORE the overwrite.
		let calls = runner.calls();
		assert_eq!(calls[0].args, pkill_args, "pkill must be issued first");
	}

	#[test]
	fn install_remote_api_from_local_binary_uploads_then_installs() {
		let source = RemoteInstallSource::LocalBinary("/tmp/aghub-api".into());
		let prepare_args =
			build_ssh_args(&conn(), &build_remote_prepare_upload_cmd());
		let scp_args = build_scp_args(
			&conn(),
			"/tmp/aghub-api",
			crate::ssh::remote_api_upload_path(),
		);
		let finish_args = build_ssh_args(
			&conn(),
			&build_remote_finish_upload_cmd("aghub-api"),
		);
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&prepare_args),
				CommandOutput {
					status_code: Some(0),
					stdout: String::new(),
					stderr: String::new(),
				},
			)
			.script(
				"scp",
				&args_as_str(&scp_args),
				CommandOutput {
					status_code: Some(0),
					stdout: String::new(),
					stderr: String::new(),
				},
			)
			.script(
				"ssh",
				&args_as_str(&finish_args),
				CommandOutput {
					status_code: Some(0),
					stdout: "aghub-api 1.1.1".to_string(),
					stderr: String::new(),
				},
			);

		install_remote_api(&runner, &conn(), "aghub-api", &source)
			.expect("local binary install should succeed");

		let calls = runner.calls();
		assert_eq!(calls.len(), 3);
		assert_eq!(calls[0].args, prepare_args);
		assert_eq!(calls[1].program, "scp");
		assert_eq!(calls[1].args, scp_args);
		assert_eq!(calls[2].args, finish_args);
	}

	// --- start_remote ------------------------------------------------------

	/// The ssh argv for the start step (default resolved path "aghub-api").
	fn start_args() -> Vec<String> {
		let cmd = build_remote_start_cmd("aghub-api", "vm-1");
		build_ssh_args(&conn(), &cmd)
	}

	/// The ssh argv for `cat <log>` against the given log path.
	fn cat_args(log: &str) -> Vec<String> {
		let cmd = build_remote_cat_cmd(log);
		build_ssh_args(&conn(), &cmd)
	}

	/// The ssh argv for the guarded kill of `pid`.
	fn kill_args(pid: u32) -> Vec<String> {
		let cmd = build_remote_kill_cmd(pid);
		build_ssh_args(&conn(), &cmd)
	}

	#[test]
	fn start_remote_success_parses_pid_and_port() {
		let log = "/run/u/aghub.log";
		let start = start_args();
		let cat = cat_args(log);
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&start),
				CommandOutput {
					status_code: Some(0),
					stdout: format!("PID=4242\nLOGPATH={log}"),
					stderr: String::new(),
				},
			)
			.script(
				"ssh",
				&args_as_str(&cat),
				CommandOutput {
					status_code: Some(0),
					stdout: "AGHUB_API_PORT=8123".to_string(),
					stderr: String::new(),
				},
			);

		let started =
			start_remote(&runner, &conn(), "aghub-api", 5, Duration::ZERO)
				.expect("start should succeed");
		assert_eq!(started.remote_pid, 4242);
		assert_eq!(started.remote_port, 8123);
		assert_eq!(started.log_path, log);
	}

	#[test]
	fn start_remote_timeout_issues_guarded_kill() {
		let log = "/run/u/aghub.log";
		let start = start_args();
		let cat = cat_args(log);
		// `cat` always returns an empty log (port never appears).
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&start),
				CommandOutput {
					status_code: Some(0),
					stdout: format!("PID=4242\nLOGPATH={log}"),
					stderr: String::new(),
				},
			)
			.script(
				"ssh",
				&args_as_str(&cat),
				CommandOutput {
					status_code: Some(0),
					stdout: String::new(),
					stderr: String::new(),
				},
			);

		let err =
			start_remote(&runner, &conn(), "aghub-api", 3, Duration::ZERO)
				.expect_err("start should time out");
		assert_eq!(err, ConnectError::StartTimeout);

		// A guarded kill of pid 4242 must have been issued.
		let expected_kill = kill_args(4242);
		let calls = runner.calls();
		assert!(
            calls.iter().any(|c| c.program == "ssh" && c.args == expected_kill),
            "expected a guarded-kill ssh call for pid 4242, recorded calls: {calls:?}"
        );
	}

	#[test]
	fn start_remote_missing_logpath_times_out() {
		let start = start_args();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&start),
			CommandOutput {
				status_code: Some(0),
				// PID present but no LOGPATH line.
				stdout: "PID=999".to_string(),
				stderr: String::new(),
			},
		);
		let err =
			start_remote(&runner, &conn(), "aghub-api", 3, Duration::ZERO)
				.expect_err("missing logpath should time out");
		assert_eq!(err, ConnectError::StartTimeout);
	}

	// --- cleanup_remote ----------------------------------------------------

	#[test]
	fn cleanup_remote_issues_guarded_kill() {
		let expected_kill = kill_args(7777);
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&expected_kill),
			CommandOutput {
				status_code: Some(0),
				stdout: String::new(),
				stderr: String::new(),
			},
		);
		cleanup_remote(&runner, &conn(), 7777);
		let calls = runner.calls();
		assert_eq!(calls.len(), 1);
		assert_eq!(calls[0].program, "ssh");
		assert_eq!(calls[0].args, expected_kill);
		assert!(calls[0]
			.args
			.last()
			.unwrap()
			.starts_with("bash -lc 'eval \"$(printf %s "));
	}

	// --- serde / Error surface ---------------------------------------------

	#[test]
	fn connect_error_is_error_trait_object_and_serializes_camel_case() {
		let e: Box<dyn std::error::Error> =
			Box::new(ConnectError::RemoteApiMissing {
				install_hint: "do the thing".to_string(),
			});
		assert!(e.to_string().contains("do the thing"));

		let json = serde_json::to_value(ConnectError::RemoteApiMissing {
			install_hint: "h".to_string(),
		})
		.unwrap();
		assert_eq!(json["remoteApiMissing"]["installHint"], "h");
	}

	#[test]
	fn test_result_serializes_camel_case() {
		let json = serde_json::to_value(present_compatible()).unwrap();
		assert!(json.get("apiPresent").is_some());
		assert!(json.get("apiVersion").is_some());
		assert!(json.get("api_present").is_none());
	}

	#[test]
	fn started_server_serializes_camel_case() {
		let json = serde_json::to_value(StartedServer {
			remote_pid: 1,
			remote_port: 2,
			log_path: "/x".to_string(),
		})
		.unwrap();
		assert!(json.get("remotePid").is_some());
		assert!(json.get("remotePort").is_some());
		assert!(json.get("logPath").is_some());
	}
}
