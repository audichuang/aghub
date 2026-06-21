//! Remote `aghub-api` bring-up state machine.
//!
//! Pure, `tauri`-free orchestration over the [`crate::ssh`] foundation. Every
//! function is generic over `<R: CommandRunner>` so the whole bring-up sequence
//! (probe → ensure/redeploy → start + bounded log poll → cleanup) can be
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
	build_remote_release_deb_install_cmd, build_remote_start_cmd,
	build_scp_args, build_ssh_args, is_version_compatible, parse_api_version,
	parse_logpath, parse_pid, parse_remote_port, probe_remote_platform,
	probe_supports_credential_forwarding, remote_api_upload_path,
	CommandRunner, Connection,
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
	/// The remote `aghub-api` advertises controller-side git-credential
	/// forwarding (the `X-Aghub-Git-Tokens` header). Probed over SSH via
	/// `--capabilities`; **fail-safe** — `false` whenever support cannot be
	/// confirmed (old binary, transport failure, missing marker), so the
	/// desktop only forwards credentials to a remote that genuinely honors
	/// them. Always `false` when the remote is unreachable or the api is
	/// absent.
	#[serde(default)]
	pub supports_credential_forwarding: bool,
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
			supports_credential_forwarding: false,
			install_attempted: false,
			install_succeeded: false,
			install_message: None,
		}
	}

	fn with_credential_forwarding(mut self, supported: bool) -> Self {
		self.supports_credential_forwarding = supported;
		self
	}

	fn with_install_result(mut self, succeeded: bool, message: String) -> Self {
		self.install_attempted = true;
		self.install_succeeded = succeeded;
		self.install_message = Some(message);
		self
	}
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
	/// A bundled/local `aghub-api` binary cannot be deployed because the
	/// remote's `(os, arch)` differs from the desktop's. Carries the probed
	/// remote platform (`"os/arch"`, or `"unknown"`) so the desktop can map
	/// this to the actionable "install manually" banner.
	#[serde(rename_all = "camelCase")]
	CrossPlatformDeploy { remote_platform: String },
	/// Intentionally retained for the desktop `From<ConnectError>` mapping;
	/// currently never constructed (`start_remote` now returns `DeployFailed`
	/// with the real stderr on every failure path).
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
			ConnectError::CrossPlatformDeploy { remote_platform } => {
				write!(
					f,
					"cannot deploy a local aghub-api binary to remote \
					 platform {remote_platform}: built for {}/{}",
					std::env::consts::OS,
					std::env::consts::ARCH
				)
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
	CargoGit {
		url: String,
		branch: Option<String>,
		tag: Option<String>,
	},
	/// Download and extract a release `.deb` on the VM.
	ReleaseDeb { url: String },
}

/// A successfully started remote server: its pid, the VM-side port it
/// bound, and the remote log path.
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
/// `remoteAghubPath` or the default `aghub-api` (resolved on the remote
/// `PATH`).
fn resolved_path(conn: &Connection) -> String {
	conn.remote_aghub_path
		.clone()
		.unwrap_or_else(|| "aghub-api".to_string())
}

/// Did this ssh invocation fail at the *transport* level (host unreachable,
/// auth refused, BatchMode failure, unknown/changed host key) rather than
/// at the *remote command* level?
///
/// OpenSSH reports its OWN failures with exit code 255; any other code
/// means the remote command actually ran, so its relayed stderr must NOT
/// be read as a transport failure (e.g. a non-executable binary exits 126
/// with "permission denied"). A missing code (ssh killed by signal) is
/// treated as transport-level.
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
				// Additive capability probe (D7): a second `--capabilities`
				// round-trip, only worth running when the binary is actually
				// present. Fail-safe inside the probe, and irrelevant when
				// absent, so an absent/garbled response stays `false`.
				let supports_forwarding = present
					&& probe_supports_credential_forwarding(runner, conn, &bin);
				return TestResult::new(
					true, present, version, compatible, message,
				)
				.with_credential_forwarding(supports_forwarding);
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

/// Ensure the remote has a COMPATIBLE `aghub-api`.
///
/// Probes first. Returns early only when a compatible binary is already
/// present. Otherwise — absent, OR present but version-incompatible — it
/// installs/upgrades over ssh/scp when a source is available (a `LocalBinary`
/// source is same-platform-gated on BOTH paths; `CargoGit` compiles on the VM
/// and is un-gated), then re-probes. With no source (or a cross-platform
/// `LocalBinary`), a present-but-incompatible binary returns the probe so the
/// caller surfaces the Incompatible screen; an absent binary errors. The final
/// [`TestResult`] is returned so callers can still reject incompatible
/// versions.
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
	// Present AND compatible -> nothing to do (unchanged fast path).
	if first.api_present && first.compatible {
		return Ok(first);
	}

	// Absent, or present-but-incompatible: try to install/upgrade when a
	// source exists. No source -> unchanged behaviour:
	//   absent  => RemoteApiMissing (UI shows the manual install hint),
	//   present => Ok(first) so the caller surfaces the Incompatible screen.
	let Some(source) = source else {
		return if first.api_present {
			Ok(first)
		} else {
			Err(ConnectError::RemoteApiMissing {
				install_hint: install_hint(),
			})
		};
	};

	// Same-platform gate for a LocalBinary source — covers BOTH the absent
	// and the upgrade path (a wrong-arch binary would never run). CargoGit
	// compiles on the VM, so it is un-gated for any remote platform.
	if let RemoteInstallSource::LocalBinary(_) = source {
		let local = (std::env::consts::OS, std::env::consts::ARCH);
		let remote = probe_remote_platform(runner, conn);
		let same = remote
			.as_ref()
			.map(|(os, arch)| os == local.0 && arch == local.1)
			.unwrap_or(false);
		if !same {
			let remote_platform = remote
				.map(|(os, arch)| format!("{os}/{arch}"))
				.unwrap_or_else(|| "unknown".to_string());
			// Cross-platform: a wrong-arch bundled binary cannot run on the VM,
			// so refuse the deploy for BOTH the absent and the present-but-
			// incompatible remote and let the desktop surface the manual-install
			// hint (via CrossPlatformRedeploy). Returning `Ok(first)` for the
			// present case would instead render an actionable "Force redeploy"
			// button that can only fail the very same cross-platform gate.
			return Err(ConnectError::CrossPlatformDeploy { remote_platform });
		}
	}

	let bin = resolved_path(conn);
	// `install_remote_api` does stage -> finish (mv + chmod 755), so an
	// upgrade overwrites an old binary cleanly in place.
	install_remote_api(runner, conn, &bin, source)?;

	let second = probe_connection(runner, conn, local_version)
		.with_install_result(true, "aghub-api installed/upgraded".to_string());
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
			stage_remote_api_upload(runner, conn, local)?;
			finish_remote_api_upload(runner, conn, resolved_path)
		}
		RemoteInstallSource::CargoGit { url, branch, tag } => {
			// `cargo install` always writes to ~/.cargo/bin/aghub-api; it cannot
			// target a custom path. If the connection pins an explicit remote
			// path, a cargo build would land where the post-install probe never
			// looks — a confusing "installed, but still unavailable". Refuse up
			// front with an actionable message instead.
			if resolved_path != "aghub-api" {
				return Err(ConnectError::DeployFailed(format!(
					"cannot auto-deploy via cargo-git to the custom remote path \
					 '{resolved_path}': cargo install only writes to \
					 ~/.cargo/bin/aghub-api. Clear the custom path to auto-deploy, \
					 or install aghub-api at '{resolved_path}' on the VM manually."
				)));
			}
			let install_cmd = build_remote_cargo_install_cmd(
				url,
				branch.as_deref(),
				tag.as_deref(),
			);
			run_remote_install_step(runner, conn, &install_cmd)
		}
		RemoteInstallSource::ReleaseDeb { url } => {
			let install_cmd =
				build_remote_release_deb_install_cmd(url, resolved_path);
			run_remote_install_step(runner, conn, &install_cmd)
		}
	}
}

/// Stage a local binary on the remote: `mkdir -p` the cache dir, then `scp` the
/// binary to its `.upload` staging path. Does NOT move it into place — call
/// [`finish_remote_api_upload`] for the atomic swap. Splitting the upload from
/// the swap lets a redeploy stage the new binary BEFORE killing the running
/// server, so a staging failure can never leave the remote with no server.
fn stage_remote_api_upload<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	local: &std::path::Path,
) -> Result<(), ConnectError> {
	let prepare_cmd = build_remote_prepare_upload_cmd();
	run_remote_install_step(runner, conn, &prepare_cmd)?;

	let local = local.to_string_lossy().into_owned();
	let scp_args = build_scp_args(conn, &local, remote_api_upload_path());
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
	Ok(())
}

/// Move a previously-staged upload into its final path (atomic `mv` + `chmod` +
/// a version self-check). Pairs with [`stage_remote_api_upload`].
fn finish_remote_api_upload<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	resolved_path: &str,
) -> Result<(), ConnectError> {
	let finish_cmd = build_remote_finish_upload_cmd(resolved_path);
	run_remote_install_step(runner, conn, &finish_cmd)
}

/// Force-redeploy a version-locked local `aghub-api` over a present-but-
/// incompatible remote one, then re-probe and return the fresh result. The
/// caller proceeds only when `compatible`.
///
/// We deliberately do NOT kill the running server first. The old, incompatible
/// server is left as a harmless orphan: it stays bound to its ephemeral
/// (`--port 0`) port that this redeploy never tunnels to, and the next
/// connection starts its OWN fresh `--port 0` server against the new binary.
/// Replacing the binary in place is safe on its own —
/// `finish_remote_api_upload` does an atomic `mv` of the staged upload, which
/// the kernel handles cleanly even while the old process holds the previous
/// inode open (no `ETXTBSY`). A
/// `pkill`-by-name/path here would inevitably be collateral: we have no pid for
/// the incompatible server (this connection did not start it), so any by-path
/// kill on a shared host would also reap a sibling connection's server running
/// the same binary path (the original self-DoS). Compatibility is still
/// confirmed regardless of the orphan, because the re-probe runs
/// `$target --version` — a fresh exec of the NEW binary. For a `LocalBinary`
/// source the new binary is STAGED first (prepare → scp) and only THEN moved
/// into place (atomic `mv`), so a failed/slow upload aborts with the old server
/// still serving — the remote is never left with no server. `CargoGit` builds
/// in place on the VM.
pub fn force_redeploy_remote_api<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	local_version: &str,
	source: &RemoteInstallSource,
) -> Result<TestResult, ConnectError> {
	force_install_remote_api(
		runner,
		conn,
		local_version,
		source,
		"aghub-api redeployed",
	)
}

/// Force-reinstall `aghub-api` on the remote and re-probe. This is intended for
/// explicit user action from the connection-management test panel, so it does
/// not skip just because a compatible binary is already present.
pub fn reinstall_remote_api<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	local_version: &str,
	source: &RemoteInstallSource,
) -> Result<TestResult, ConnectError> {
	force_install_remote_api(
		runner,
		conn,
		local_version,
		source,
		"aghub-api reinstalled",
	)
}

fn force_install_remote_api<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	local_version: &str,
	source: &RemoteInstallSource,
	install_message: &str,
) -> Result<TestResult, ConnectError> {
	let bin = resolved_path(conn);
	match source {
		RemoteInstallSource::LocalBinary(local) => {
			// Stage before the swap: a staging failure must not
			// down the server.
			stage_remote_api_upload(runner, conn, local)?;
			finish_remote_api_upload(runner, conn, &bin)?;
		}
		RemoteInstallSource::CargoGit { .. }
		| RemoteInstallSource::ReleaseDeb { .. } => {
			// These sources install in place on the VM.
			install_remote_api(runner, conn, &bin, source)?;
		}
	}
	let probe = probe_connection(runner, conn, local_version);
	if !probe.reachable {
		return Err(ConnectError::Unreachable {
			stderr: probe.message,
		});
	}
	Ok(probe.with_install_result(true, install_message.to_string()))
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

/// Attach a command's relayed output to a context message (stderr first,
/// falling back to stdout). Unlike [`nonzero_message`] this carries no status
/// code — it is for cases where the command exited 0 but produced unparseable
/// output.
fn command_output_message(
	context: &str,
	out: &crate::ssh::CommandOutput,
) -> String {
	let detail = if out.stderr.trim().is_empty() {
		out.stdout.trim()
	} else {
		out.stderr.trim()
	};
	if detail.is_empty() {
		context.to_string()
	} else {
		format!("{context}: {detail}")
	}
}

/// The user-facing install instruction returned when we cannot auto-deploy.
pub fn install_hint() -> String {
	"aghub-api is not installed on the remote. Install it on the VM with \
     `cargo install --path crates/api` (or `just install`) and ensure it is on \
     your PATH, or set a remoteAghubPath for this connection."
		.to_string()
}

/// Start `aghub-api` on the remote and poll its log for the bound port.
///
/// Issues the detached start command, parses the pid + log path it echoes, then
/// polls `cat <log>` up to `poll_attempts` times (sleeping `delay` between
/// tries) for an `AGHUB_API_PORT=<n>` line. Every failure path returns a
/// [`ConnectError::DeployFailed`] carrying the actual remote cause (start
/// stderr / unparseable output / last log contents) instead of an opaque
/// timeout, and any orphaned server is reaped with a guarded kill first. Tests
/// pass `delay = Duration::ZERO` for instant runs.
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
	// The start command ran but failed: surface its real stderr/exit status.
	if start_out.status_code != Some(0) {
		return Err(ConnectError::DeployFailed(nonzero_message(
			"remote start",
			&start_out,
		)));
	}

	let pid = match parse_pid(&start_out.stdout) {
		Some(pid) => pid,
		None => {
			return Err(ConnectError::DeployFailed(command_output_message(
				"remote start did not report a PID",
				&start_out,
			)));
		}
	};
	let log_path = match parse_logpath(&start_out.stdout) {
		Some(path) => path,
		None => {
			// We have a pid but no log to poll — reap it before bailing.
			cleanup_remote(runner, conn, pid);
			return Err(ConnectError::DeployFailed(command_output_message(
				"remote start did not report a LOGPATH",
				&start_out,
			)));
		}
	};

	let cat_cmd = build_remote_cat_cmd(&log_path);
	let cat_args = build_ssh_args(conn, &cat_cmd);

	let mut last_detail = String::new();
	for attempt in 0..poll_attempts {
		if let Ok(out) = runner.run("ssh", &cat_args) {
			if let Some(remote_port) = parse_remote_port(&out.stdout) {
				return Ok(StartedServer {
					remote_pid: pid,
					remote_port,
					log_path,
				});
			}
			last_detail = if out.stderr.trim().is_empty() {
				out.stdout.trim().to_string()
			} else {
				out.stderr.trim().to_string()
			};
		}
		// Don't sleep after the final attempt.
		if attempt + 1 < poll_attempts {
			sleep(delay);
		}
	}

	// Exhausted: clean up the orphaned server, then report with the last log we
	// managed to read so the failure is diagnosable rather than opaque.
	cleanup_remote(runner, conn, pid);
	let msg = if last_detail.is_empty() {
		"remote start did not report a port in time".to_string()
	} else {
		format!("remote start did not report a port in time: {last_detail}")
	};
	Err(ConnectError::DeployFailed(msg))
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

	/// The exact ssh argv the capability probe builds for [`conn`]. The
	/// version probe now issues this SECOND round-trip whenever the binary is
	/// present, so any test scripting a present-binary probe must also script
	/// (or deliberately leave unscripted -> fail-safe false) this key.
	fn capabilities_args() -> Vec<String> {
		let remote_cmd = crate::ssh::build_remote_capabilities_cmd("aghub-api");
		build_ssh_args(&conn(), &remote_cmd)
	}

	/// A scripted `--capabilities` output advertising forwarding support.
	fn capabilities_ok() -> CommandOutput {
		CommandOutput {
			status_code: Some(0),
			stdout: "AGHUB_API_CAPABILITIES=git-credential-forwarding\n"
				.to_string(),
			stderr: String::new(),
		}
	}

	fn args_as_str(args: &[String]) -> Vec<&str> {
		args.iter().map(|s| s.as_str()).collect()
	}

	/// `uname -sm` stdout mapping to THIS host's (os, arch) so the
	/// same-platform gate passes wherever the test runs. Mirrors the mapping
	/// in `probe_remote_platform` (Linux/Darwin + x86_64/arm64/aarch64).
	///
	/// Only the same-platform tests use this, and those are cfg-gated off
	/// Windows (whose uname vocabulary `normalize_platform` does not map), so
	/// gate the helper too or it is dead code on Windows (`-D warnings`).
	#[cfg(any(target_os = "linux", target_os = "macos"))]
	fn local_uname_stdout() -> String {
		let os = match std::env::consts::OS {
			"linux" => "Linux",
			"macos" => "Darwin",
			other => other,
		};
		let arch = std::env::consts::ARCH;
		format!("{os} {arch}\n")
	}

	fn local_source() -> RemoteInstallSource {
		RemoteInstallSource::LocalBinary("/tmp/aghub-api".into())
	}

	// --- probe_connection --------------------------------------------------

	#[test]
	fn probe_ok_reports_present_compatible() {
		let args = probe_args();
		let caps = capabilities_args();
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&args),
				CommandOutput {
					status_code: Some(0),
					stdout: "aghub-api 1.1.1".to_string(),
					stderr: String::new(),
				},
			)
			.script("ssh", &args_as_str(&caps), capabilities_ok());
		let res = probe_connection(&runner, &conn(), LOCAL);
		assert!(res.reachable);
		assert!(res.api_present);
		assert_eq!(res.api_version.as_deref(), Some("1.1.1"));
		assert!(res.compatible);
		// A capable remote advertises the marker -> forwarding engaged.
		assert!(res.supports_credential_forwarding);
	}

	#[test]
	fn probe_present_old_remote_reports_no_credential_forwarding() {
		// An OLD remote: version probe succeeds (present + compatible), but the
		// capability probe hits the binary's unknown-flag error (non-zero
		// exit). Fail-safe: forwarding NOT engaged even though the version is
		// otherwise compatible.
		let args = probe_args();
		let caps = capabilities_args();
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&args),
				CommandOutput {
					status_code: Some(0),
					stdout: "aghub-api 1.1.1".to_string(),
					stderr: String::new(),
				},
			)
			.script(
				"ssh",
				&args_as_str(&caps),
				CommandOutput {
					status_code: Some(1),
					stdout: String::new(),
					stderr: "unknown flag: --capabilities".to_string(),
				},
			);
		let res = probe_connection(&runner, &conn(), LOCAL);
		assert!(res.reachable);
		assert!(res.api_present);
		assert!(res.compatible, "same major.minor stays compatible");
		assert!(
			!res.supports_credential_forwarding,
			"an old same-version remote must NOT be treated as forwarding-capable"
		);
	}

	#[test]
	fn probe_absent_binary_reports_no_credential_forwarding() {
		// Binary absent (command not found): no capability probe runs and the
		// flag stays false.
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
		assert!(!res.supports_credential_forwarding);
		// Only the version probe ran; no capability probe for an absent binary.
		assert_eq!(runner.calls().len(), 1);
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

	// --- TestResult fixture (shared by serde + ensure_remote_api tests) ----

	fn present_compatible() -> TestResult {
		TestResult::new(
			true,
			true,
			Some("1.1.1".to_string()),
			true,
			String::new(),
		)
	}

	// --- install_remote_api -----------------------------------------------

	// --- ensure_remote_api -------------------------------------------------

	#[test]
	fn ensure_remote_api_local_binary_cross_platform_refuses_before_scp() {
		// Probe: reachable but the binary is missing (command not found).
		let probe = probe_args();
		// Platform probe (`uname -sm`) reports an unmappable platform, so the
		// resolved remote platform does not equal the local OS/arch.
		let uname_args = build_ssh_args(&conn(), "uname -sm");
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&probe),
				CommandOutput {
					status_code: Some(127),
					stdout: String::new(),
					stderr: "bash: aghub-api: command not found".to_string(),
				},
			)
			.script(
				"ssh",
				&args_as_str(&uname_args),
				CommandOutput {
					status_code: Some(0),
					// Windows_NT does not map to the consts vocabulary -> None,
					// which the gate treats as "not the same platform".
					stdout: "Windows_NT x86_64\n".to_string(),
					stderr: String::new(),
				},
			);

		let source = RemoteInstallSource::LocalBinary("/tmp/aghub-api".into());
		let err = ensure_remote_api(&runner, &conn(), LOCAL, Some(&source))
			.expect_err("cross-platform local binary must be refused");
		match err {
			ConnectError::CrossPlatformDeploy { remote_platform } => {
				// `Windows_NT x86_64` does not map to the consts vocabulary, so
				// the probed platform resolves to "unknown".
				assert_eq!(
					remote_platform, "unknown",
					"got {remote_platform:?}"
				);
			}
			other => panic!("expected CrossPlatformDeploy, got {other:?}"),
		}

		// The gate fires BEFORE any mutation: no scp (and no prepare/finish)
		// ever ran.
		let calls = runner.calls();
		assert!(
			!calls.iter().any(|c| c.program == "scp"),
			"no scp may run on a cross-platform refusal: {calls:?}"
		);
	}

	#[test]
	fn ensure_remote_api_present_compatible_returns_ok_without_install() {
		// A shipped build against a VM that already has a compatible aghub-api:
		// the first probe finds it present + compatible, so we return Ok and
		// NEVER attempt an install — even though a source IS available.
		let args = probe_args();
		let caps = capabilities_args();
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&args),
				CommandOutput {
					status_code: Some(0),
					stdout: "aghub-api 1.1.1".to_string(),
					stderr: String::new(),
				},
			)
			.script("ssh", &args_as_str(&caps), capabilities_ok());
		let source = RemoteInstallSource::CargoGit {
			url: "https://github.com/audichuang/aghub.git".to_string(),
			branch: None,
			tag: Some("v1.1.1".to_string()),
		};

		let result = ensure_remote_api(&runner, &conn(), LOCAL, Some(&source))
			.expect("present + compatible api should return Ok");
		assert!(result.api_present);
		assert!(result.compatible);
		assert!(result.supports_credential_forwarding);
		assert!(!result.install_attempted, "no install should be attempted");

		// The version probe + its additive capability probe; no scp / install.
		let calls = runner.calls();
		assert_eq!(
			calls.len(),
			2,
			"version probe + capability probe only: {calls:?}"
		);
		assert!(!calls.iter().any(|c| c.program == "scp"));
	}

	#[test]
	fn ensure_remote_api_absent_and_no_source_is_remote_api_missing() {
		// Reachable, but the binary is absent and no install source is given.
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

		let err = ensure_remote_api(&runner, &conn(), LOCAL, None)
			.expect_err("absent api with no source must fail");
		assert!(
			matches!(err, ConnectError::RemoteApiMissing { .. }),
			"got {err:?}"
		);
		// No install was attempted: only the single probe ran.
		let calls = runner.calls();
		assert_eq!(calls.len(), 1, "only the probe should run: {calls:?}");
	}

	// Same-platform gate: the remote `uname -sm` must normalize to THIS
	// host's (os, arch). `normalize_platform` (ssh.rs) only knows
	// Linux/Darwin, so the Windows release-CI runner cannot satisfy the gate
	// — cfg-gate this same-platform test off Windows. (Real Windows remote
	// deploy is out of scope; the cross-platform test below still runs
	// everywhere.)
	#[cfg(any(target_os = "linux", target_os = "macos"))]
	#[test]
	fn ensure_present_incompatible_local_binary_same_platform_upgrades() {
		// Present-but-INCOMPATIBLE remote binary + LocalBinary source +
		// matching platform: ensure_remote_api must UPGRADE (uname gate ->
		// stage(prepare+scp) -> finish(mv+chmod)) and re-probe.
		//
		// MockRunner keys on (program, args) and the first/second probe build
		// IDENTICAL argv, so we script the probe key INCOMPATIBLE-ONLY: the
		// first probe is present + !compatible (no short-circuit) and the
		// re-probe replays the same incompatible output but api_present=true,
		// so ensure_remote_api returns Ok(second) with install_attempted=true.
		// We assert the side-effects (an scp + a finish ran, install_attempted)
		// — NOT result.compatible, which this single-key seam cannot flip.
		let probe = probe_args();
		let uname_args = build_ssh_args(&conn(), "uname -sm");
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
		let incompatible = || CommandOutput {
			status_code: Some(0),
			stdout: "aghub-api 1.0.0".to_string(),
			stderr: String::new(),
		};
		let ok = || CommandOutput {
			status_code: Some(0),
			stdout: String::new(),
			stderr: String::new(),
		};
		let runner = MockRunner::new()
			.script("ssh", &args_as_str(&probe), incompatible())
			.script(
				"ssh",
				&args_as_str(&uname_args),
				CommandOutput {
					status_code: Some(0),
					stdout: local_uname_stdout(),
					stderr: String::new(),
				},
			)
			.script("ssh", &args_as_str(&prepare_args), ok())
			.script("scp", &args_as_str(&scp_args), ok())
			.script("ssh", &args_as_str(&finish_args), ok());

		let result =
			ensure_remote_api(&runner, &conn(), LOCAL, Some(&local_source()))
				.expect("same-platform upgrade returns Ok(second)");
		assert!(
			result.api_present,
			"re-probe still finds the binary present"
		);
		assert!(
			result.install_attempted,
			"the upgrade install path must have run"
		);

		let calls = runner.calls();
		assert!(
			calls
				.iter()
				.any(|c| c.program == "scp" && c.args == scp_args),
			"the bundled binary must be uploaded on upgrade: {calls:?}"
		);
		assert!(
			calls.iter().any(|c| c.args == finish_args),
			"the staged upload must be moved into place: {calls:?}"
		);
	}

	#[test]
	fn ensure_present_incompatible_no_source_returns_ok_first() {
		// Present-but-incompatible + NO source: unchanged behaviour — return
		// Ok(first) so the caller surfaces the Incompatible screen. No
		// platform probe, no scp; the version probe + its additive capability
		// probe run (two ssh round-trips), but no mutation.
		let probe = probe_args();
		let caps = capabilities_args();
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&probe),
				CommandOutput {
					status_code: Some(0),
					stdout: "aghub-api 1.0.0".to_string(),
					stderr: String::new(),
				},
			)
			.script("ssh", &args_as_str(&caps), capabilities_ok());
		let result = ensure_remote_api(&runner, &conn(), LOCAL, None)
			.expect("present-but-incompatible + no source returns Ok(first)");
		assert!(result.api_present);
		assert!(!result.compatible);
		assert!(!result.install_attempted, "no install with no source");
		// Capability is probed independently of version compatibility.
		assert!(result.supports_credential_forwarding);

		let calls = runner.calls();
		assert_eq!(
			calls.len(),
			2,
			"version probe + capability probe only: {calls:?}"
		);
		assert!(!calls.iter().any(|c| c.program == "scp"));
	}

	#[test]
	fn ensure_present_incompatible_local_binary_cross_platform_refuses() {
		// Present-but-incompatible + LocalBinary source + CROSS-platform
		// remote: cannot deploy the wrong-arch binary, so refuse with
		// CrossPlatformDeploy (same as the absent case) WITHOUT scp, so the
		// desktop shows the manual-install hint instead of a Force-redeploy
		// button that could only fail the same gate.
		let probe = probe_args();
		let uname_args = build_ssh_args(&conn(), "uname -sm");
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&probe),
				CommandOutput {
					status_code: Some(0),
					stdout: "aghub-api 1.0.0".to_string(),
					stderr: String::new(),
				},
			)
			.script(
				"ssh",
				&args_as_str(&uname_args),
				CommandOutput {
					status_code: Some(0),
					// Windows_NT does not map to the consts vocabulary -> None,
					// treated as not-the-same-platform.
					stdout: "Windows_NT x86_64\n".to_string(),
					stderr: String::new(),
				},
			);
		let err =
			ensure_remote_api(&runner, &conn(), LOCAL, Some(&local_source()))
				.expect_err("cross-platform incompatible must refuse");
		assert!(
			matches!(err, ConnectError::CrossPlatformDeploy { .. }),
			"expected CrossPlatformDeploy, got {err:?}"
		);

		// The platform probe must have RUN before refusing, and no scp upload
		// may happen for a wrong-arch binary.
		let calls = runner.calls();
		assert!(
			calls
				.iter()
				.any(|c| c.program == "ssh" && c.args == uname_args),
			"the platform probe must run before refusing: {calls:?}"
		);
		assert!(
			!calls.iter().any(|c| c.program == "scp"),
			"no scp on a cross-platform incompatible binary: {calls:?}"
		);
	}

	// Same-platform gate (see the upgrade test above) — cfg-gate off
	// Windows, whose uname vocabulary `normalize_platform` does not map.
	#[cfg(any(target_os = "linux", target_os = "macos"))]
	#[test]
	fn ensure_absent_same_platform_local_binary_runs_install_then_fails() {
		// Absent (command not found) + LocalBinary + matching platform: the
		// same-platform gate passes and install runs (prepare+scp+finish).
		// MockRunner replays the same probe key, so the SECOND probe is still
		// 127 -> api_present=false -> ensure_remote_api returns DeployFailed.
		// We assert that error AND that the install steps ran first (the
		// single-key seam cannot make the re-probe "present"; the success path
		// is exercised by ..._same_platform_upgrades above).
		let probe = probe_args();
		let uname_args = build_ssh_args(&conn(), "uname -sm");
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
		let ok = || CommandOutput {
			status_code: Some(0),
			stdout: String::new(),
			stderr: String::new(),
		};
		let runner = MockRunner::new()
			.script(
				"ssh",
				&args_as_str(&probe),
				CommandOutput {
					status_code: Some(127),
					stdout: String::new(),
					stderr: "bash: aghub-api: command not found".to_string(),
				},
			)
			.script(
				"ssh",
				&args_as_str(&uname_args),
				CommandOutput {
					status_code: Some(0),
					stdout: local_uname_stdout(),
					stderr: String::new(),
				},
			)
			.script("ssh", &args_as_str(&prepare_args), ok())
			.script("scp", &args_as_str(&scp_args), ok())
			.script("ssh", &args_as_str(&finish_args), ok());

		let err =
			ensure_remote_api(&runner, &conn(), LOCAL, Some(&local_source()))
				.expect_err("re-probe still absent -> DeployFailed");
		assert!(matches!(err, ConnectError::DeployFailed(_)), "got {err:?}");

		// The install ran before the failing re-probe.
		let calls = runner.calls();
		assert!(
			calls
				.iter()
				.any(|c| c.program == "scp" && c.args == scp_args),
			"absent same-platform must scp the binary: {calls:?}"
		);
		assert!(
			calls.iter().any(|c| c.args == finish_args),
			"absent same-platform must run finish: {calls:?}"
		);
	}

	#[test]
	fn install_remote_api_via_cargo_git_runs_remote_cargo_install() {
		let source = RemoteInstallSource::CargoGit {
			url: "https://github.com/audichuang/aghub.git".to_string(),
			branch: Some("feat/remote-ssh-management".to_string()),
			tag: None,
		};
		let install_cmd = build_remote_cargo_install_cmd(
			"https://github.com/audichuang/aghub.git",
			Some("feat/remote-ssh-management"),
			None,
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
	fn install_remote_api_via_release_deb_runs_remote_deb_extract() {
		let source = RemoteInstallSource::ReleaseDeb {
			url: "https://github.com/audichuang/aghub/releases/download/v2.3.2/aghub_2.3.2_amd64.deb"
				.to_string(),
		};
		let install_cmd = build_remote_release_deb_install_cmd(
			"https://github.com/audichuang/aghub/releases/download/v2.3.2/aghub_2.3.2_amd64.deb",
			"~/.local/bin/aghub-api",
		);
		let install_args = build_ssh_args(&conn(), &install_cmd);
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&install_args),
			CommandOutput {
				status_code: Some(0),
				stdout: "aghub-api 2.3.2".to_string(),
				stderr: String::new(),
			},
		);

		install_remote_api(&runner, &conn(), "~/.local/bin/aghub-api", &source)
			.expect("release .deb install should succeed");

		let calls = runner.calls();
		assert_eq!(calls.len(), 1);
		assert_eq!(calls[0].program, "ssh");
		assert_eq!(calls[0].args, install_args);
	}

	#[test]
	fn install_remote_api_cargo_git_refuses_explicit_custom_path() {
		// `cargo install` only writes to ~/.cargo/bin/aghub-api, so a CargoGit
		// deploy to an explicit custom path is refused up front (no remote
		// command runs) instead of installing where the post-install probe
		// would never look.
		let source = RemoteInstallSource::CargoGit {
			url: "https://github.com/audichuang/aghub.git".to_string(),
			branch: None,
			tag: Some("v1.1.1".to_string()),
		};
		let runner = MockRunner::new();
		let err =
			install_remote_api(&runner, &conn(), "/opt/aghub-api", &source)
				.expect_err("cargo-git + custom path must be refused");
		assert!(
			matches!(err, ConnectError::DeployFailed(_)),
			"expected DeployFailed, got {err:?}"
		);
		assert!(
			runner.calls().is_empty(),
			"must refuse before running any remote command: {:?}",
			runner.calls()
		);
	}

	#[test]
	fn force_redeploy_stages_then_finishes_then_probes() {
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
		let caps_args = capabilities_args();
		let runner = MockRunner::new()
			.script("ssh", &args_as_str(&prepare_args), ok())
			.script("scp", &args_as_str(&scp_args), ok())
			.script("ssh", &args_as_str(&finish_args), ver())
			.script("ssh", &args_as_str(&probe_args), ver())
			.script("ssh", &args_as_str(&caps_args), capabilities_ok());

		let result =
			force_redeploy_remote_api(&runner, &conn(), "1.1.1", &source)
				.unwrap();

		assert!(result.compatible);
		assert!(result.api_present);
		assert!(result.supports_credential_forwarding);
		// Strict ordering: the new binary is fully STAGED (prepare + scp),
		// then the staged upload is moved into place (atomic `mv`), then we
		// re-probe (version probe + its additive capability probe). No pkill;
		// the old incompatible server is left orphaned.
		let calls = runner.calls();
		assert_eq!(calls.len(), 5);
		assert_eq!(calls[0].args, prepare_args, "prepare first");
		assert_eq!(calls[1].program, "scp");
		assert_eq!(calls[1].args, scp_args, "scp upload second");
		assert_eq!(calls[2].args, finish_args, "finish (atomic mv) third");
		assert_eq!(calls[3].args, probe_args, "re-probe (version) fourth");
		assert_eq!(calls[4].args, caps_args, "capability probe last");
	}

	#[test]
	fn force_redeploy_staging_failure_aborts_before_finish() {
		// If staging fails (here: scp upload errors), the swap must NOT
		// happen — the remote keeps serving the old binary, no `mv` runs.
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
					status_code: Some(1),
					stdout: String::new(),
					stderr: "scp: connection lost".to_string(),
				},
			);

		let err = force_redeploy_remote_api(&runner, &conn(), "1.1.1", &source)
			.expect_err("staging failure must propagate");
		assert!(matches!(err, ConnectError::DeployFailed(_)), "got {err:?}");

		// No finish (atomic mv) ever ran: the old server is untouched.
		let calls = runner.calls();
		assert!(
			!calls.iter().any(|c| c.args == finish_args),
			"finish must NOT run when staging failed: {calls:?}"
		);
	}

	#[test]
	fn reinstall_remote_api_forces_install_even_when_present() {
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
		let caps_args = capabilities_args();
		let runner = MockRunner::new()
			.script("ssh", &args_as_str(&prepare_args), ok())
			.script("scp", &args_as_str(&scp_args), ok())
			.script("ssh", &args_as_str(&finish_args), ver())
			.script("ssh", &args_as_str(&probe_args), ver())
			.script("ssh", &args_as_str(&caps_args), capabilities_ok());

		let result =
			reinstall_remote_api(&runner, &conn(), "1.1.1", &source).unwrap();

		assert!(result.compatible);
		assert!(result.install_attempted);
		assert!(result.install_succeeded);
		assert!(result.supports_credential_forwarding);
		assert_eq!(
			result.install_message.as_deref(),
			Some("aghub-api reinstalled")
		);
		let calls = runner.calls();
		assert_eq!(
			calls[0].args, prepare_args,
			"upload must be staged before the binary is swapped"
		);
		// prepare + scp + finish + re-probe (version) + capability probe.
		assert_eq!(calls.len(), 5);
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
		// Now a DeployFailed carrying the diagnosable cause, not an opaque
		// StartTimeout.
		assert!(
			matches!(err, ConnectError::DeployFailed(ref m)
				if m.contains("did not report a port in time")),
			"got {err:?}"
		);

		// A guarded kill of pid 4242 must have been issued.
		let expected_kill = kill_args(4242);
		let calls = runner.calls();
		assert!(
            calls.iter().any(|c| c.program == "ssh" && c.args == expected_kill),
            "expected a guarded-kill ssh call for pid 4242, recorded calls: {calls:?}"
        );
	}

	#[test]
	fn start_remote_timeout_carries_last_log_detail() {
		let log = "/run/u/aghub.log";
		let start = start_args();
		let cat = cat_args(log);
		// The log never shows a port, but it DOES carry a panic line that we
		// must surface in the failure message.
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
					stdout: "thread 'main' panicked: boom".to_string(),
					stderr: String::new(),
				},
			);

		let err =
			start_remote(&runner, &conn(), "aghub-api", 2, Duration::ZERO)
				.expect_err("port never appears");
		match err {
			ConnectError::DeployFailed(msg) => {
				assert!(msg.contains("did not report a port in time"));
				assert!(msg.contains("boom"), "should carry last log: {msg}");
			}
			other => panic!("expected DeployFailed, got {other:?}"),
		}
	}

	#[test]
	fn start_remote_missing_logpath_is_deploy_failed_and_reaps_pid() {
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
				.expect_err("missing logpath should fail");
		assert!(
			matches!(err, ConnectError::DeployFailed(ref m)
				if m.contains("did not report a LOGPATH")),
			"got {err:?}"
		);
		// The dangling pid must be reaped via a guarded kill.
		let expected_kill = kill_args(999);
		let calls = runner.calls();
		assert!(
			calls
				.iter()
				.any(|c| c.program == "ssh" && c.args == expected_kill),
			"expected a guarded-kill ssh call for pid 999: {calls:?}"
		);
	}

	#[test]
	fn start_remote_nonzero_exit_returns_deploy_failed_with_stderr() {
		// The start command RAN but exited non-zero (e.g. binary missing a lib)
		// — surface its stderr, not an opaque timeout.
		let start = start_args();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&start),
			CommandOutput {
				status_code: Some(127),
				stdout: String::new(),
				stderr: "aghub-api: error while loading shared libraries"
					.to_string(),
			},
		);
		let err =
			start_remote(&runner, &conn(), "aghub-api", 3, Duration::ZERO)
				.expect_err("non-zero start should fail");
		match err {
			ConnectError::DeployFailed(msg) => {
				assert!(msg.contains("remote start"), "got {msg:?}");
				assert!(
					msg.contains("shared libraries"),
					"should carry stderr: {msg}"
				);
			}
			other => panic!("expected DeployFailed, got {other:?}"),
		}
	}

	#[test]
	fn start_remote_missing_pid_is_deploy_failed() {
		// status 0 but no PID line at all.
		let start = start_args();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&start),
			CommandOutput {
				status_code: Some(0),
				stdout: "weird output, no pid".to_string(),
				stderr: String::new(),
			},
		);
		let err =
			start_remote(&runner, &conn(), "aghub-api", 3, Duration::ZERO)
				.expect_err("missing pid should fail");
		assert!(
			matches!(err, ConnectError::DeployFailed(ref m)
				if m.contains("did not report a PID")),
			"got {err:?}"
		);
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
		// The capability marker crosses IPC as camelCase for the TS side.
		assert_eq!(
			json.get("supportsCredentialForwarding"),
			Some(&false.into())
		);
		assert!(json.get("supports_credential_forwarding").is_none());
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
