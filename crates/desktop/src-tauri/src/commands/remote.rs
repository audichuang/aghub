//! Tauri command layer for remote SSH management.
//!
//! All the testable transport + bring-up logic lives in the tauri-free
//! `aghub-remote` crate (unit-tested with a `MockRunner`). This module is the
//! thin glue: it deserializes the `Connection` payload from the frontend, drives
//! the bring-up via a real [`SystemRunner`], owns the local tunnel child + a
//! watcher thread that reports unexpected disconnects, and tracks live handles
//! so they can be torn down on disconnect and on app exit.
//!
//! Commands are **synchronous** `#[tauri::command] fn` on purpose: Tauri runs
//! sync commands on its worker thread-pool, so the blocking `std::process` ssh
//! work never stalls the (rt-only, single-thread) tokio runtime. The frontend
//! still receives a Promise from `invoke`.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aghub_remote::bringup::{
	probe_connection, start_remote, ConnectError, TestResult,
};
use aghub_remote::ssh::{build_tunnel_args, Connection, SystemRunner};
use log::{info, warn};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::server::find_available_port;

/// Local desktop version, used to gate remote `aghub-api` compatibility.
const LOCAL_VERSION: &str = env!("CARGO_PKG_VERSION");
/// How many times to poll the remote log for the bound port.
const PORT_POLL_ATTEMPTS: u32 = 40;
/// Delay between port-poll attempts (~40 * 250ms = 10s budget).
const PORT_POLL_DELAY: Duration = Duration::from_millis(250);
/// Grace period after spawning the tunnel before we check it stayed up.
const TUNNEL_SETTLE: Duration = Duration::from_millis(400);

/// A live remote connection: the local tunnel process, the remote server pid,
/// and the connection it belongs to.
struct RemoteHandle {
	/// PID of the local `ssh -L` tunnel process (used for teardown).
	tunnel_pid: u32,
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
	/// The remote server never reported its port within the poll budget.
	StartTimeout,
	/// The local ssh tunnel failed to establish the port-forward.
	TunnelFailed { message: String },
	/// A bring-up for this connection is already in progress.
	AlreadyConnecting,
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
			ConnectError::StartTimeout => RemoteError::StartTimeout,
			ConnectError::TunnelFailed(message) => {
				RemoteError::TunnelFailed { message }
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

/// Probe a connection without mutating anything. Reports reachability, whether a
/// compatible `aghub-api` is present, and a human-facing message.
#[tauri::command]
pub fn test_connection(connection: Connection) -> TestResult {
	let runner = SystemRunner;
	probe_connection(&runner, &connection, LOCAL_VERSION)
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
	if let Some(port) = existing_local_port(&state, &id) {
		return Ok(port);
	}
	// Claim an in-progress slot so concurrent calls don't double-bring-up.
	{
		let mut connecting = lock(&state.connecting)?;
		if !connecting.insert(id.clone()) {
			return Err(RemoteError::AlreadyConnecting);
		}
	}

	// Do the slow ssh work WITHOUT holding any lock.
	let outcome = bring_up(&app, &connection);

	// Release the in-progress slot regardless of result.
	if let Ok(mut connecting) = state.connecting.lock() {
		connecting.remove(&id);
	}

	let handle = outcome?;
	let port = handle.local_port;
	lock(&state.handles)?.insert(id, handle);
	Ok(port)
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
	state
		.handles
		.lock()
		.map(|h| h.contains_key(&connection_id))
		.unwrap_or(false)
}

/// Kill every live remote connection. Called on app exit.
pub fn cleanup_all_remotes(state: &RemoteState) {
	let handles: Vec<RemoteHandle> = match state.handles.lock() {
		Ok(mut guard) => guard.drain().map(|(_, h)| h).collect(),
		Err(_) => return,
	};
	for handle in &handles {
		teardown(handle);
	}
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

fn existing_local_port(
	state: &State<'_, RemoteState>,
	id: &str,
) -> Option<u16> {
	state.handles.lock().ok()?.get(id).map(|h| h.local_port)
}

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, RemoteError> {
	m.lock().map_err(|e| RemoteError::Internal {
		message: format!("state lock poisoned: {e}"),
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

	let test = probe_connection(&runner, connection, LOCAL_VERSION);
	if !test.reachable {
		return Err(RemoteError::Unreachable {
			stderr: test.message,
		});
	}
	if !test.api_present {
		// v1: detect-or-instruct. Auto-scp deploy is a documented v2 follow-up.
		return Err(RemoteError::RemoteApiMissing {
			install_hint: install_hint(),
		});
	}
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

	let local_port = find_available_port()
		.map_err(|message| RemoteError::TunnelFailed { message })?;

	let tunnel_args =
		build_tunnel_args(connection, local_port, started.remote_port);
	let mut tunnel =
		Command::new("ssh")
			.args(&tunnel_args)
			.spawn()
			.map_err(|e| RemoteError::TunnelFailed {
				message: e.to_string(),
			})?;
	let tunnel_pid = tunnel.id();

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
		tunnel,
		started.remote_pid,
		intentional.clone(),
	);

	info!(
		"remote '{}' connected: local port {} -> VM port {}",
		connection.id, local_port, started.remote_port
	);
	Ok(RemoteHandle {
		tunnel_pid,
		remote_pid: started.remote_pid,
		local_port,
		connection: connection.clone(),
		intentional,
	})
}

/// Watch the tunnel child; when it exits, clean up the remote server and — if
/// the exit was unexpected — notify the frontend and drop the handle.
fn spawn_tunnel_watcher(
	app: AppHandle,
	connection: Connection,
	mut tunnel: std::process::Child,
	remote_pid: u32,
	intentional: Arc<AtomicBool>,
) {
	let connection_id = connection.id.clone();
	std::thread::spawn(move || {
		let _ = tunnel.wait();
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
				if let Ok(mut handles) = state.handles.lock() {
					handles.remove(&connection_id);
				}
			}
		}
	});
}

/// Kill the local tunnel (which wakes the watcher) and the remote server.
fn teardown(handle: &RemoteHandle) {
	handle.intentional.store(true, Ordering::SeqCst);
	// Kill the local tunnel by pid; the watcher's wait() then returns.
	let _ = Command::new("kill")
		.arg(handle.tunnel_pid.to_string())
		.status();
	// Guarded remote kill (idempotent with the watcher's own cleanup).
	let runner = SystemRunner;
	aghub_remote::bringup::cleanup_remote(
		&runner,
		&handle.connection,
		handle.remote_pid,
	);
}

/// Install hint shown when the VM has no compatible `aghub-api`.
fn install_hint() -> String {
	"Install aghub-api on the VM (e.g. `cargo install --path crates/api` or \
	 `just install`) and ensure it is on PATH, then reconnect."
		.to_string()
}
