pub mod credentials;
pub mod logging;
pub mod remote;
pub mod server;
pub mod skill_check;
pub mod window;

pub use credentials::{list_bound_sources, resolve_git_token};
pub use logging::{
	clear_log_files, export_diagnostic_logs, get_log_dir_path, get_log_entries,
	get_log_stats,
};
pub use remote::{
	cleanup_all_remotes, connect_remote, disconnect_remote,
	force_redeploy_remote, list_remote_directories, list_ssh_config_hosts,
	local_api_version, reinstall_remote_api, remote_install_source_available,
	remote_status, test_connection, RemoteState,
};
pub use server::start_server;
pub use skill_check::{
	get_last_skill_check, get_skill_check_schedule, resolve_aghub_cli,
	set_skill_check_schedule,
};
pub use window::minimize_to_tray;

#[cfg(test)]
mod tests {
	/// A sync `#[tauri::command] fn` runs on the main (UI) thread, so one that
	/// spawns a process or touches the network freezes the webview. Every
	/// command must be `async` (blocking work in `spawn_blocking`) unless it is
	/// listed here as instant.
	const INSTANT_SYNC_COMMANDS: &[&str] =
		&["local_api_version", "remote_status"];

	#[test]
	fn tauri_commands_are_async_unless_instant() {
		let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
		let mut sync = Vec::new();
		let mut stack = vec![dir];
		while let Some(dir) = stack.pop() {
			for entry in std::fs::read_dir(&dir).unwrap() {
				let path = entry.unwrap().path();
				if path.is_dir() {
					stack.push(path);
					continue;
				}
				if path.extension().is_none_or(|e| e != "rs") {
					continue;
				}
				let src = std::fs::read_to_string(&path).unwrap();
				let mut lines = src.lines();
				while let Some(line) = lines.next() {
					if !line.trim_start().starts_with("#[tauri::command") {
						continue;
					}
					// The signature is the first line after the attribute(s).
					let sig = lines
						.by_ref()
						.map(str::trim_start)
						.find(|l| !l.starts_with("#["))
						.unwrap_or_default();
					if sig.contains("async fn") {
						continue;
					}
					let name = sig
						.split("fn ")
						.nth(1)
						.and_then(|rest| rest.split(['(', '<']).next())
						.unwrap_or(sig);
					if !INSTANT_SYNC_COMMANDS.contains(&name) {
						sync.push(format!("{}: {name}", path.display()));
					}
				}
			}
		}
		assert!(sync.is_empty(), "sync #[tauri::command] fns: {sync:#?}");
	}
}
