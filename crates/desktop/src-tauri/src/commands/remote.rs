//! Tauri command layer for remote SSH management.
//!
//! All the testable transport + bring-up logic lives in the tauri-free
//! `aghub-remote` crate (unit-tested with a `MockRunner`). This module is the
//! thin glue: it deserializes the `Connection` payload from the frontend,
//! drives the bring-up via a real [`SystemRunner`], owns the local tunnel
//! child + a
//! watcher thread that reports unexpected disconnects, and tracks live handles
//! so they can be torn down on disconnect and on app exit.
//!
//! Commands are **synchronous** `#[tauri::command] fn` on purpose: Tauri runs
//! sync commands on its worker thread-pool, so the blocking `std::process` ssh
//! work never stalls the (rt-only, single-thread) tokio runtime. The frontend
//! still receives a Promise from `invoke`.

use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aghub_remote::bringup::{
	ensure_remote_api, force_redeploy_remote_api, install_hint,
	probe_connection, reinstall_remote_api as reinstall_remote_api_core,
	start_remote, ConnectError, RemoteInstallSource, StartedServer, TestResult,
};
use aghub_remote::fs::{
	list_remote_directories as list_remote_directories_core,
	RemoteDirectoryError, RemoteDirectoryListing,
};
use aghub_remote::ssh::{
	build_tunnel_args, probe_remote_platform, ChildHandle, CommandRunner,
	Connection, SystemRunner,
};
use aghub_remote::ssh_config::{read_default_ssh_config_hosts, SshConfigHost};
use log::{info, warn};
use serde::Serialize;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::server::find_available_port;

/// Local embedded API version, used to gate remote `aghub-api` compatibility.
const LOCAL_VERSION: &str = aghub_api::VERSION;
/// How many times to poll the remote log for the bound port.
const PORT_POLL_ATTEMPTS: u32 = 40;
/// Delay between port-poll attempts (~40 * 250ms = 10s budget).
const PORT_POLL_DELAY: Duration = Duration::from_millis(250);
/// Grace period after spawning the tunnel before we check it stayed up.
const TUNNEL_SETTLE: Duration = Duration::from_millis(400);
/// Public repository used as the last-resort cargo fallback for remote installs.
const DEFAULT_REMOTE_INSTALL_GIT_URL: &str =
	"https://github.com/audichuang/aghub.git";
/// Release asset root used for cross-platform Linux VM installs. The `.deb`
/// contains the CI-versioned Linux `aghub-api` Tauri resource.
const DEFAULT_REMOTE_RELEASE_BASE_URL: &str =
	"https://github.com/audichuang/aghub/releases/download";

/// A live remote connection: the local tunnel process, the remote server pid,
/// and the connection it belongs to.
struct RemoteHandle {
	/// Handle to the local `ssh -L` tunnel child. Cloned into the watcher
	/// thread; teardown calls `tunnel.kill()` (cross-platform, and holding the
	/// live child closes the pid-reuse window).
	tunnel: ChildHandle,
	/// PID of the `aghub-api` process on the VM (guarded-killed on teardown).
	remote_pid: u32,
	/// The local port the tunnel listens on (== the frontend baseUrl port).
	local_port: u16,
	/// The connection definition (needed to re-issue ssh for remote cleanup).
	connection: Connection,
	/// Set before an intentional teardown so the watcher suppresses the
	/// `remote-disconnected` event.
	intentional: Arc<AtomicBool>,
}

/// Managed Tauri state holding every live remote connection.
#[derive(Default)]
pub struct RemoteState {
	handles: Mutex<HashMap<String, RemoteHandle>>,
	/// Connection ids whose bring-up is currently in flight (dedup guard).
	connecting: Mutex<HashSet<String>>,
}

/// RAII claim on a connection's in-flight `connecting` slot.
///
/// [`SlotGuard::claim`] inserts the id (erroring with
/// [`RemoteError::AlreadyConnecting`] if another bring-up already holds it)
/// and [`Drop`] removes it — so the slot is released on every exit path,
/// including early `?` returns and a panic in the slow ssh work. Keep the
/// guard alive until AFTER the handle is stored in `state.handles`, so no
/// concurrent caller can observe "free slot, no handle".
struct SlotGuard<'a> {
	set: &'a Mutex<HashSet<String>>,
	id: String,
}

impl<'a> SlotGuard<'a> {
	/// Claim `id` in `set`, or return [`RemoteError::AlreadyConnecting`] if it
	/// is already claimed.
	fn claim(
		set: &'a Mutex<HashSet<String>>,
		id: String,
	) -> Result<Self, RemoteError> {
		if !lock(set)?.insert(id.clone()) {
			return Err(RemoteError::AlreadyConnecting);
		}
		Ok(Self { set, id })
	}
}

impl Drop for SlotGuard<'_> {
	fn drop(&mut self) {
		// Poison-recovering remove: a panic elsewhere must not leak the slot.
		lock_recover(self.set).remove(&self.id);
	}
}

/// Structured, serializable error returned to the frontend so the UI can show
/// an actionable message (e.g. an install hint).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RemoteError {
	/// SSH transport failed (auth / connectivity / unknown host key).
	Unreachable { stderr: String },
	/// No compatible `aghub-api` on the VM and we can't auto-deploy.
	RemoteApiMissing { install_hint: String },
	/// A remote `aghub-api` was found but its version is incompatible.
	Incompatible { remote_version: Option<String> },
	/// Force-redeploy was requested but the remote's `(os, arch)` differs from
	/// the desktop's, so the bundled/local binary would not run there. Carries
	/// the probed platform (or `"unknown"`) and the manual install hint.
	CrossPlatformRedeploy {
		remote_platform: String,
		hint: String,
	},
	/// The remote server never reported its port within the poll budget.
	StartTimeout,
	/// The local ssh tunnel failed to establish the port-forward.
	TunnelFailed { message: String },
	/// Automatic install/deploy of `aghub-api` failed.
	DeployFailed { message: String },
	/// A bring-up for this connection is already in progress.
	AlreadyConnecting,
	/// Remote directory browsing failed.
	RemoteDirectoryFailed { message: String },
	/// Anything else (spawn failures, poisoned locks, ...).
	Internal { message: String },
}

impl From<ConnectError> for RemoteError {
	fn from(e: ConnectError) -> Self {
		match e {
			ConnectError::RemoteApiMissing { install_hint } => {
				RemoteError::RemoteApiMissing { install_hint }
			}
			ConnectError::Unreachable { stderr } => {
				RemoteError::Unreachable { stderr }
			}
			ConnectError::CrossPlatformDeploy { remote_platform } => {
				// Converge the connect path on the same persistent
				// "install manually" banner the redeploy path already shows.
				RemoteError::CrossPlatformRedeploy {
					remote_platform,
					hint: install_hint(),
				}
			}
			ConnectError::StartTimeout => RemoteError::StartTimeout,
			ConnectError::TunnelFailed(message) => {
				RemoteError::TunnelFailed { message }
			}
			ConnectError::DeployFailed(message) => {
				RemoteError::DeployFailed { message }
			}
		}
	}
}

impl From<RemoteDirectoryError> for RemoteError {
	fn from(e: RemoteDirectoryError) -> Self {
		match e {
			RemoteDirectoryError::Unreachable { stderr } => {
				RemoteError::Unreachable { stderr }
			}
			RemoteDirectoryError::NotDirectory { message }
			| RemoteDirectoryError::CommandFailed { message }
			| RemoteDirectoryError::Parse { message } => {
				RemoteError::RemoteDirectoryFailed { message }
			}
		}
	}
}

/// Payload for the `remote-disconnected` event emitted when a tunnel dies
/// unexpectedly.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDisconnected {
	connection_id: String,
}

/// The resolved remote binary path (user override or `aghub-api` on PATH).
fn resolved_path(conn: &Connection) -> String {
	conn.remote_aghub_path
		.clone()
		.unwrap_or_else(|| "aghub-api".to_string())
}

/// Probe a connection without mutating anything. Reports reachability,
/// whether a compatible `aghub-api` is present, and a human-facing message.
#[tauri::command]
pub fn test_connection(connection: Connection) -> TestResult {
	let runner = SystemRunner;
	probe_connection(&runner, &connection, LOCAL_VERSION)
}

/// Force-reinstall `aghub-api` on the remote and return a fresh probe result.
///
/// This is deliberately separate from [`force_redeploy_remote`]: the connection
/// manager needs a test-panel action that mutates the remote binary without
/// opening a tunnel or switching the active connection.
#[tauri::command]
pub fn reinstall_remote_api(
	state: State<'_, RemoteState>,
	app: AppHandle,
	connection: Connection,
) -> Result<TestResult, RemoteError> {
	let id = connection.id.clone();

	if existing_local_port(&state, &id)?.is_some() {
		return Err(RemoteError::AlreadyConnecting);
	}

	let source = remote_install_source(&app).ok_or_else(|| {
		RemoteError::RemoteApiMissing {
			install_hint: install_hint(),
		}
	})?;

	let runner = SystemRunner;
	let source = deployable_install_source(&runner, &connection, source)?;
	info!(
		"remote '{}': force reinstall aghub-api using {}",
		connection.id,
		install_source_summary(&source)
	);
	let _slot = SlotGuard::claim(&state.connecting, id)?;

	let outcome =
		reinstall_remote_api_core(&runner, &connection, LOCAL_VERSION, &source);
	if let Ok(result) = &outcome {
		info!(
			"remote '{}': force reinstall finished: version={:?}, \
			 compatible={}, install_attempted={}, install_succeeded={}",
			connection.id,
			result.api_version,
			result.compatible,
			result.install_attempted,
			result.install_succeeded
		);
	}

	outcome.map_err(RemoteError::from)
}

/// The desktop's embedded `aghub-api` version (`aghub_api::VERSION`), i.e. the
/// version `is_version_compatible` enforces against the remote. This is the
/// **workspace** version, distinct from the Tauri app version reported by
/// `@tauri-apps/api/app`'s `getVersion()`.
#[tauri::command]
pub fn local_api_version() -> &'static str {
	LOCAL_VERSION
}

/// Return selectable aliases discovered from the user's local `~/.ssh/config`.
#[tauri::command]
pub fn list_ssh_config_hosts() -> Vec<SshConfigHost> {
	read_default_ssh_config_hosts()
}

/// List immediate child directories on a remote VM for the project picker.
#[tauri::command]
pub fn list_remote_directories(
	connection: Connection,
	path: String,
) -> Result<RemoteDirectoryListing, RemoteError> {
	let runner = SystemRunner;
	list_remote_directories_core(&runner, &connection, &path)
		.map_err(Into::into)
}

/// Bring up the remote server and a tunnel; returns the **local** port the
/// frontend should point its `baseUrl` at. Idempotent per connection id.
#[tauri::command]
pub fn connect_remote(
	state: State<'_, RemoteState>,
	app: AppHandle,
	connection: Connection,
) -> Result<u16, RemoteError> {
	let id = connection.id.clone();

	// Already connected? Reuse the existing tunnel.
	if let Some(port) = existing_local_port(&state, &id)? {
		return Ok(port);
	}
	// Claim an in-progress slot so concurrent calls don't double-bring-up; the
	// guard releases it on every exit path (including a panic in `bring_up`).
	let _slot = SlotGuard::claim(&state.connecting, id.clone())?;

	// Do the slow ssh work WITHOUT holding any lock.
	let handle = bring_up(&app, &connection)?;
	let port = handle.local_port;
	insert_handle_or_teardown(&state, id, handle)?;
	Ok(port)
	// `_slot` drops here, after the handle is stored.
}

/// Force-redeploy the desktop's `aghub-api` over an incompatible remote one,
/// then connect; returns the **local** tunnel port. Only reachable from the
/// incompatible/failed state: it refuses to clobber a live connection and is
/// gated to the desktop's own `(os, arch)`.
#[tauri::command]
pub fn force_redeploy_remote(
	state: State<'_, RemoteState>,
	app: AppHandle,
	connection: Connection,
) -> Result<u16, RemoteError> {
	let id = connection.id.clone();

	// A live handle means there is a working connection — never tear it down.
	if existing_local_port(&state, &id)?.is_some() {
		return Err(RemoteError::AlreadyConnecting);
	}

	// Resolve the install source (dev fallback until bundling lands).
	let source = remote_install_source(&app).ok_or_else(|| {
		RemoteError::RemoteApiMissing {
			install_hint: install_hint(),
		}
	})?;

	// Same-platform gate BEFORE any mutation, but ONLY for a bundled/local
	// binary: a wrong-arch binary would not run on the VM. VM-native install
	// sources (`ReleaseDeb`, `CargoGit`) are arch-safe for the supported remote
	// platform, so skip the refusal once `deployable_install_source` has picked
	// one for the common Mac-desktop -> Linux-VM case.
	// Mirrors the connect path (`ensure_remote_api`), which gates only
	// `LocalBinary`.
	let runner = SystemRunner;
	let source = deployable_install_source(&runner, &connection, source)?;

	// Claim the in-progress slot so a concurrent connect can't race us; the
	// guard releases it on every exit path (including a panic mid-redeploy).
	let _slot = SlotGuard::claim(&state.connecting, id.clone())?;

	let handle = force_redeploy(&app, &connection, &source)?;
	let port = handle.local_port;
	insert_handle_or_teardown(&state, id, handle)?;
	Ok(port)
	// `_slot` drops here, after the handle is stored.
}

/// Tear down a connection: kill the local tunnel and the remote server.
#[tauri::command]
pub fn disconnect_remote(
	state: State<'_, RemoteState>,
	connection_id: String,
) -> Result<(), RemoteError> {
	let handle = lock(&state.handles)?.remove(&connection_id);
	if let Some(handle) = handle {
		teardown(&handle);
	}
	Ok(())
}

/// Whether a connection currently has a live tunnel.
#[tauri::command]
pub fn remote_status(
	state: State<'_, RemoteState>,
	connection_id: String,
) -> bool {
	// Poison-recover: a poisoned lock must not silently report "disconnected"
	// for a connection that is in fact still live.
	lock_recover(&state.handles).contains_key(&connection_id)
}

/// Kill every live remote connection. Called on app exit.
pub fn cleanup_all_remotes(state: &RemoteState) {
	// Poison-recover and still drain: app-exit cleanup must tear down every
	// live tunnel + remote server even if a lock holder panicked.
	let handles: Vec<RemoteHandle> = lock_recover(&state.handles)
		.drain()
		.map(|(_, h)| h)
		.collect();
	for handle in &handles {
		teardown(handle);
	}
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

/// Local port of an already-live connection, or `None` if not connected.
///
/// Returns `Err` on a poisoned lock so connect/force_redeploy surface
/// [`RemoteError::Internal`] instead of silently treating a poisoned (and thus
/// indeterminate) state as "disconnected" and racing a second bring-up.
fn existing_local_port(
	state: &State<'_, RemoteState>,
	id: &str,
) -> Result<Option<u16>, RemoteError> {
	Ok(lock(&state.handles)?.get(id).map(|h| h.local_port))
}

/// User-facing lock: a poisoned lock becomes [`RemoteError::Internal`] so the
/// caller can abort the operation cleanly.
fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, RemoteError> {
	m.lock().map_err(|e| RemoteError::Internal {
		message: format!("state lock poisoned: {e}"),
	})
}

/// Poison-recovering lock for teardown / cleanup paths that MUST make progress
/// even after a panic poisoned the mutex (app-exit cleanup, the watcher's
/// handle removal, the slot guard's drop). Recovers via `into_inner`.
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Store a freshly brought-up handle, or — if `state.handles` is poisoned —
/// tear the new tunnel + remote server down and report
/// [`RemoteError::Internal`].
///
/// The handle is not yet reachable by disconnect/cleanup (the caller only owns
/// it locally), so on a poisoned-lock failure we are the only owner and must
/// clean it up here rather than leak the tunnel and remote process.
fn insert_handle_or_teardown(
	state: &State<'_, RemoteState>,
	id: String,
	handle: RemoteHandle,
) -> Result<(), RemoteError> {
	match state.handles.lock() {
		Ok(mut handles) => {
			handles.insert(id, handle);
			Ok(())
		}
		Err(poisoned) => {
			// Recover the map so future calls still work, store nothing, and
			// tear down the orphan we were about to register.
			let _guard = poisoned.into_inner();
			teardown(&handle);
			Err(RemoteError::Internal {
				message: "state lock poisoned while registering connection"
					.to_string(),
			})
		}
	}
}

fn deployable_install_source(
	runner: &SystemRunner,
	connection: &Connection,
	source: RemoteInstallSource,
) -> Result<RemoteInstallSource, RemoteError> {
	if !matches!(source, RemoteInstallSource::LocalBinary(_)) {
		return Ok(source);
	}

	let probed = probe_remote_platform(runner, connection);
	let same_platform = matches!(
		&probed,
		Some((os, arch))
			if os == std::env::consts::OS && arch == std::env::consts::ARCH
	);
	if same_platform {
		return Ok(source);
	}

	if let Some(fallback) = remote_release_deb_install_source(&probed) {
		info!(
			"remote '{}': local install binary is not deployable to remote \
			 platform {:?}; falling back to {}",
			connection.id,
			probed,
			install_source_summary(&fallback)
		);
		return Ok(fallback);
	}

	if let Some(fallback) = remote_git_install_source() {
		info!(
			"remote '{}': no release asset install source for remote \
			 platform {:?}; falling back to {}",
			connection.id,
			probed,
			install_source_summary(&fallback)
		);
		return Ok(fallback);
	}

	let remote_platform = probed
		.map(|(os, arch)| format!("{os}/{arch}"))
		.unwrap_or_else(|| "unknown".to_string());
	Err(RemoteError::CrossPlatformRedeploy {
		remote_platform,
		hint: install_hint(),
	})
}

/// The full bring-up sequence (probe → start → tunnel → watcher). No shared
/// locks are held here.
fn bring_up(
	app: &AppHandle,
	connection: &Connection,
) -> Result<RemoteHandle, RemoteError> {
	let runner = SystemRunner;
	let bin = resolved_path(connection);

	info!(
		"remote '{}': ensuring aghub-api '{}' for local version {}",
		connection.id, bin, LOCAL_VERSION
	);
	let install_source = remote_install_source(app)
		.map(|source| deployable_install_source(&runner, connection, source))
		.transpose()?;
	match &install_source {
		Some(source) => info!(
			"remote '{}': install source resolved to {}",
			connection.id,
			install_source_summary(source)
		),
		None => info!(
			"remote '{}': no install source available; probe only",
			connection.id
		),
	}
	let test = ensure_remote_api(
		&runner,
		connection,
		LOCAL_VERSION,
		install_source.as_ref(),
	)?;
	info!(
		"remote '{}': aghub-api ensure finished: version={:?}, \
		 compatible={}, install_attempted={}, install_succeeded={}",
		connection.id,
		test.api_version,
		test.compatible,
		test.install_attempted,
		test.install_succeeded
	);
	if !test.compatible {
		return Err(RemoteError::Incompatible {
			remote_version: test.api_version,
		});
	}

	let started = start_remote(
		&runner,
		connection,
		&bin,
		PORT_POLL_ATTEMPTS,
		PORT_POLL_DELAY,
	)?;

	finish_bring_up(app, connection, started)
}

/// Tail of the bring-up shared by [`connect_remote`] and
/// [`force_redeploy_remote`]: allocate a local port, spawn the ssh tunnel,
/// settle-check it, start the watcher, and return the [`RemoteHandle`].
///
/// **Invariant:** on entry the remote server is already running, so every early
/// return MUST guarded-kill it (the `RemoteHandle` is only stored by the caller
/// after this returns `Ok`, so disconnect/cleanup cannot reach it yet).
fn finish_bring_up(
	app: &AppHandle,
	connection: &Connection,
	started: StartedServer,
) -> Result<RemoteHandle, RemoteError> {
	let runner = SystemRunner;

	let local_port = match find_available_port() {
		Ok(port) => port,
		Err(message) => {
			aghub_remote::bringup::cleanup_remote(
				&runner,
				connection,
				started.remote_pid,
			);
			return Err(RemoteError::TunnelFailed { message });
		}
	};

	let tunnel_args =
		build_tunnel_args(connection, local_port, started.remote_port);
	// Spawn through the runner so the `#[cfg(windows)] CREATE_NO_WINDOW` flag
	// (applied in `SystemRunner::spawn`) is preserved and we get a cloneable,
	// kill-able `ChildHandle` instead of a raw pid.
	let tunnel = match runner.spawn("ssh", &tunnel_args) {
		Ok(tunnel) => tunnel,
		Err(e) => {
			aghub_remote::bringup::cleanup_remote(
				&runner,
				connection,
				started.remote_pid,
			);
			return Err(RemoteError::TunnelFailed {
				message: e.to_string(),
			});
		}
	};

	// Give the forward a moment; if ssh already exited, the forward failed.
	std::thread::sleep(TUNNEL_SETTLE);
	if let Ok(Some(status)) = tunnel.try_wait() {
		// Tunnel died immediately — clean up the orphaned remote server.
		aghub_remote::bringup::cleanup_remote(
			&runner,
			connection,
			started.remote_pid,
		);
		return Err(RemoteError::TunnelFailed {
			message: format!("ssh tunnel exited early ({status})"),
		});
	}

	let intentional = Arc::new(AtomicBool::new(false));
	spawn_tunnel_watcher(
		app.clone(),
		connection.clone(),
		tunnel.clone(),
		started.remote_pid,
		intentional.clone(),
	);

	info!(
		"remote '{}' connected: local port {} -> VM port {}",
		connection.id, local_port, started.remote_port
	);
	Ok(RemoteHandle {
		tunnel,
		remote_pid: started.remote_pid,
		local_port,
		connection: connection.clone(),
		intentional,
	})
}

/// Force-redeploy the desktop's version-locked `aghub-api` over a present-but-
/// incompatible remote one, then start + tunnel + connect. Same-platform-gated
/// (a wrong-arch binary would not run) and only reachable from the failed /
/// incompatible state; it refuses to clobber a live connection.
fn force_redeploy(
	app: &AppHandle,
	connection: &Connection,
	source: &RemoteInstallSource,
) -> Result<RemoteHandle, RemoteError> {
	let runner = SystemRunner;
	let test =
		force_redeploy_remote_api(&runner, connection, LOCAL_VERSION, source)?;
	if !test.compatible {
		// Redeploy ran but the re-probe is still incompatible — surface it
		// rather than starting the wrong binary.
		return Err(RemoteError::Incompatible {
			remote_version: test.api_version,
		});
	}

	let bin = resolved_path(connection);
	let started = start_remote(
		&runner,
		connection,
		&bin,
		PORT_POLL_ATTEMPTS,
		PORT_POLL_DELAY,
	)?;

	finish_bring_up(app, connection, started)
}

/// Resolve the bundled, version-locked `aghub-api` shipped as a Tauri
/// resource, or `None` in a dev build where it was never bundled. The
/// `.exists()` gate keeps `bun run start` from the repo on the env/cargo-git
/// fallback (the resource was never staged there). The executable bit is NOT
/// handled here — the remote finish step (`crates/remote/src/ssh.rs`) chmods.
fn bundled_api_path(app: &AppHandle) -> Option<PathBuf> {
	let name = if cfg!(windows) {
		"binaries/aghub-api.exe"
	} else {
		"binaries/aghub-api"
	};
	let path = app.path().resolve(name, BaseDirectory::Resource).ok()?;
	path.exists().then_some(path)
}

/// Pure precedence: bundled `LocalBinary` -> env `LocalBinary` -> `CargoGit`.
/// Extracted from [`remote_install_source`] so the bundled-first ordering is
/// unit-testable without an `AppHandle`. A blank `env_binary` is ignored.
fn pick_install_source(
	bundled: Option<PathBuf>,
	env_binary: Option<String>,
	git_url: Option<String>,
	git_branch: Option<String>,
	git_tag: Option<String>,
) -> Option<RemoteInstallSource> {
	if let Some(path) = bundled {
		return Some(RemoteInstallSource::LocalBinary(path));
	}
	if let Some(raw) = env_binary {
		let trimmed = raw.trim();
		if !trimmed.is_empty() {
			return Some(RemoteInstallSource::LocalBinary(PathBuf::from(
				trimmed,
			)));
		}
	}
	let url = git_url?;
	Some(RemoteInstallSource::CargoGit {
		url,
		branch: git_branch,
		tag: git_tag,
	})
}

/// Resolve where an automatic remote install should source `aghub-api`: the
/// bundled resource first (shipped builds), then the dev env override, then
/// the git checkout (`cargo install --git`). Thin wrapper that performs the
/// I/O (resource resolve + env/git reads) and delegates ordering to the pure
/// [`pick_install_source`].
fn remote_install_source(app: &AppHandle) -> Option<RemoteInstallSource> {
	let bundled = bundled_api_path(app);
	let env_binary = std::env::var("AGHUB_REMOTE_API_BINARY").ok();
	pick_install_source(
		bundled,
		env_binary,
		remote_git_url(),
		remote_git_branch(),
		remote_git_tag(),
	)
}

fn install_source_summary(source: &RemoteInstallSource) -> String {
	match source {
		RemoteInstallSource::LocalBinary(_) => "local-binary".to_string(),
		RemoteInstallSource::CargoGit { url, branch, tag } => {
			format!("cargo-git url={url} branch={branch:?} tag={tag:?}")
		}
		RemoteInstallSource::ReleaseDeb { url } => {
			format!("release-deb url={url}")
		}
	}
}

fn remote_release_deb_install_source(
	probed: &Option<(String, String)>,
) -> Option<RemoteInstallSource> {
	let (os, arch) = probed.as_ref()?;
	let deb_arch = release_deb_arch(os, arch)?;
	Some(RemoteInstallSource::ReleaseDeb {
		url: release_deb_url(LOCAL_VERSION, deb_arch),
	})
}

fn release_deb_arch(os: &str, arch: &str) -> Option<&'static str> {
	if os != "linux" {
		return None;
	}
	match arch {
		"x86_64" | "amd64" => Some("amd64"),
		_ => None,
	}
}

fn release_deb_url(version: &str, deb_arch: &str) -> String {
	format!(
		"{DEFAULT_REMOTE_RELEASE_BASE_URL}/v{version}/aghub_{version}_{deb_arch}.deb"
	)
}

fn remote_git_install_source() -> Option<RemoteInstallSource> {
	let url = remote_git_url()?;
	Some(RemoteInstallSource::CargoGit {
		url,
		branch: remote_git_branch(),
		tag: remote_git_tag(),
	})
}

fn remote_git_url() -> Option<String> {
	std::env::var("AGHUB_REMOTE_INSTALL_GIT_URL")
		.ok()
		.filter(|s| !s.trim().is_empty())
		.or_else(|| git_output(&["remote", "get-url", "origin"]))
		.or_else(|| Some(DEFAULT_REMOTE_INSTALL_GIT_URL.to_string()))
}

fn remote_git_branch() -> Option<String> {
	std::env::var("AGHUB_REMOTE_INSTALL_GIT_BRANCH")
		.ok()
		.filter(|s| !s.trim().is_empty())
		.or_else(|| git_output(&["branch", "--show-current"]))
}

fn remote_git_tag() -> Option<String> {
	if remote_git_branch().is_some() {
		return None;
	}
	std::env::var("AGHUB_REMOTE_INSTALL_GIT_TAG")
		.ok()
		.filter(|s| !s.trim().is_empty())
		.or_else(|| Some(format!("v{LOCAL_VERSION}")))
}

fn git_output(args: &[&str]) -> Option<String> {
	let mut command = Command::new("git");
	command.args(args);
	#[cfg(windows)]
	command.creation_flags(crate::CREATE_NO_WINDOW);
	let output = command.output().ok()?;
	if !output.status.success() {
		return None;
	}
	let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
	(!value.is_empty()).then_some(value)
}

/// How often the watcher polls the tunnel child for exit.
const WATCHER_POLL_DELAY: Duration = Duration::from_millis(250);

/// Watch the tunnel child; when it exits, clean up the remote server and — if
/// the exit was unexpected — notify the frontend and drop the handle.
///
/// Polls with `try_wait()` instead of a blocking `wait()`: the child lives
/// behind a shared `Mutex<Child>` (the [`RemoteHandle`] holds a clone), so a
/// blocking wait here would hold the lock and deadlock a concurrent
/// `teardown` → `tunnel.kill()`.
fn spawn_tunnel_watcher(
	app: AppHandle,
	connection: Connection,
	tunnel: ChildHandle,
	remote_pid: u32,
	intentional: Arc<AtomicBool>,
) {
	let connection_id = connection.id.clone();
	std::thread::spawn(move || {
		loop {
			match tunnel.try_wait() {
				Ok(Some(_)) => break,
				// Still running, or a transient errno — sleep and retry.
				Ok(None) | Err(_) => {
					std::thread::sleep(WATCHER_POLL_DELAY);
				}
			}
		}
		let runner = SystemRunner;
		aghub_remote::bringup::cleanup_remote(&runner, &connection, remote_pid);

		if !intentional.load(Ordering::SeqCst) {
			warn!("remote '{connection_id}' tunnel exited unexpectedly");
			let _ = app.emit(
				"remote-disconnected",
				RemoteDisconnected {
					connection_id: connection_id.clone(),
				},
			);
			if let Some(state) = app.try_state::<RemoteState>() {
				// Poison-recover so an unexpected disconnect still drops the
				// dead handle even after a panic poisoned the map.
				lock_recover(&state.handles).remove(&connection_id);
			}
		}
	});
}

/// Kill the local tunnel (which wakes the watcher) and the remote server.
fn teardown(handle: &RemoteHandle) {
	handle.intentional.store(true, Ordering::SeqCst);
	// Kill the local tunnel directly via the owned `Child`. This is
	// cross-platform (`SIGKILL` on Unix, `TerminateProcess` on Windows) and,
	// because we still hold the live child, there is no pid-reuse window — we
	// always signal exactly this process, never a recycled pid. Killing it also
	// wakes the watcher's `try_wait` poll, which then runs the remote cleanup.
	let _ = handle.tunnel.kill();
	// Guarded remote kill (idempotent with the watcher's own cleanup).
	let runner = SystemRunner;
	aghub_remote::bringup::cleanup_remote(
		&runner,
		&handle.connection,
		handle.remote_pid,
	);
}

/// Whether this build can resolve a source to deploy `aghub-api` to a remote.
/// True in a bundled build (the embedded resource resolves) or a dev build
/// with an env var / git checkout; false in a shipped build with none, so the
/// UI can hide the otherwise-dead "Force redeploy" affordance.
#[tauri::command]
pub fn remote_install_source_available(app: AppHandle) -> bool {
	remote_install_source(&app).is_some()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	/// The slot guard releases its claim even when the work between claim and
	/// drop panics — otherwise a panicked bring-up would wedge a connection in
	/// the perpetual `AlreadyConnecting` state.
	#[test]
	fn slot_guard_drop_removes_slot_after_panic() {
		let set: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
		let id = "vm-1".to_string();

		let result =
			std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
				let _slot = SlotGuard::claim(&set, id.clone()).unwrap();
				panic!("bring-up blew up");
			}));
		assert!(result.is_err(), "the closure should have panicked");

		// The set was poisoned by the panic; `lock_recover` still sees it, and
		// the guard's Drop must have removed the id.
		assert!(
			!lock_recover(&set).contains(&id),
			"slot must be released after a panic"
		);
	}

	/// A second claim on an already-held id is rejected.
	#[test]
	fn slot_guard_rejects_duplicate() {
		let set: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
		let id = "vm-1".to_string();

		let first = SlotGuard::claim(&set, id.clone()).unwrap();
		let second = SlotGuard::claim(&set, id.clone());
		assert!(
			matches!(second, Err(RemoteError::AlreadyConnecting)),
			"a duplicate claim must be AlreadyConnecting"
		);

		drop(first);
		// Once released, the id can be claimed again.
		assert!(SlotGuard::claim(&set, id).is_ok());
	}

	/// `lock_recover` hands back the inner data even after the mutex was
	/// poisoned by a panic while a guard was held.
	#[test]
	fn lock_recover_returns_inner_after_poison() {
		let m: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

		let result =
			std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
				let mut guard = m.lock().unwrap();
				guard.insert("present".to_string());
				panic!("poison the mutex");
			}));
		assert!(result.is_err(), "the closure should have panicked");

		// The standard `lock()` would return Err here; `lock_recover` must not.
		let recovered = lock_recover(&m);
		assert!(
			recovered.contains("present"),
			"recovered data should retain the pre-panic insert"
		);
	}

	// --- pick_install_source precedence (pure, no AppHandle) --------------

	#[test]
	fn pick_source_prefers_bundled_over_env_and_git() {
		let src = pick_install_source(
			Some(PathBuf::from("/Applications/aghub.app/.../aghub-api")),
			Some("/dev/override/aghub-api".to_string()),
			Some("https://github.com/audichuang/aghub.git".to_string()),
			Some("main".to_string()),
			None,
		);
		assert_eq!(
			src,
			Some(RemoteInstallSource::LocalBinary(PathBuf::from(
				"/Applications/aghub.app/.../aghub-api"
			))),
			"a bundled binary wins over env + git"
		);
	}

	#[test]
	fn pick_source_falls_back_to_env_binary_when_unbundled() {
		let src = pick_install_source(
			None,
			Some("/dev/override/aghub-api".to_string()),
			Some("https://example.com/aghub.git".to_string()),
			None,
			None,
		);
		assert_eq!(
			src,
			Some(RemoteInstallSource::LocalBinary(PathBuf::from(
				"/dev/override/aghub-api"
			))),
			"env binary wins over git when unbundled"
		);
	}

	#[test]
	fn pick_source_falls_back_to_cargo_git_when_no_binary() {
		let src = pick_install_source(
			None,
			None,
			Some("https://example.com/aghub.git".to_string()),
			Some("feat/x".to_string()),
			None,
		);
		assert_eq!(
			src,
			Some(RemoteInstallSource::CargoGit {
				url: "https://example.com/aghub.git".to_string(),
				branch: Some("feat/x".to_string()),
				tag: None,
			}),
			"cargo-git is the last resort"
		);
	}

	#[test]
	fn pick_source_is_none_when_nothing_resolves() {
		assert_eq!(pick_install_source(None, None, None, None, None), None);
	}

	#[test]
	fn pick_source_ignores_blank_env_binary() {
		// A blank/whitespace env var must not be treated as a real path; it
		// falls through to git.
		let src = pick_install_source(
			None,
			Some("   ".to_string()),
			Some("https://example.com/aghub.git".to_string()),
			None,
			None,
		);
		assert_eq!(
			src,
			Some(RemoteInstallSource::CargoGit {
				url: "https://example.com/aghub.git".to_string(),
				branch: None,
				tag: None,
			}),
			"a blank env binary is ignored, falling through to git"
		);
	}

	#[test]
	fn release_deb_source_targets_linux_x86_64_release_asset() {
		let source = remote_release_deb_install_source(&Some((
			"linux".to_string(),
			"x86_64".to_string(),
		)));
		assert_eq!(
			source,
			Some(RemoteInstallSource::ReleaseDeb {
				url: release_deb_url(LOCAL_VERSION, "amd64"),
			})
		);
	}

	#[test]
	fn release_deb_source_ignores_unsupported_platforms() {
		assert_eq!(
			remote_release_deb_install_source(&Some((
				"darwin".to_string(),
				"x86_64".to_string(),
			))),
			None
		);
		assert_eq!(
			remote_release_deb_install_source(&Some((
				"linux".to_string(),
				"aarch64".to_string(),
			))),
			None
		);
	}
}
