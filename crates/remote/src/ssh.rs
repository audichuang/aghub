//! SSH transport foundation: connection model, process abstraction, pure argv
//! builders, remote shell-command composers, and stdout parsers.
//!
//! Everything here is `tauri`-free and (the pure functions) I/O-free so it can
//! be exhaustively unit-tested in the dev sandbox.

use std::fmt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Child, ExitStatus};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Connection model
// ---------------------------------------------------------------------------

/// A user-defined remote target. Field names are snake_case in Rust but
/// (de)serialize as camelCase so the desktop frontend's `sshTarget` /
/// `remoteAghubPath` payload round-trips cleanly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
	pub id: String,
	pub label: String,
	pub ssh_target: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub user: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub port: Option<u16>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub remote_aghub_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Process execution abstraction
// ---------------------------------------------------------------------------

/// Captured output of a finished blocking command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
	pub status_code: Option<i32>,
	pub stdout: String,
	pub stderr: String,
}

/// Errors raised by a [`CommandRunner`]. Plain enum implementing
/// [`std::error::Error`] — deliberately no `thiserror` dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
	/// The child process could not be spawned / launched.
	Spawn(String),
	/// I/O failure while running or collecting output.
	Io(String),
}

impl fmt::Display for RunError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			RunError::Spawn(msg) => write!(f, "failed to spawn process: {msg}"),
			RunError::Io(msg) => write!(f, "process I/O error: {msg}"),
		}
	}
}

impl std::error::Error for RunError {}

/// A handle to a long-lived spawned child (e.g. the SSH tunnel).
///
/// The child is owned behind an `Arc<Mutex<_>>` so the handle is cheaply
/// [`Clone`]able and can be shared between the owning [`RemoteHandle`] and the
/// watcher thread. Methods take a short-lived lock and NEVER hold it across a
/// blocking wait — the watcher polls with [`ChildHandle::try_wait`] instead.
#[derive(Debug, Clone)]
pub struct ChildHandle {
	child: Arc<Mutex<Child>>,
}

impl ChildHandle {
	/// Wrap a freshly spawned [`Child`].
	pub fn new(child: Child) -> Self {
		Self {
			child: Arc::new(Mutex::new(child)),
		}
	}

	/// The OS process id (the kernel pid is stable for the [`Child`]'s
	/// lifetime, even after it exits, so this never blocks meaningfully).
	pub fn pid(&self) -> Option<u32> {
		Some(self.lock().id())
	}

	/// Send a kill signal to the child. Cross-platform (`Child::kill` maps to
	/// `SIGKILL` on Unix and `TerminateProcess` on Windows), and because we
	/// hold the live [`Child`] the OS pid cannot be reused out from under us.
	pub fn kill(&self) -> std::io::Result<()> {
		self.lock().kill()
	}

	/// Non-blocking exit check. Returns `Some(status)` once the child has
	/// exited, `None` while it is still running. Never blocks while holding the
	/// lock, so a concurrent [`ChildHandle::kill`] can always make progress.
	pub fn try_wait(&self) -> std::io::Result<Option<ExitStatus>> {
		self.lock().try_wait()
	}

	/// Lock the inner mutex, recovering a poisoned lock via `into_inner` — a
	/// panic in another holder must not strand the tunnel child forever.
	fn lock(&self) -> std::sync::MutexGuard<'_, Child> {
		self.child.lock().unwrap_or_else(|e| e.into_inner())
	}
}

/// Abstraction over process execution so the bring-up logic can be unit-tested
/// against a mock instead of a real `ssh`/`scp`.
pub trait CommandRunner {
	/// Run `program` with `args`, block until it exits, and capture its output.
	fn run(
		&self,
		program: &str,
		args: &[String],
	) -> Result<CommandOutput, RunError>;

	/// Spawn `program` with `args` as a long-lived child and return its handle.
	fn spawn(
		&self,
		program: &str,
		args: &[String],
	) -> Result<ChildHandle, RunError>;
}

/// Real [`CommandRunner`] backed by [`std::process::Command`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
	fn run(
		&self,
		program: &str,
		args: &[String],
	) -> Result<CommandOutput, RunError> {
		let mut command = std::process::Command::new(program);
		command.args(args);
		#[cfg(windows)]
		command.creation_flags(crate::CREATE_NO_WINDOW);
		let output = command
			.output()
			.map_err(|e| RunError::Spawn(e.to_string()))?;
		Ok(CommandOutput {
			status_code: output.status.code(),
			stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
			stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
		})
	}

	fn spawn(
		&self,
		program: &str,
		args: &[String],
	) -> Result<ChildHandle, RunError> {
		let mut command = std::process::Command::new(program);
		command.args(args);
		#[cfg(windows)]
		command.creation_flags(crate::CREATE_NO_WINDOW);
		let child = command
			.spawn()
			.map_err(|e| RunError::Spawn(e.to_string()))?;
		Ok(ChildHandle::new(child))
	}
}

// ---------------------------------------------------------------------------
// Pure argv builders (positional args, NEVER a joined shell string)
// ---------------------------------------------------------------------------

/// Build the argv for `ssh <conn> bash -lc <decoded remote_cmd>`.
///
/// OpenSSH runs the command through the account's login shell. Users often use
/// fish/zsh there, while our remote scripts are bash. The command seen by the
/// login shell is a fixed bash wrapper plus a base64 payload, so the
/// login shell never has to parse the real script's quoting, semicolons,
/// or expansions.
pub fn build_ssh_args(conn: &Connection, remote_cmd: &str) -> Vec<String> {
	let mut args = vec![
		"-o".to_string(),
		"BatchMode=yes".to_string(),
		"-o".to_string(),
		"ConnectTimeout=10".to_string(),
		"-o".to_string(),
		"StrictHostKeyChecking=accept-new".to_string(),
	];
	if let Some(port) = conn.port {
		args.push("-p".to_string());
		args.push(port.to_string());
	}
	if let Some(user) = &conn.user {
		args.push("-l".to_string());
		args.push(user.clone());
	}
	args.push(conn.ssh_target.clone());
	args.push(build_bash_wrapped_cmd(remote_cmd));
	args
}

fn build_bash_wrapped_cmd(remote_cmd: &str) -> String {
	let encoded = base64_encode(remote_cmd.as_bytes());
	format!("bash -lc 'eval \"$(printf %s {encoded} | base64 -d)\"'")
}

fn base64_encode(bytes: &[u8]) -> String {
	const TABLE: &[u8; 64] =
		b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
	for chunk in bytes.chunks(3) {
		let b0 = chunk[0];
		let b1 = *chunk.get(1).unwrap_or(&0);
		let b2 = *chunk.get(2).unwrap_or(&0);
		out.push(TABLE[(b0 >> 2) as usize] as char);
		out.push(
			TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char,
		);
		if chunk.len() > 1 {
			out.push(
				TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char,
			);
		} else {
			out.push('=');
		}
		if chunk.len() > 2 {
			out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
		} else {
			out.push('=');
		}
	}
	out
}

/// Build the argv for `scp <local> <user@target:remote>`.
///
/// Note `scp` uses an UPPERCASE `-P` for the port (unlike `ssh`).
pub fn build_scp_args(
	conn: &Connection,
	local: &str,
	remote: &str,
) -> Vec<String> {
	let mut args = vec![
		"-o".to_string(),
		"BatchMode=yes".to_string(),
		"-o".to_string(),
		"ConnectTimeout=10".to_string(),
		"-o".to_string(),
		"StrictHostKeyChecking=accept-new".to_string(),
	];
	if let Some(port) = conn.port {
		args.push("-P".to_string());
		args.push(port.to_string());
	}
	args.push(local.to_string());
	let dest = match &conn.user {
		Some(user) => format!("{user}@{}:{remote}", conn.ssh_target),
		None => format!("{}:{remote}", conn.ssh_target),
	};
	args.push(dest);
	args
}

/// Remote upload target used before an install step moves it into place.
///
/// `nonce` must be a per-install unique suffix (see `bringup::install_nonce`)
/// so two installs racing against the same remote account never share (and
/// corrupt) one fixed staging file: scp + finish are separate round-trips,
/// so a fixed path lets install A validate the staged file just as install B
/// overwrites it mid-copy, then A renames B's truncated upload into place.
pub fn remote_api_upload_path(nonce: &str) -> String {
	format!(".cache/aghub/aghub-api.upload.{nonce}")
}

/// Build the argv for an SSH loopback port-forward tunnel.
///
/// The forward is explicitly bound to `127.0.0.1` on both ends regardless of
/// the server's `GatewayPorts` setting.
///
/// **Multiplexing is disabled deliberately, and the tunnel's whole lifecycle
/// depends on it.** With a user `ControlMaster auto` + `ControlPersist` entry
/// for the host — a common, sensible thing to have in `~/.ssh/config` — this
/// process would not open its own connection at all: it hands the forward to
/// the existing master and **exits 0 immediately**, while the forward stays up
/// under the master. Every caller here reads the child process as the tunnel
/// (bring-up treats an early exit as a failed forward, the watcher treats its
/// exit as a disconnect, and teardown kills it to close the forward), so all
/// three silently mean the wrong thing — bring-up reported
/// `ssh tunnel exited early (exit status: 0)` and tore down a remote server
/// whose forward was in fact working. `ControlPath=none` restores the
/// invariant that this child owns the forward; `ControlMaster=no` keeps it from
/// becoming a master for anyone else. Only the TUNNEL needs this — the one-shot
/// remote commands are free to reuse the user's master.
pub fn build_tunnel_args(
	conn: &Connection,
	local_port: u16,
	remote_port: u16,
) -> Vec<String> {
	let mut args = vec![
		"-N".to_string(),
		"-o".to_string(),
		"BatchMode=yes".to_string(),
		"-o".to_string(),
		"ConnectTimeout=10".to_string(),
		"-o".to_string(),
		"StrictHostKeyChecking=accept-new".to_string(),
		"-o".to_string(),
		"ExitOnForwardFailure=yes".to_string(),
		"-o".to_string(),
		"ControlPath=none".to_string(),
		"-o".to_string(),
		"ControlMaster=no".to_string(),
		"-L".to_string(),
		format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"),
	];
	if let Some(port) = conn.port {
		args.push("-p".to_string());
		args.push(port.to_string());
	}
	if let Some(user) = &conn.user {
		args.push("-l".to_string());
		args.push(user.clone());
	}
	args.push(conn.ssh_target.clone());
	args
}

// ---------------------------------------------------------------------------
// Remote shell-command composition (single re-parsed-by-login-shell strings)
// ---------------------------------------------------------------------------

/// Wrap `s` in single quotes, escaping each embedded single quote as the
/// four-character sequence `'\''` so the result is a single inert shell token.
pub fn shell_quote_single(s: &str) -> String {
	let mut out = String::with_capacity(s.len() + 2);
	out.push('\'');
	for ch in s.chars() {
		if ch == '\'' {
			// close quote, escaped literal quote, reopen quote
			out.push_str("'\\''");
		} else {
			out.push(ch);
		}
	}
	out.push('\'');
	out
}

/// Keep only `[A-Za-z0-9_-]` from `id` (for use in a log file name).
fn sanitize_id(id: &str) -> String {
	id.chars()
		.filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
		.collect()
}

/// POSIX shell snippet that resolves the default `aghub-api` binary. Non-login
/// SSH commands frequently miss `~/.cargo/bin`, especially with fish login
/// shells, so check the common install locations before giving up. Also
/// checks the two standard linuxbrew prefixes (system-wide and per-user) since
/// a non-login shell has no Homebrew `PATH` either.
fn default_api_path_script() -> &'static str {
	"if command -v aghub-api >/dev/null 2>&1; then \
	     command -v aghub-api; \
	 elif [ -x \"$HOME/.cargo/bin/aghub-api\" ]; then \
	     printf '%s\\n' \"$HOME/.cargo/bin/aghub-api\"; \
	 elif [ -x \"$HOME/.local/bin/aghub-api\" ]; then \
	     printf '%s\\n' \"$HOME/.local/bin/aghub-api\"; \
	 elif [ -x \"/home/linuxbrew/.linuxbrew/bin/aghub-api\" ]; then \
	     printf '%s\\n' \"/home/linuxbrew/.linuxbrew/bin/aghub-api\"; \
	 elif [ -x \"$HOME/.linuxbrew/bin/aghub-api\" ]; then \
	     printf '%s\\n' \"$HOME/.linuxbrew/bin/aghub-api\"; \
	 else \
	     printf '%s\\n' aghub-api; \
	 fi"
}

/// Build a POSIX assignment that stores the resolved aghub-api path in `$bin`.
///
/// An explicit `~/…` path expands `$HOME` so the probe and start resolve the
/// SAME location the install target writes to (see
/// [`assign_install_target_cmd`]); without this, an explicit
/// `~/.local/bin/aghub-api` would be installed to `$HOME/.local/bin/aghub-api`
/// but probed/started as a literal `~/…` token the remote shell never expands,
/// so a freshly installed binary would never be found. Any other path is
/// single-quote escaped verbatim.
fn assign_api_bin_cmd(resolved_path: &str) -> String {
	if resolved_path == "aghub-api" {
		format!("bin=\"$({})\";", default_api_path_script())
	} else if let Some(rest) = resolved_path.strip_prefix("~/") {
		format!("bin=\"$HOME\"/{};", shell_quote_single(rest))
	} else {
		format!("bin={};", shell_quote_single(resolved_path))
	}
}

/// POSIX shell snippet that resolves WHERE to install the default `aghub-api`:
/// overwrite the existing binary the probe would resolve (so a
/// `cargo install`-ed `~/.cargo/bin/aghub-api` is upgraded IN PLACE rather than
/// shadowed by a new `~/.local/bin` copy), falling back to
/// `~/.local/bin/aghub-api` for a fresh install.
///
/// Deliberately NOT a full mirror of [`default_api_path_script`]: this script
/// omits the two linuxbrew fallback branches, so a binary the PROBE resolved
/// from a linuxbrew prefix is NOT overwritten in place — an upgrade instead
/// installs a fresh copy at `~/.local/bin/aghub-api`, leaving the older
/// linuxbrew binary on disk (now shadowed on `PATH` by the new default
/// location). This asymmetry is a deliberate, tracked deferral, not an
/// oversight — see
/// `.scratch/remote-version-hardening/issues/01-install-target-linuxbrew-parity.md`.
fn default_install_target_script() -> &'static str {
	"if command -v aghub-api >/dev/null 2>&1; then \
	     command -v aghub-api; \
	 elif [ -x \"$HOME/.cargo/bin/aghub-api\" ]; then \
	     printf '%s\\n' \"$HOME/.cargo/bin/aghub-api\"; \
	 elif [ -x \"$HOME/.local/bin/aghub-api\" ]; then \
	     printf '%s\\n' \"$HOME/.local/bin/aghub-api\"; \
	 else \
	     printf '%s\\n' \"$HOME/.local/bin/aghub-api\"; \
	 fi"
}

fn assign_install_target_cmd(resolved_path: &str) -> String {
	if resolved_path == "aghub-api" {
		format!("target=\"$({})\";", default_install_target_script())
	} else if let Some(rest) = resolved_path.strip_prefix("~/") {
		format!("target=\"$HOME\"/{};", shell_quote_single(rest))
	} else {
		format!("target={};", shell_quote_single(resolved_path))
	}
}

/// Compose the remote preparation command for an scp upload.
pub fn build_remote_prepare_upload_cmd() -> String {
	"mkdir -p \"$HOME/.cache/aghub\"".to_string()
}

/// Compose the remote install command that validates the STAGED upload
/// before it ever touches `$target`: chmod it executable and run `--version`
/// on the staged path itself.
///
/// The staged file scp uploaded normally lives under `$HOME/.cache`, which
/// can be a DIFFERENT filesystem than `$target`'s directory — a cross-fs
/// `mv` silently falls back to copy+remove, so a failure partway through
/// (ENOSPC, I/O error) can destroy `$target` before the copy completes. To
/// keep the swap atomic regardless of filesystem layout, this stages a
/// SECOND copy into `$target`'s own directory (a `$target.tmp.<nonce>`
/// sibling) and `mv`s THAT into `$target` — a same-directory rename is
/// always an atomic inode swap on the same filesystem, never a partial
/// copy+remove.
///
/// A failing self-check (corrupt download, glibc/ABI mismatch) exits
/// non-zero WITHOUT ever `cp`/`mv`-ing into `$target`, so the previously
/// working `$target` binary is never destroyed by a bad upload. (A
/// `noexec`-mounted staging dir fails the same way — the exec attempt itself
/// errors — so that degenerate case is fail-safe too.) `tmp` is initialized
/// to empty up front, and the failure cleanup guards on `[ -n "$tmp" ]`
/// before removing it — a login-shell-inherited `tmp` from `bash -lc` must
/// never be deleted just because an earlier step in this script failed
/// before `tmp` was ever assigned (see `build_remote_release_deb_install_cmd`
/// for the same hygiene applied to `$stage`). `nonce` must be the SAME value
/// passed to [`remote_api_upload_path`] for this install, so the staged path
/// referenced here is the one scp actually uploaded to.
pub fn build_remote_finish_upload_cmd(
	resolved_path: &str,
	nonce: &str,
) -> String {
	let target_assignment = assign_install_target_cmd(resolved_path);
	let staged = format!("$HOME/.cache/aghub/aghub-api.upload.{nonce}");
	format!(
		"{target_assignment} \
	     tmp=\"\"; \
	     chmod 755 \"{staged}\" && \
	     \"{staged}\" --version && \
	     mkdir -p \"$(dirname -- \"$target\")\" && \
	     tmp=\"$target.tmp.{nonce}\" && \
	     cp -- \"{staged}\" \"$tmp\" && \
	     chmod 755 \"$tmp\" && \
	     mv -f -- \"$tmp\" \"$target\" && \
	     rm -f -- \"{staged}\" \
	     || {{ [ -n \"$tmp\" ] && rm -f -- \"$tmp\"; rm -f -- \"{staged}\"; exit 1; }}"
	)
}

/// Compose a remote install command that downloads a Linux `.deb` release
/// bundle, extracts the packaged `aghub-api`, and installs it at the same path
/// the probe/start commands resolve.
///
/// Validates the extracted binary BEFORE it replaces `$target`: the staged
/// copy is chmod'd executable and self-checked with `--version` in place; only
/// a passing check `mv`s it onto `$target`, and a failing one removes the
/// stray staged file and exits non-zero without touching `$target`. (A
/// `noexec`-mounted target dir fails the self-check the same way, so that
/// degenerate case is fail-safe too.)
///
/// `stage` is initialized to empty at the VERY start of the script, before
/// anything else runs. Without this, a failure early in the chain (e.g. the
/// `mkdir -p "$target_dir"` before `stage` is ever assigned) would still
/// reach the `|| { rm -f "$stage"; ... }` cleanup — and under `bash -lc`
/// (the login shell this whole script runs through), an UNSET `stage` falls
/// back to whatever the login environment happens to bind that name to
/// (e.g. a stray `stage=~/.ssh/authorized_keys` in some profile script),
/// which `rm -f` would then delete. The cleanup itself also guards on
/// `[ -n "$stage" ]` so it never fires on an empty/unset value.
pub fn build_remote_release_deb_install_cmd(
	deb_url: &str,
	resolved_path: &str,
) -> String {
	let target_assignment = assign_install_target_cmd(resolved_path);
	let quoted_url = shell_quote_single(deb_url);
	format!(
		"{target_assignment} \
	     stage=\"\"; \
	     command -v dpkg-deb >/dev/null 2>&1 || {{ echo 'dpkg-deb not found' >&2; exit 127; }}; \
	     tmp=\"$(mktemp -d)\"; \
	     cleanup() {{ rm -rf \"$tmp\"; }}; \
	     trap cleanup EXIT; \
	     deb=\"$tmp/aghub.deb\"; \
	     if command -v curl >/dev/null 2>&1; then \
	         curl -fsSL --connect-timeout 10 --max-time 120 -o \"$deb\" {quoted_url}; \
	     elif command -v wget >/dev/null 2>&1; then \
	         wget -q -T 120 -O \"$deb\" {quoted_url}; \
	     else \
	         echo 'curl or wget not found' >&2; exit 127; \
	     fi; \
	     root=\"$tmp/root\"; mkdir -p \"$root\"; \
	     dpkg-deb -x \"$deb\" \"$root\"; \
	     bin=\"$(find \"$root\" -type f \\( -path '*/resources/binaries/aghub-api' -o -path '*/binaries/aghub-api' \\) | head -n 1)\"; \
	     if [ -z \"$bin\" ]; then bin=\"$(find \"$root\" -type f -name aghub-api | head -n 1)\"; fi; \
	     if [ -z \"$bin\" ]; then echo 'aghub-api not found in release package' >&2; exit 1; fi; \
	     target_dir=\"$(dirname -- \"$target\")\"; \
	     mkdir -p \"$target_dir\" && \
	     stage=\"$(mktemp \"$target_dir/.aghub-api.XXXXXX\")\" && \
	     cp \"$bin\" \"$stage\" && \
	     chmod 755 \"$stage\" && \
	     \"$stage\" --version && \
	     mv \"$stage\" \"$target\" || {{ [ -n \"$stage\" ] && rm -f -- \"$stage\"; exit 1; }}"
	)
}

/// Compose a remote install command that builds `aghub-api` on the VM itself.
///
/// Stamps `AGHUB_RELEASE_VERSION=<release_version>` in the cargo invocation's
/// own environment (the VAR=... prefix applies to `cargo` AND the `aghub-api`
/// build script it runs). A `cargo install --git` checkout has no tag refs, so
/// the build script's `git describe` fallback fails there and it falls all the
/// way back to the workspace manifest placeholder (`1.1.1`) — a version the
/// desktop never considers compatible, so it reinstalls forever. Stamping the
/// desktop's own version here breaks that loop.
///
/// When an explicit `tag` is being installed AND no `branch` is also given,
/// that tag names a SPECIFIC release which may differ from the desktop's own
/// `release_version` — in that case stamp the tag's version (leading `v`
/// stripped) instead, so the VM build reports the version it actually built
/// rather than falsely advertising the desktop's. The stamp mirrors the SAME
/// `branch`-beats-`tag` precedence `ref_arg` uses below: a `tag` is only the
/// ref actually installed when `branch` is `None`, so a call supplying BOTH
/// installs the branch but must stamp `release_version`, not the (unused)
/// tag's version. `branch`/no-ref installs have no pinned version, so they
/// keep stamping `release_version`.
pub fn build_remote_cargo_install_cmd(
	git_url: &str,
	branch: Option<&str>,
	tag: Option<&str>,
	release_version: &str,
) -> String {
	let ref_arg = branch
		.map(|branch| format!(" --branch {}", shell_quote_single(branch)))
		.or_else(|| {
			tag.map(|tag| format!(" --tag {}", shell_quote_single(tag)))
		})
		.unwrap_or_default();
	// Same precedence as `ref_arg`: `tag` only names the ref actually used
	// when `branch` is absent.
	let stamp_version = tag
		.filter(|_| branch.is_none())
		.map(|t| t.strip_prefix('v').unwrap_or(t))
		.unwrap_or(release_version);
	format!(
		"cargo_bin=\"$(command -v cargo || true)\"; \
	     if [ -z \"$cargo_bin\" ] && [ -x \"$HOME/.cargo/bin/cargo\" ]; then cargo_bin=\"$HOME/.cargo/bin/cargo\"; fi; \
	     if [ -z \"$cargo_bin\" ]; then echo 'cargo not found' >&2; exit 127; fi; \
	     command -v git >/dev/null 2>&1 || {{ echo 'git not found' >&2; exit 127; }}; \
	     AGHUB_RELEASE_VERSION={} \"$cargo_bin\" install --git {}{} aghub-api --bin aghub-api --force",
		shell_quote_single(stamp_version),
		shell_quote_single(git_url),
		ref_arg
	)
}

/// Compose the single remote shell command that starts `aghub-api` detached.
///
/// The resolved binary path is single-quote escaped, so any dangerous
/// characters in it land inside the quoted region and stay inert. The command
/// echoes `PID=<pid>` and `LOGPATH=<path>` so the caller can parse them.
pub fn build_remote_start_cmd(resolved_path: &str, conn_id: &str) -> String {
	let bin_assignment = assign_api_bin_cmd(resolved_path);
	let safe_id = sanitize_id(conn_id);
	format!(
		"{bin_assignment} \
	     d=\"${{XDG_RUNTIME_DIR:-$HOME/.cache/aghub}}\"; \
         mkdir -p -m 700 \"$d\"; \
         log=\"$d/aghub-api.{safe_id}.log\"; \
         : > \"$log\"; chmod 600 \"$log\"; \
         nohup bash -lc 'exec \"$1\" --port 0' bash \"$bin\" >\"$log\" 2>&1 & \
         echo PID=$!; echo LOGPATH=\"$log\""
	)
}

/// Compose `cat <log_path>` with the path single-quote escaped.
pub fn build_remote_cat_cmd(log_path: &str) -> String {
	format!("cat {}", shell_quote_single(log_path))
}

/// Compose a guarded remote kill that only fires if the pid is alive AND its
/// process command name contains `aghub-api` (defends against PID reuse).
pub fn build_remote_kill_cmd(pid: u32) -> String {
	format!("kill -0 {pid} 2>/dev/null && ps -o comm= -p {pid} | grep -q aghub-api && kill {pid}")
}

/// Compose the probe command `<bin> --version` with the path escaped.
pub fn build_remote_probe_cmd(resolved_path: &str) -> String {
	format!("{} \"$bin\" --version", assign_api_bin_cmd(resolved_path))
}

/// Stdout line prefix `aghub-api --capabilities` emits. Cross-crate contract
/// mirrored from `aghub_api::cli::CAPABILITIES_LINE_PREFIX` (this crate does
/// NOT depend on `aghub-api`, exactly like the `AGHUB_API_PORT=` contract). A
/// test in each crate locks the literal so drift breaks a test.
pub const CAPABILITY_LINE_PREFIX: &str = "AGHUB_API_CAPABILITIES=";

/// Capability token advertising controller-side git-credential forwarding
/// (mirrors `aghub_api::cli::CAP_GIT_CREDENTIAL_FORWARDING`).
pub const CAP_GIT_CREDENTIAL_FORWARDING: &str = "git-credential-forwarding";

/// Compose the capability probe `<bin> --capabilities` with the path escaped.
///
/// An `aghub-api` predating this feature has no `--capabilities` flag, so it
/// exits non-zero / prints an unknown-flag error — the caller treats that as
/// "capability unsupported" (fail-safe).
pub fn build_remote_capabilities_cmd(resolved_path: &str) -> String {
	format!(
		"{} \"$bin\" --capabilities",
		assign_api_bin_cmd(resolved_path)
	)
}

/// Does `s` advertise the named capability `token`? True only when a
/// well-formed `AGHUB_API_CAPABILITIES=` line (left-anchored at line start or
/// after whitespace) lists `token` among its space-separated tokens. Anything
/// else — no line, empty list, a different token — is `false`.
pub fn capability_line_advertises(s: &str, token: &str) -> bool {
	for line in s.lines() {
		if let Some(rest) = key_value_rest(line, CAPABILITY_LINE_PREFIX) {
			if rest.split_whitespace().any(|t| t == token) {
				return true;
			}
		}
	}
	false
}

// ---------------------------------------------------------------------------
// Stdout parsers
// ---------------------------------------------------------------------------

/// Extract `<n>` from a `AGHUB_API_PORT=<n>` line.
pub fn parse_remote_port(s: &str) -> Option<u16> {
	parse_kv_after(s, "AGHUB_API_PORT=").and_then(|v| v.parse::<u16>().ok())
}

/// Extract `<n>` from a `PID=<n>` line.
pub fn parse_pid(s: &str) -> Option<u32> {
	parse_kv_after(s, "PID=").and_then(|v| v.parse::<u32>().ok())
}

/// Extract `<path>` (to end of line) from a `LOGPATH=<path>` line. The key must
/// be at the start of a (trimmed) line or follow whitespace, so a longer token
/// ending in `LOGPATH=` cannot be mis-parsed.
pub fn parse_logpath(s: &str) -> Option<String> {
	for line in s.lines() {
		if let Some(rest) = key_value_rest(line, "LOGPATH=") {
			return Some(rest.to_string());
		}
	}
	None
}

/// Extract `<semver>` from a `aghub-api <semver>` version line.
pub fn parse_api_version(s: &str) -> Option<String> {
	for line in s.lines() {
		let trimmed = line.trim();
		if let Some(rest) = trimmed.strip_prefix("aghub-api ") {
			let ver = rest.trim();
			if !ver.is_empty() {
				return Some(ver.to_string());
			}
		}
	}
	None
}

/// v1 compatibility rule: equal `major.minor`.
pub fn is_version_compatible(local: &str, remote: &str) -> bool {
	match (major_minor(local), major_minor(remote)) {
		(Some(l), Some(r)) => l == r,
		_ => false,
	}
}

/// Normalize `uname -s`/`uname -m` output into the
/// `std::env::consts::{OS, ARCH}` vocabulary so a remote platform can be
/// compared to the desktop's own. Returns
/// `None` for anything not mappable (the caller treats that as cross-platform).
pub fn normalize_platform(
	uname_s: &str,
	uname_m: &str,
) -> Option<(String, String)> {
	let os = match uname_s.trim() {
		"Darwin" => "macos",
		"Linux" => "linux",
		_ => return None,
	};
	let arch = match uname_m.trim() {
		"arm64" | "aarch64" => "aarch64",
		"x86_64" | "amd64" => "x86_64",
		_ => return None,
	};
	Some((os.to_string(), arch.to_string()))
}

/// Probe the remote platform via `uname -sm`, normalized to the
/// `std::env::consts` vocabulary. `None` on transport failure, a non-zero
/// remote exit, or an unmappable platform — the caller treats that as "not the
/// same platform" and refuses a cross-platform redeploy.
pub fn probe_remote_platform<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
) -> Option<(String, String)> {
	let args = build_ssh_args(conn, "uname -sm");
	let out = runner.run("ssh", &args).ok()?;
	if out.status_code != Some(0) {
		return None;
	}
	let mut tokens = out.stdout.split_whitespace();
	let s = tokens.next()?;
	let m = tokens.next()?;
	normalize_platform(s, m)
}

/// Probe whether the remote `aghub-api` advertises git-credential forwarding,
/// over SSH and WITHOUT a running HTTP server, by running `<bin>
/// --capabilities` and looking for the [`CAP_GIT_CREDENTIAL_FORWARDING`] token.
///
/// **Fail-safe:** returns `false` on ANY uncertainty — transport failure, a
/// non-zero remote exit (an old binary that lacks the flag prints an
/// unknown-flag error and exits non-zero), or output missing the marker. It
/// never falsely claims support, so the desktop only forwards the credential
/// header to a remote that genuinely honors it.
pub fn probe_supports_credential_forwarding<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	resolved_path: &str,
) -> bool {
	let remote_cmd = build_remote_capabilities_cmd(resolved_path);
	let args = build_ssh_args(conn, &remote_cmd);
	let Ok(out) = runner.run("ssh", &args) else {
		return false;
	};
	// Only trust a clean exit; a non-zero status (incl. ssh transport 255 or an
	// old binary's unknown-flag error) means we cannot confirm support.
	if out.status_code != Some(0) {
		return false;
	}
	capability_line_advertises(&out.stdout, CAP_GIT_CREDENTIAL_FORWARDING)
}

/// Find the first whitespace/EOL-terminated token after `key` on any
/// line, where `key` is anchored to the line start or to a whitespace
/// boundary (so e.g. `OLDPID=9` is NOT matched when looking for `PID=`).
fn parse_kv_after(s: &str, key: &str) -> Option<String> {
	for line in s.lines() {
		if let Some(rest) = key_value_rest(line, key) {
			let token: String =
				rest.chars().take_while(|c| !c.is_whitespace()).collect();
			if !token.is_empty() {
				return Some(token);
			}
		}
	}
	None
}

/// Return the substring after `key` on `line` iff `key` appears at the
/// start of the trimmed line or immediately after whitespace (a left word
/// boundary), so a longer token whose suffix is `key` (e.g. `OLDPID=`
/// for `PID=`) is rejected.
fn key_value_rest<'a>(line: &'a str, key: &str) -> Option<&'a str> {
	let trimmed = line.trim_start();
	if let Some(rest) = trimmed.strip_prefix(key) {
		return Some(rest);
	}
	let (before, after) = trimmed.split_once(key)?;
	if before.ends_with(char::is_whitespace) {
		Some(after)
	} else {
		None
	}
}

/// Parse the `(major, minor)` pair from a semver-ish string.
fn major_minor(v: &str) -> Option<(u64, u64)> {
	let mut parts = v.trim().split('.');
	let major = parts.next()?.parse::<u64>().ok()?;
	let minor = parts.next()?.parse::<u64>().ok()?;
	Some((major, minor))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_support::MockRunner;

	fn conn_full() -> Connection {
		Connection {
			id: "vm-1".to_string(),
			label: "My VM".to_string(),
			ssh_target: "my-vm".to_string(),
			user: Some("alice".to_string()),
			port: Some(2222),
			remote_aghub_path: Some("/opt/aghub-api".to_string()),
		}
	}

	fn conn_minimal() -> Connection {
		Connection {
			id: "vm-2".to_string(),
			label: "Bare".to_string(),
			ssh_target: "example.com".to_string(),
			user: None,
			port: None,
			remote_aghub_path: None,
		}
	}

	fn assert_bash_wrapped(remote_cmd: &str) {
		assert!(
				remote_cmd.starts_with("bash -lc 'eval \"$(printf %s "),
				"remote command should enter bash through a fixed wrapper: {remote_cmd}"
			);
		assert!(
			remote_cmd.ends_with(" | base64 -d)\"'"),
			"remote command should decode the script inside bash: {remote_cmd}"
		);
	}

	// --- Connection serde round-trip (camelCase) ---------------------------

	#[test]
	fn connection_serializes_camel_case() {
		let conn = conn_full();
		let json = serde_json::to_value(&conn).unwrap();
		assert_eq!(json["sshTarget"], "my-vm");
		assert_eq!(json["remoteAghubPath"], "/opt/aghub-api");
		assert!(json.get("ssh_target").is_none());
		assert!(json.get("remote_aghub_path").is_none());
	}

	#[test]
	fn connection_deserializes_camel_case() {
		let json = r#"{
            "id": "vm-1",
            "label": "My VM",
            "sshTarget": "my-vm",
            "user": "alice",
            "port": 2222,
            "remoteAghubPath": "/opt/aghub-api"
        }"#;
		let conn: Connection = serde_json::from_str(json).unwrap();
		assert_eq!(conn, conn_full());
	}

	#[test]
	fn connection_round_trips_through_json() {
		let conn = conn_full();
		let json = serde_json::to_string(&conn).unwrap();
		let back: Connection = serde_json::from_str(&json).unwrap();
		assert_eq!(conn, back);
	}

	#[test]
	fn connection_deserializes_minimal_without_optionals() {
		let json = r#"{ "id": "x", "label": "L", "sshTarget": "h" }"#;
		let conn: Connection = serde_json::from_str(json).unwrap();
		assert_eq!(conn.user, None);
		assert_eq!(conn.port, None);
		assert_eq!(conn.remote_aghub_path, None);
	}

	// --- build_ssh_args ----------------------------------------------------

	#[test]
	fn ssh_args_full() {
		let args = build_ssh_args(&conn_full(), "echo hi");
		assert_eq!(
			&args[..args.len() - 1],
			[
				"-o",
				"BatchMode=yes",
				"-o",
				"ConnectTimeout=10",
				"-o",
				"StrictHostKeyChecking=accept-new",
				"-p",
				"2222",
				"-l",
				"alice",
				"my-vm",
			]
		);
		assert_bash_wrapped(args.last().unwrap());
	}

	#[test]
	fn ssh_args_minimal() {
		let args = build_ssh_args(&conn_minimal(), "uname -a");
		assert_eq!(
			&args[..args.len() - 1],
			[
				"-o",
				"BatchMode=yes",
				"-o",
				"ConnectTimeout=10",
				"-o",
				"StrictHostKeyChecking=accept-new",
				"example.com",
			]
		);
		assert_bash_wrapped(args.last().unwrap());
	}

	#[test]
	fn ssh_args_contain_batchmode() {
		let args = build_ssh_args(&conn_minimal(), "x");
		assert!(args.iter().any(|a| a == "BatchMode=yes"));
		assert!(args.iter().any(|a| a == "StrictHostKeyChecking=accept-new"));
	}

	#[test]
	fn ssh_args_hostile_target_stays_one_argv_element() {
		let mut conn = conn_minimal();
		conn.ssh_target = "h; rm -rf /".to_string();
		let args = build_ssh_args(&conn, "cmd");
		assert!(args.contains(&"h; rm -rf /".to_string()));
		// The hostile target is exactly one element; nothing got split.
		assert_eq!(args.iter().filter(|a| a.contains("rm -rf")).count(), 1);
	}

	#[test]
	fn ssh_args_remote_cmd_is_single_element() {
		let args = build_ssh_args(&conn_minimal(), "a && b; c");
		assert_bash_wrapped(args.last().unwrap());
	}

	#[test]
	fn ssh_args_wrap_remote_cmd_in_posix_shell() {
		let args = build_ssh_args(&conn_minimal(), "echo hi");
		assert_bash_wrapped(args.last().unwrap());
	}

	#[test]
	fn ssh_args_do_not_expose_script_to_login_shell_parser() {
		let args = build_ssh_args(&conn_minimal(), "echo 'hi' && echo fish");
		let remote = args.last().unwrap();
		assert_bash_wrapped(remote);
		assert!(!remote.contains("echo 'hi'"));
		assert!(!remote.contains("&& echo fish"));
	}

	// --- build_scp_args ----------------------------------------------------

	#[test]
	fn scp_args_full_uses_uppercase_port_and_user_at_target() {
		let args =
			build_scp_args(&conn_full(), "./bin", "~/.local/bin/aghub-api");
		assert_eq!(
			args,
			vec![
				"-o",
				"BatchMode=yes",
				"-o",
				"ConnectTimeout=10",
				"-o",
				"StrictHostKeyChecking=accept-new",
				"-P",
				"2222",
				"./bin",
				"alice@my-vm:~/.local/bin/aghub-api"
			]
		);
	}

	#[test]
	fn scp_args_minimal_no_user() {
		let args = build_scp_args(&conn_minimal(), "./bin", "/tmp/x");
		assert_eq!(
			args,
			vec![
				"-o",
				"BatchMode=yes",
				"-o",
				"ConnectTimeout=10",
				"-o",
				"StrictHostKeyChecking=accept-new",
				"./bin",
				"example.com:/tmp/x"
			]
		);
	}

	#[test]
	fn scp_args_uses_uppercase_p_not_lowercase() {
		let args = build_scp_args(&conn_full(), "a", "b");
		assert!(args.contains(&"-P".to_string()));
		assert!(!args.contains(&"-p".to_string()));
	}

	#[test]
	fn scp_args_contain_batchmode() {
		let args = build_scp_args(&conn_minimal(), "a", "b");
		assert!(args.iter().any(|a| a == "BatchMode=yes"));
	}

	// --- build_tunnel_args -------------------------------------------------

	#[test]
	fn tunnel_args_full() {
		let args = build_tunnel_args(&conn_full(), 5000, 8080);
		assert_eq!(
			args,
			vec![
				"-N",
				"-o",
				"BatchMode=yes",
				"-o",
				"ConnectTimeout=10",
				"-o",
				"StrictHostKeyChecking=accept-new",
				"-o",
				"ExitOnForwardFailure=yes",
				"-o",
				"ControlPath=none",
				"-o",
				"ControlMaster=no",
				"-L",
				"127.0.0.1:5000:127.0.0.1:8080",
				"-p",
				"2222",
				"-l",
				"alice",
				"my-vm"
			]
		);
	}

	#[test]
	fn tunnel_args_minimal() {
		let args = build_tunnel_args(&conn_minimal(), 6000, 9090);
		assert_eq!(
			args,
			vec![
				"-N",
				"-o",
				"BatchMode=yes",
				"-o",
				"ConnectTimeout=10",
				"-o",
				"StrictHostKeyChecking=accept-new",
				"-o",
				"ExitOnForwardFailure=yes",
				"-o",
				"ControlPath=none",
				"-o",
				"ControlMaster=no",
				"-L",
				"127.0.0.1:6000:127.0.0.1:9090",
				"example.com"
			]
		);
	}

	#[test]
	fn tunnel_args_loopback_bound_and_exit_on_failure() {
		let args = build_tunnel_args(&conn_minimal(), 1, 2);
		assert!(args.contains(&"127.0.0.1:1:127.0.0.1:2".to_string()));
		assert!(args.iter().any(|a| a == "ExitOnForwardFailure=yes"));
		assert!(args.iter().any(|a| a == "BatchMode=yes"));
	}

	/// Without this the tunnel silently joins a user `ControlMaster auto` +
	/// `ControlPersist` session: `ssh -N -L ...` hands the forward to the
	/// running master and exits 0 at once, and bring-up reads that as
	/// `ssh tunnel exited early (exit status: 0)` — killing a remote server
	/// whose forward was working. Observed against a real Mac -> VM pair.
	#[test]
	fn tunnel_args_never_multiplex_onto_a_user_control_master() {
		let args = build_tunnel_args(&conn_full(), 1, 2);
		assert!(
			args.iter().any(|a| a == "ControlPath=none"),
			"tunnel must own its own connection: {args:?}"
		);
		assert!(
			args.iter().any(|a| a == "ControlMaster=no"),
			"tunnel must not become a master either: {args:?}"
		);
	}

	// --- shell_quote_single ------------------------------------------------

	#[test]
	fn shell_quote_plain() {
		assert_eq!(shell_quote_single("/opt/aghub-api"), "'/opt/aghub-api'");
	}

	#[test]
	fn shell_quote_embedded_single_quote() {
		// `it's` -> 'it'\''s'
		assert_eq!(shell_quote_single("it's"), "'it'\\''s'");
	}

	#[test]
	fn shell_quote_only_a_quote() {
		assert_eq!(shell_quote_single("'"), "''\\'''");
	}

	#[test]
	fn shell_quote_neutralizes_metachars() {
		let q = shell_quote_single("a; rm -rf / #");
		assert_eq!(q, "'a; rm -rf / #'");
	}

	// --- build_remote_start_cmd -------------------------------------------

	#[test]
	fn remote_start_cmd_emits_pid_and_logpath() {
		let cmd = build_remote_start_cmd("/opt/aghub-api", "vm-1");
		assert!(cmd.contains("echo PID=$!"));
		assert!(cmd.contains("LOGPATH="));
		assert!(cmd.contains("--port 0"));
	}

	#[test]
	fn remote_start_cmd_uses_private_dir_and_modes() {
		let cmd = build_remote_start_cmd("/opt/aghub-api", "vm-1");
		assert!(cmd.contains("${XDG_RUNTIME_DIR:-$HOME/.cache/aghub}"));
		assert!(cmd.contains("mkdir -p -m 700"));
		assert!(cmd.contains("chmod 600"));
		assert!(cmd.contains("aghub-api.vm-1.log"));
	}

	#[test]
	fn remote_start_cmd_neutralizes_injection_in_path() {
		let hostile = "/x; rm -rf /";
		let cmd = build_remote_start_cmd(hostile, "vm-1");
		// The hostile path is wrapped in single quotes, inert.
		assert!(cmd.contains("'/x; rm -rf /'"));
	}

	#[test]
	fn remote_start_cmd_resolves_default_api_from_common_paths() {
		let cmd = build_remote_start_cmd("aghub-api", "vm-1");
		assert!(cmd.contains("$HOME/.cargo/bin/aghub-api"));
		assert!(cmd.contains("$HOME/.local/bin/aghub-api"));
		assert!(cmd.contains("/home/linuxbrew/.linuxbrew/bin/aghub-api"));
		assert!(cmd.contains("$HOME/.linuxbrew/bin/aghub-api"));
	}

	#[test]
	fn remote_start_cmd_sanitizes_id_for_log_name() {
		let cmd = build_remote_start_cmd("/bin/x", "../../etc/passwd; rm");
		// Only [A-Za-z0-9_-] survive in the sanitized id.
		assert!(cmd.contains("aghub-api.etcpasswdrm.log"));
		assert!(!cmd.contains("aghub-api.../../"));
	}

	// --- build_remote_cat_cmd ---------------------------------------------

	#[test]
	fn remote_cat_cmd_quotes_path() {
		assert_eq!(
			build_remote_cat_cmd("/run/user/1000/aghub-api.vm-1.log"),
			"cat '/run/user/1000/aghub-api.vm-1.log'"
		);
	}

	#[test]
	fn remote_cat_cmd_neutralizes_injection() {
		let cmd = build_remote_cat_cmd("/x; rm -rf /");
		assert_eq!(cmd, "cat '/x; rm -rf /'");
	}

	// --- build_remote_kill_cmd --------------------------------------------

	#[test]
	fn remote_kill_cmd_is_guarded() {
		let cmd = build_remote_kill_cmd(4242);
		assert_eq!(
            cmd,
            "kill -0 4242 2>/dev/null && ps -o comm= -p 4242 | grep -q aghub-api && kill 4242"
        );
	}

	#[test]
	fn remote_kill_cmd_contains_guard_pieces() {
		let cmd = build_remote_kill_cmd(7);
		assert!(cmd.contains("kill -0 7"));
		assert!(cmd.contains("ps -o comm= -p 7"));
		assert!(cmd.contains("grep -q aghub-api"));
	}

	// --- build_remote_probe_cmd -------------------------------------------

	#[test]
	fn remote_probe_cmd_quotes_path_and_appends_version() {
		assert_eq!(
			build_remote_probe_cmd("/opt/aghub-api"),
			"bin='/opt/aghub-api'; \"$bin\" --version"
		);
	}

	#[test]
	fn remote_probe_cmd_resolves_default_api_from_common_paths() {
		let cmd = build_remote_probe_cmd("aghub-api");
		assert!(cmd.contains("$HOME/.cargo/bin/aghub-api"));
		assert!(cmd.contains("$HOME/.local/bin/aghub-api"));
		assert!(cmd.contains("/home/linuxbrew/.linuxbrew/bin/aghub-api"));
		assert!(cmd.contains("$HOME/.linuxbrew/bin/aghub-api"));
		assert!(cmd.contains("--version"));
	}

	#[test]
	fn remote_probe_cmd_neutralizes_injection() {
		let cmd = build_remote_probe_cmd("a; rm -rf /");
		assert_eq!(cmd, "bin='a; rm -rf /'; \"$bin\" --version");
	}

	#[test]
	fn remote_probe_cmd_expands_explicit_tilde_path() {
		// An explicit `~/…` path must expand `$HOME` (single-quoted remainder),
		// not be passed as a literal `~/…` token the remote shell won't expand.
		assert_eq!(
			build_remote_probe_cmd("~/.local/bin/aghub-api"),
			"bin=\"$HOME\"/'.local/bin/aghub-api'; \"$bin\" --version"
		);
	}

	// --- build_remote_capabilities_cmd / capability parsing ---------------

	#[test]
	fn capabilities_cmd_quotes_path_and_appends_flag() {
		assert_eq!(
			build_remote_capabilities_cmd("/opt/aghub-api"),
			"bin='/opt/aghub-api'; \"$bin\" --capabilities"
		);
	}

	#[test]
	fn capabilities_cmd_resolves_default_api_from_common_paths() {
		let cmd = build_remote_capabilities_cmd("aghub-api");
		assert!(cmd.contains("$HOME/.cargo/bin/aghub-api"));
		assert!(cmd.contains("$HOME/.local/bin/aghub-api"));
		assert!(cmd.contains("/home/linuxbrew/.linuxbrew/bin/aghub-api"));
		assert!(cmd.contains("$HOME/.linuxbrew/bin/aghub-api"));
		assert!(cmd.contains("--capabilities"));
	}

	#[test]
	fn capabilities_cmd_neutralizes_injection() {
		let cmd = build_remote_capabilities_cmd("a; rm -rf /");
		assert_eq!(cmd, "bin='a; rm -rf /'; \"$bin\" --capabilities");
	}

	#[test]
	fn capability_line_prefix_and_token_are_locked_literals() {
		// Cross-crate contract mirrored from aghub_api::cli — must stay byte
		// identical to what the binary emits.
		assert_eq!(CAPABILITY_LINE_PREFIX, "AGHUB_API_CAPABILITIES=");
		assert_eq!(CAP_GIT_CREDENTIAL_FORWARDING, "git-credential-forwarding");
	}

	#[test]
	fn capability_line_advertises_matches_present_token() {
		assert!(capability_line_advertises(
			"AGHUB_API_CAPABILITIES=git-credential-forwarding",
			CAP_GIT_CREDENTIAL_FORWARDING
		));
		// Multiple tokens, target among them.
		assert!(capability_line_advertises(
			"AGHUB_API_CAPABILITIES=foo git-credential-forwarding bar",
			CAP_GIT_CREDENTIAL_FORWARDING
		));
		// Multiline banner before the capability line.
		assert!(capability_line_advertises(
			"some banner\nAGHUB_API_CAPABILITIES=git-credential-forwarding",
			CAP_GIT_CREDENTIAL_FORWARDING
		));
	}

	#[test]
	fn capability_line_advertises_rejects_absent_or_malformed() {
		// No capability line at all (old binary's unknown-flag stderr leaks
		// nothing useful to stdout).
		assert!(!capability_line_advertises(
			"error: unrecognized argument --capabilities",
			CAP_GIT_CREDENTIAL_FORWARDING
		));
		// Empty token list.
		assert!(!capability_line_advertises(
			"AGHUB_API_CAPABILITIES=",
			CAP_GIT_CREDENTIAL_FORWARDING
		));
		// A different token only.
		assert!(!capability_line_advertises(
			"AGHUB_API_CAPABILITIES=some-other-feature",
			CAP_GIT_CREDENTIAL_FORWARDING
		));
		// A prefix-suffix collision must NOT match (left word boundary).
		assert!(!capability_line_advertises(
			"X_AGHUB_API_CAPABILITIES=git-credential-forwarding",
			CAP_GIT_CREDENTIAL_FORWARDING
		));
		// A substring of the token is not the token.
		assert!(!capability_line_advertises(
			"AGHUB_API_CAPABILITIES=git-credential",
			CAP_GIT_CREDENTIAL_FORWARDING
		));
	}

	fn cap_conn() -> Connection {
		Connection {
			id: "c".into(),
			label: "c".into(),
			ssh_target: "host".into(),
			user: None,
			port: None,
			remote_aghub_path: None,
		}
	}

	#[test]
	fn probe_credential_forwarding_true_when_marker_present() {
		let conn = cap_conn();
		let cmd = build_remote_capabilities_cmd("aghub-api");
		let args_owned = build_ssh_args(&conn, &cmd);
		let args: Vec<&str> = args_owned.iter().map(String::as_str).collect();
		let runner = MockRunner::new().script(
			"ssh",
			&args,
			CommandOutput {
				status_code: Some(0),
				stdout: "AGHUB_API_CAPABILITIES=git-credential-forwarding\n"
					.into(),
				stderr: String::new(),
			},
		);
		assert!(probe_supports_credential_forwarding(
			&runner,
			&conn,
			"aghub-api"
		));
	}

	#[test]
	fn probe_credential_forwarding_false_for_old_binary_unknown_flag() {
		// An old aghub-api lacks `--capabilities`: the hand-rolled parser
		// returns an UnknownFlag error and the binary exits non-zero, printing
		// the error to stderr. No capability line on stdout -> false.
		let conn = cap_conn();
		let cmd = build_remote_capabilities_cmd("aghub-api");
		let args_owned = build_ssh_args(&conn, &cmd);
		let args: Vec<&str> = args_owned.iter().map(String::as_str).collect();
		let runner = MockRunner::new().script(
			"ssh",
			&args,
			CommandOutput {
				status_code: Some(1),
				stdout: String::new(),
				stderr: "unknown flag: --capabilities".into(),
			},
		);
		assert!(!probe_supports_credential_forwarding(
			&runner,
			&conn,
			"aghub-api"
		));
	}

	#[test]
	fn probe_credential_forwarding_false_on_zero_exit_but_no_marker() {
		// Defensive: a clean exit whose stdout lacks the marker (e.g. a future
		// binary that prints an unrelated line) must still be treated as
		// unsupported.
		let conn = cap_conn();
		let cmd = build_remote_capabilities_cmd("aghub-api");
		let args_owned = build_ssh_args(&conn, &cmd);
		let args: Vec<&str> = args_owned.iter().map(String::as_str).collect();
		let runner = MockRunner::new().script(
			"ssh",
			&args,
			CommandOutput {
				status_code: Some(0),
				stdout: "aghub-api 1.0.0\n".into(),
				stderr: String::new(),
			},
		);
		assert!(!probe_supports_credential_forwarding(
			&runner,
			&conn,
			"aghub-api"
		));
	}

	#[test]
	fn probe_credential_forwarding_false_on_transport_failure() {
		let conn = cap_conn();
		let cmd = build_remote_capabilities_cmd("aghub-api");
		let args_owned = build_ssh_args(&conn, &cmd);
		let args: Vec<&str> = args_owned.iter().map(String::as_str).collect();
		let runner = MockRunner::new().script(
			"ssh",
			&args,
			CommandOutput {
				status_code: Some(255),
				stdout: String::new(),
				stderr: "ssh: connect to host failed".into(),
			},
		);
		assert!(!probe_supports_credential_forwarding(
			&runner,
			&conn,
			"aghub-api"
		));
	}

	#[test]
	fn probe_credential_forwarding_false_when_runner_errors() {
		// An unscripted MockRunner call returns Err (runner-level failure);
		// the probe must fail safe.
		let conn = cap_conn();
		let runner = MockRunner::new();
		assert!(!probe_supports_credential_forwarding(
			&runner,
			&conn,
			"aghub-api"
		));
	}

	#[test]
	fn remote_start_cmd_expands_explicit_tilde_path() {
		let cmd = build_remote_start_cmd("~/.local/bin/aghub-api", "vm-1");
		assert!(
			cmd.contains("bin=\"$HOME\"/'.local/bin/aghub-api';"),
			"start must expand the tilde to $HOME: {cmd}"
		);
	}

	#[test]
	fn probe_and_install_target_agree_on_tilde_expansion() {
		// Regression: the probe/start path and the install target must expand
		// `~/` the SAME way, or an explicit `~/.local/bin/aghub-api` is
		// installed to `$HOME/.local/bin/aghub-api` but probed as a literal
		// `~/…` and never found — a successful install that can't connect.
		let probe = build_remote_probe_cmd("~/.local/bin/aghub-api");
		let finish =
			build_remote_finish_upload_cmd("~/.local/bin/aghub-api", "n1");
		assert!(
			probe.contains("bin=\"$HOME\"/'.local/bin/aghub-api';"),
			"probe must expand the tilde to $HOME: {probe}"
		);
		assert!(
			finish.contains("target=\"$HOME\"/'.local/bin/aghub-api';"),
			"finish must expand the tilde to $HOME: {finish}"
		);
	}

	// --- build_remote_cargo_install_cmd -----------------------------------

	#[test]
	fn remote_cargo_install_cmd_uses_crate_spec_not_package_flag() {
		let cmd = build_remote_cargo_install_cmd(
			"https://example.com/aghub.git",
			Some("feat/remote-ssh-management"),
			None,
			"2.7.2",
		);
		assert!(cmd.contains(
			"AGHUB_RELEASE_VERSION='2.7.2' \"$cargo_bin\" install \
		     --git 'https://example.com/aghub.git' \
		     --branch 'feat/remote-ssh-management' aghub-api \
		     --bin aghub-api --force"
		));
		assert!(cmd.contains("$HOME/.cargo/bin/cargo"));
		assert!(!cmd.contains("--package"));
	}

	#[test]
	fn remote_cargo_install_cmd_can_pin_tag() {
		// An explicit tag names a SPECIFIC release, which may differ from the
		// desktop's own local_version — the stamp must be the TAG's version
		// (leading `v` stripped), not the desktop's, or the remote would
		// falsely advertise 2.7.2 when it actually built 2.3.1.
		let cmd = build_remote_cargo_install_cmd(
			"https://example.com/aghub.git",
			None,
			Some("v2.3.1"),
			"2.7.2",
		);
		assert!(cmd.contains(
			"AGHUB_RELEASE_VERSION='2.3.1' \"$cargo_bin\" install \
		     --git 'https://example.com/aghub.git' \
		     --tag 'v2.3.1' aghub-api --bin aghub-api --force"
		));
		assert!(!cmd.contains("--branch"));
		assert!(
			!cmd.contains("AGHUB_RELEASE_VERSION='2.7.2'"),
			"must not stamp the desktop's local_version over an explicit \
			 tag's own version: {cmd}"
		);
	}

	#[test]
	fn remote_cargo_install_cmd_stamps_tag_without_v_prefix_verbatim() {
		// A tag lacking the conventional `v` prefix has nothing to strip and
		// is stamped verbatim.
		let cmd = build_remote_cargo_install_cmd(
			"https://example.com/aghub.git",
			None,
			Some("2.3.1"),
			"2.7.2",
		);
		assert!(cmd.contains("AGHUB_RELEASE_VERSION='2.3.1'"));
	}

	#[test]
	fn remote_cargo_install_cmd_stamps_release_version() {
		// A cargo-git checkout has no tag refs, so the remote build.rs's
		// `git describe` fallback fails there and it would otherwise stamp the
		// workspace placeholder (1.1.1) — an eternal reinstall loop. The
		// desktop's own version must be visible to the build script's
		// environment.
		let cmd = build_remote_cargo_install_cmd(
			"https://example.com/aghub.git",
			Some("main"),
			None,
			"2.7.2-3-gabc1234",
		);
		assert!(cmd.contains(
			"AGHUB_RELEASE_VERSION='2.7.2-3-gabc1234' \"$cargo_bin\" install"
		));
	}

	#[test]
	fn remote_cargo_install_cmd_both_branch_and_tag_stamps_release_version() {
		// Ref-selection prefers `branch` over `tag` when both are given (see
		// `ref_arg`): the install actually checks out the branch. The stamp
		// must mirror that SAME precedence — stamping the tag's version here
		// would falsely advertise a version the VM never actually built.
		let cmd = build_remote_cargo_install_cmd(
			"https://example.com/aghub.git",
			Some("main"),
			Some("v9.9.9"),
			"2.7.3",
		);
		assert!(cmd.contains("--branch 'main'"), "got: {cmd}");
		assert!(!cmd.contains("--tag"), "branch must win the ref: {cmd}");
		assert!(
			cmd.contains("AGHUB_RELEASE_VERSION='2.7.3'"),
			"must stamp release_version when branch wins the ref: {cmd}"
		);
		assert!(
			!cmd.contains("AGHUB_RELEASE_VERSION='9.9.9'"),
			"must NOT stamp the unused tag's version: {cmd}"
		);
	}

	#[test]
	fn remote_release_deb_install_cmd_extracts_packaged_api() {
		let cmd = build_remote_release_deb_install_cmd(
			"https://github.com/audichuang/aghub/releases/download/v2.3.2/aghub_2.3.2_amd64.deb",
			"aghub-api",
		);
		assert!(cmd.contains("dpkg-deb -x \"$deb\" \"$root\""));
		assert!(cmd.contains("curl -fsSL --connect-timeout 10 --max-time 120"));
		assert!(cmd.contains("wget -q -T 120 -O \"$deb\""));
		assert!(cmd.contains("resources/binaries/aghub-api"));
		assert!(cmd
			.contains("stage=\"$(mktemp \"$target_dir/.aghub-api.XXXXXX\")\""));
		assert!(cmd.contains("cp \"$bin\" \"$stage\""));
		assert!(cmd.contains("\"$stage\" --version"));
		assert!(cmd.contains("mv \"$stage\" \"$target\""));
		// The self-check runs on the STAGED copy, never on the live target.
		assert!(!cmd.contains("\"$target\" --version"));
		// A failed self-check/mv must clean up the stray staged file, guarded
		// on a non-empty $stage (Fix A hygiene).
		assert!(cmd.contains("[ -n \"$stage\" ] && rm -f -- \"$stage\""));
		assert!(cmd.contains("$HOME/.local/bin/aghub-api"));
	}

	#[test]
	fn remote_release_deb_install_cmd_initializes_stage_before_any_use() {
		// Regression (Fix A): `stage=""` must be bound at the VERY start of
		// the script — before the dpkg-deb check, before the mkdir the
		// staged-file mktemp depends on — so a failure early in the chain
		// (e.g. `mkdir -p "$target_dir"` itself failing) still reaches the
		// `|| { ...; rm -f -- "$stage"; ... }` cleanup with `stage` bound to
		// an empty string, never a stray value inherited from the `bash -lc`
		// login shell's environment (e.g. a profile script's own `stage=`).
		let cmd = build_remote_release_deb_install_cmd(
			"https://example.com/aghub.deb",
			"aghub-api",
		);
		let stage_init_pos = cmd
			.find("stage=\"\";")
			.expect("stage must be initialized to empty up front");
		let dpkg_check_pos = cmd
			.find("command -v dpkg-deb")
			.expect("dpkg-deb check must be present");
		let mkdir_pos = cmd
			.find("mkdir -p \"$target_dir\"")
			.expect("target_dir mkdir must be present");
		assert!(
			stage_init_pos < dpkg_check_pos && stage_init_pos < mkdir_pos,
			"stage=\"\" must precede every other use: {cmd}"
		);
		// The failure cleanup must guard on non-empty AND use `--`.
		assert!(
			cmd.contains("[ -n \"$stage\" ] && rm -f -- \"$stage\""),
			"cleanup must guard on a non-empty $stage and use `--`: {cmd}"
		);
	}

	#[test]
	fn remote_release_deb_install_cmd_validates_staged_binary_before_mv() {
		// Order pin: the self-check on the STAGED path must run BEFORE the mv
		// that replaces `$target`, so a corrupt/incompatible download can
		// never destroy the previously working binary.
		let cmd = build_remote_release_deb_install_cmd(
			"https://example.com/aghub.deb",
			"aghub-api",
		);
		let check_pos = cmd
			.find("\"$stage\" --version")
			.expect("staged self-check must be present");
		let mv_pos = cmd
			.find("mv \"$stage\" \"$target\"")
			.expect("mv into target must be present");
		assert!(
			check_pos < mv_pos,
			"staged self-check must precede the mv into $target: {cmd}"
		);
	}

	#[test]
	fn remote_release_deb_install_cmd_expands_custom_tilde_path() {
		let cmd = build_remote_release_deb_install_cmd(
			"https://example.com/aghub.deb",
			"~/.local/bin/aghub-api",
		);
		assert!(
			cmd.contains("target=\"$HOME\"/'.local/bin/aghub-api';"),
			"release-deb install must use the same tilde expansion as probe: {cmd}"
		);
	}

	// --- parsers -----------------------------------------------------------

	#[test]
	fn parse_remote_port_happy() {
		assert_eq!(parse_remote_port("AGHUB_API_PORT=54321"), Some(54321));
	}

	#[test]
	fn parse_remote_port_multiline() {
		let s = "some noise\nAGHUB_API_PORT=8080\nmore";
		assert_eq!(parse_remote_port(s), Some(8080));
	}

	#[test]
	fn parse_remote_port_none() {
		assert_eq!(parse_remote_port("nothing here"), None);
		assert_eq!(parse_remote_port("AGHUB_API_PORT="), None);
		assert_eq!(parse_remote_port("AGHUB_API_PORT=notanumber"), None);
	}

	#[test]
	fn parse_pid_happy() {
		assert_eq!(parse_pid("PID=12345"), Some(12345));
	}

	#[test]
	fn parse_pid_multiline() {
		assert_eq!(parse_pid("PID=99\nLOGPATH=/x"), Some(99));
	}

	#[test]
	fn parse_pid_none() {
		assert_eq!(parse_pid("no pid"), None);
		assert_eq!(parse_pid("PID="), None);
	}

	#[test]
	fn parse_kv_is_left_anchored_not_substring() {
		// A longer token ending in the key must NOT match; the real line wins.
		assert_eq!(parse_pid("OLDPID=999\nPID=4242"), Some(4242));
		assert_eq!(parse_pid("OLDPID=999"), None);
		assert_eq!(
			parse_remote_port("MY_AGHUB_API_PORT=1\nAGHUB_API_PORT=8123"),
			Some(8123)
		);
		// Key after whitespace is still a valid match.
		assert_eq!(parse_pid("  PID=7"), Some(7));
		assert_eq!(parse_pid("note: PID=7"), Some(7));
	}

	#[test]
	fn parse_logpath_is_left_anchored() {
		assert_eq!(
			parse_logpath("X_LOGPATH=/nope\nLOGPATH=/run/u/a.log"),
			Some("/run/u/a.log".to_string())
		);
	}

	#[test]
	fn parse_logpath_happy() {
		assert_eq!(
			parse_logpath("LOGPATH=/run/user/1000/aghub-api.vm-1.log"),
			Some("/run/user/1000/aghub-api.vm-1.log".to_string())
		);
	}

	#[test]
	fn parse_logpath_to_end_of_line_with_spaces() {
		assert_eq!(
			parse_logpath("PID=1\nLOGPATH=/path with spaces/x.log\ntrailing"),
			Some("/path with spaces/x.log".to_string())
		);
	}

	#[test]
	fn parse_logpath_none() {
		assert_eq!(parse_logpath("nope"), None);
	}

	#[test]
	fn parse_api_version_happy() {
		assert_eq!(
			parse_api_version("aghub-api 1.1.1"),
			Some("1.1.1".to_string())
		);
	}

	#[test]
	fn parse_api_version_multiline() {
		assert_eq!(
			parse_api_version("banner\naghub-api 2.3.4\nfoo"),
			Some("2.3.4".to_string())
		);
	}

	#[test]
	fn parse_api_version_none() {
		assert_eq!(parse_api_version("some other tool 1.0.0"), None);
		assert_eq!(parse_api_version("aghub-api "), None);
	}

	#[test]
	fn version_compatible_same_minor() {
		assert!(is_version_compatible("1.1.1", "1.1.9"));
		assert!(is_version_compatible("1.1.0", "1.1.0"));
	}

	#[test]
	fn version_incompatible_cross_minor_or_major() {
		assert!(!is_version_compatible("1.1.1", "1.2.0"));
		assert!(!is_version_compatible("1.1.1", "2.1.1"));
		assert!(!is_version_compatible("1.1.1", "garbage"));
	}

	#[test]
	fn version_compatible_with_git_describe_suffix() {
		// A dev desktop build now self-reports `aghub_api::VERSION` as a
		// git-describe string (e.g. `2.7.2-3-gabc1234` or with a `-dev`
		// dirty-tree suffix), not the bare `CARGO_PKG_VERSION`. `major_minor`
		// only parses the first two dot-separated components, so the
		// trailing `-N-g<sha>`/`-dev` noise on the THIRD component never
		// reaches it — compatibility still turns on major.minor alone.
		assert!(is_version_compatible("2.7.2-3-gabc1234", "2.7.2"));
		assert!(is_version_compatible("2.7.2-3-gabc1234-dev", "2.7.9"));
		assert!(!is_version_compatible("2.7.2-3-gabc1234", "2.8.0"));
	}

	#[test]
	fn normalize_platform_maps_uname_to_consts_vocab() {
		// `uname -sm` vocabulary → std::env::consts vocabulary.
		assert_eq!(
			normalize_platform("Darwin", "arm64"),
			Some(("macos".to_string(), "aarch64".to_string()))
		);
		assert_eq!(
			normalize_platform("Linux", "x86_64"),
			Some(("linux".to_string(), "x86_64".to_string()))
		);
	}

	#[test]
	fn normalize_platform_rejects_unknown() {
		assert_eq!(normalize_platform("Windows_NT", "x86_64"), None);
		assert_eq!(normalize_platform("Linux", "riscv64"), None);
		assert_eq!(normalize_platform("", ""), None);
	}

	fn plat_conn() -> Connection {
		Connection {
			id: "c".into(),
			label: "c".into(),
			ssh_target: "host".into(),
			user: None,
			port: None,
			remote_aghub_path: None,
		}
	}

	#[test]
	fn probe_remote_platform_parses_and_normalizes() {
		let conn = plat_conn();
		let args_owned = build_ssh_args(&conn, "uname -sm");
		let args: Vec<&str> = args_owned.iter().map(String::as_str).collect();
		let runner = MockRunner::new().script(
			"ssh",
			&args,
			CommandOutput {
				status_code: Some(0),
				stdout: "Darwin arm64\n".into(),
				stderr: String::new(),
			},
		);
		assert_eq!(
			probe_remote_platform(&runner, &conn),
			Some(("macos".to_string(), "aarch64".to_string()))
		);
	}

	#[test]
	fn probe_remote_platform_none_on_transport_failure() {
		let conn = plat_conn();
		let args_owned = build_ssh_args(&conn, "uname -sm");
		let args: Vec<&str> = args_owned.iter().map(String::as_str).collect();
		let runner = MockRunner::new().script(
			"ssh",
			&args,
			CommandOutput {
				status_code: Some(255),
				stdout: String::new(),
				stderr: "ssh: connect refused".into(),
			},
		);
		assert_eq!(probe_remote_platform(&runner, &conn), None);
	}

	#[test]
	fn finish_upload_cmd_uses_atomic_mv_not_install() {
		let cmd = build_remote_finish_upload_cmd("aghub-api", "n1");
		assert!(
			cmd.contains("mv -f -- \"$tmp\" \"$target\""),
			"expected an atomic same-dir mv, got: {cmd}"
		);
		assert!(
			!cmd.contains("install -m 755"),
			"install -m 755 risks ETXTBSY on a running binary"
		);
	}

	#[test]
	fn finish_upload_cmd_stages_into_target_dir_before_atomic_rename() {
		// Fix B: the cache-staged upload may be on a DIFFERENT filesystem than
		// $target's directory, so the swap must go through a SAME-DIRECTORY
		// tmp file ("$target.tmp.<nonce>") rather than mv'ing the cache path
		// straight into $target (a cross-fs mv silently falls back to
		// copy+remove, which can destroy $target on a failure partway
		// through).
		let cmd = build_remote_finish_upload_cmd("aghub-api", "n1");
		assert!(
			cmd.contains("\"$target.tmp.n1\""),
			"the intermediate file must be a sibling of $target: {cmd}"
		);
		let validate_pos = cmd
			.find("\"$HOME/.cache/aghub/aghub-api.upload.n1\" --version")
			.expect("staged self-check must run");
		let cp_pos = cmd
			.find("cp -- \"$HOME/.cache/aghub/aghub-api.upload.n1\" \"$tmp\"")
			.expect("cp into the target-dir tmp file must run");
		let mv_pos = cmd
			.find("mv -f -- \"$tmp\" \"$target\"")
			.expect("atomic same-dir mv must run");
		assert!(
			validate_pos < cp_pos && cp_pos < mv_pos,
			"order must be validate -> cp -> mv: {cmd}"
		);
		// No cross-filesystem mv of the cache path straight into $target may
		// remain.
		assert!(
			!cmd.contains("mv \"$HOME/.cache/aghub/aghub-api.upload"),
			"the cache path must never be mv'd directly into $target: {cmd}"
		);
	}

	#[test]
	fn finish_upload_cmd_validates_staged_binary_before_mv() {
		// Order pin (the BLOCKING fix): the staged-path self-check must run
		// BEFORE the mv that replaces `$target`, so a bad upload (corrupt
		// download, glibc/ABI mismatch) never destroys the previously working
		// binary — the check fails, the script exits non-zero, and $target is
		// untouched.
		let cmd = build_remote_finish_upload_cmd("aghub-api", "n1");
		let staged = "$HOME/.cache/aghub/aghub-api.upload.n1";
		let check_pos = cmd
			.find(&format!("\"{staged}\" --version"))
			.expect("staged self-check must be present");
		let mv_pos = cmd
			.find("mv -f -- \"$tmp\" \"$target\"")
			.expect("mv into target must be present");
		assert!(
			check_pos < mv_pos,
			"staged self-check must precede the mv into $target: {cmd}"
		);
		// The self-check must run on the STAGED path, never on the live
		// target — a check retargeted back onto $target would defeat the
		// whole fix.
		assert!(!cmd.contains("\"$target\" --version"), "got: {cmd}");
	}

	#[test]
	fn finish_upload_cmd_failure_cleans_tmp_and_staged_with_guard() {
		// Fix A hygiene applied to the NEW `$tmp` variable Fix B introduces:
		// `tmp` is bound empty up front, and the failure cleanup only removes
		// it when non-empty — the same guard as `$stage` in
		// build_remote_release_deb_install_cmd, and for the same reason (an
		// early failure before `tmp` is ever assigned must not fall back to a
		// login-shell-inherited value of that name).
		let cmd = build_remote_finish_upload_cmd("aghub-api", "n1");
		let tmp_init_pos = cmd
			.find("tmp=\"\";")
			.expect("tmp must be initialized to empty up front");
		let validate_pos = cmd
			.find("\"$HOME/.cache/aghub/aghub-api.upload.n1\" --version")
			.expect("staged self-check must run");
		assert!(
			tmp_init_pos < validate_pos,
			"tmp=\"\" must precede every other use: {cmd}"
		);
		assert!(
			cmd.contains("[ -n \"$tmp\" ] && rm -f -- \"$tmp\""),
			"failure cleanup must guard tmp on non-empty: {cmd}"
		);
		assert!(
			cmd.contains("rm -f -- \"$HOME/.cache/aghub/aghub-api.upload.n1\""),
			"failure cleanup must also remove the staged upload: {cmd}"
		);
	}

	#[test]
	fn finish_upload_cmd_failure_branch_never_references_target() {
		// The failure branch (after `||`) must never touch $target — only the
		// success chain's final `mv` ever writes to it, so any failure before
		// that point leaves $target completely untouched.
		let cmd = build_remote_finish_upload_cmd("aghub-api", "n1");
		let (_, failure_branch) =
			cmd.split_once("|| {").expect("a failure branch must exist");
		assert!(
			!failure_branch.contains("\"$target\""),
			"failure branch must never touch $target: {failure_branch}"
		);
	}

	#[test]
	fn finish_upload_default_overwrites_resolved_existing_path() {
		// The default install target must follow the SAME resolution as the
		// probe (command -v -> ~/.cargo/bin -> ~/.local/bin) so a
		// cargo-installed binary is upgraded IN PLACE, not shadowed by a new
		// ~/.local/bin copy. Falls back to ~/.local/bin only for a fresh
		// install.
		let cmd = build_remote_finish_upload_cmd("aghub-api", "n1");
		assert!(
			cmd.contains("command -v aghub-api"),
			"target must follow the resolved path: {cmd}"
		);
		assert!(
			cmd.contains("$HOME/.cargo/bin/aghub-api"),
			"must consider cargo-bin: {cmd}"
		);
		assert!(
			cmd.contains("$HOME/.local/bin/aghub-api"),
			"must fall back to local-bin: {cmd}"
		);
		assert!(
			cmd.contains("mv -f -- \"$tmp\" \"$target\""),
			"still moves the staged copy into $target: {cmd}"
		);
		assert!(
			cmd.contains(
				"chmod 755 \"$HOME/.cache/aghub/aghub-api.upload.n1\""
			),
			"still chmods the staged upload: {cmd}"
		);
	}

	#[test]
	fn finish_upload_explicit_paths_used_verbatim() {
		// An explicit tilde path expands $HOME but is otherwise verbatim.
		let tilde = build_remote_finish_upload_cmd("~/bin/aghub-api", "n1");
		assert!(
			tilde.contains("target=\"$HOME\"/'bin/aghub-api';"),
			"tilde path must expand $HOME verbatim: {tilde}"
		);
		// An absolute / other path is single-quoted verbatim.
		let abs = build_remote_finish_upload_cmd("/abs/aghub-api", "n1");
		assert!(
			abs.contains("target='/abs/aghub-api';"),
			"absolute path must be verbatim: {abs}"
		);
	}

	// --- MockRunner --------------------------------------------------------

	#[test]
	fn mock_runner_returns_scripted_output_and_records_call() {
		let runner = MockRunner::new().script(
			"ssh",
			&["-o", "BatchMode=yes", "host", "cmd"],
			CommandOutput {
				status_code: Some(0),
				stdout: "AGHUB_API_PORT=4321".to_string(),
				stderr: String::new(),
			},
		);
		let args: Vec<String> = ["-o", "BatchMode=yes", "host", "cmd"]
			.iter()
			.map(|s| s.to_string())
			.collect();
		let out = runner.run("ssh", &args).unwrap();
		assert_eq!(out.stdout, "AGHUB_API_PORT=4321");
		assert_eq!(parse_remote_port(&out.stdout), Some(4321));

		let calls = runner.calls();
		assert_eq!(calls.len(), 1);
		assert_eq!(calls[0].program, "ssh");
		assert_eq!(calls[0].args, args);
	}

	#[test]
	fn mock_runner_errors_on_unscripted_call() {
		let runner = MockRunner::new();
		let res = runner.run("ssh", &["x".to_string()]);
		assert!(res.is_err());
		// Call is still recorded even when unscripted.
		assert_eq!(runner.calls().len(), 1);
	}

	// --- RunError is a real std::error::Error ------------------------------

	#[test]
	fn run_error_is_error_trait_object() {
		let e: Box<dyn std::error::Error> =
			Box::new(RunError::Spawn("boom".to_string()));
		assert!(e.to_string().contains("boom"));
	}

	// --- ChildHandle kill ---------------------------------------------------

	#[cfg(unix)]
	#[test]
	fn child_handle_kill_terminates_live_child() {
		use std::thread::sleep;
		use std::time::Duration;

		let child = std::process::Command::new("sleep")
			.arg("30")
			.spawn()
			.expect("spawn sleep");
		let handle = ChildHandle::new(child);

		// A freshly spawned long-running child is still alive.
		assert_eq!(
			handle.try_wait().expect("try_wait before kill"),
			None,
			"child should still be running before kill"
		);

		handle.kill().expect("kill");

		// Poll until the child is reaped (kill is async; the kernel needs a
		// moment, and Child::try_wait must reap the zombie).
		let mut waited = Duration::ZERO;
		let step = Duration::from_millis(25);
		loop {
			if handle.try_wait().expect("try_wait after kill").is_some() {
				break;
			}
			assert!(
				waited < Duration::from_secs(5),
				"child did not exit within 5s after kill"
			);
			sleep(step);
			waited += step;
		}
	}
}
