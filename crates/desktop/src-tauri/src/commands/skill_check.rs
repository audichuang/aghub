//! Optional OS-level schedule that runs the CLI's READ-ONLY update check while
//! the desktop app is closed, plus the sidecar readback.
//!
//! The registered command is always `aghub-cli check skills --online -g --json
//! --write-result <sidecar>`: it never applies an update (no `--yes`, no
//! `apply-update`) and never starts the desktop binary. Every platform builder
//! runs its payload through [`schedule_is_check_only`] before it touches disk,
//! and the builders are pure + compiled on all platforms so the payload tests
//! run everywhere — not only on the OS that ships that backend.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

const SIDECAR_NAME: &str = "skill-check-last.json";

/// Local wall-clock hour for the daily run, identical on all three backends.
/// Not midnight: a laptop that is off overnight would simply never run the
/// Windows task (schtasks has no `Persistent=` equivalent without an XML
/// definition).
const SCHEDULE_HOUR: u32 = 9;

/// Whether this build has a registration backend. False only on platforms with
/// none, where the UI hides the row instead of offering a switch that errors.
pub const SCHEDULE_SUPPORTED: bool = cfg!(any(
	target_os = "linux",
	target_os = "macos",
	target_os = "windows"
));

/// Args after the `aghub-cli` program: check-only, global, never apply.
pub fn build_check_args(sidecar: &Path) -> Vec<String> {
	vec![
		"check".into(),
		"skills".into(),
		"--online".into(),
		"-g".into(),
		"--json".into(),
		"--write-result".into(),
		sidecar.to_string_lossy().into_owned(),
	]
}

pub fn build_check_argv(program: &Path, sidecar: &Path) -> Vec<String> {
	let mut argv = vec![program.to_string_lossy().into_owned()];
	argv.extend(build_check_args(sidecar));
	argv
}

/// Judge the ARGV, never the joined string. A substring test asked whether the
/// text "apply-update" appears anywhere — so a data directory named
/// `.../apply-update/` would refuse a perfectly good schedule, while a mutating
/// verb spelled differently could have passed.
pub fn schedule_is_check_only(argv: &[String]) -> bool {
	// argv[0] is the program; the VERB must be exactly `check`.
	let Some(verb) = argv.get(1) else {
		return false;
	};
	if verb != "check" {
		return false;
	}
	let flags = &argv[1..];
	if !flags.iter().any(|a| a == "--online") {
		return false;
	}
	!flags.iter().any(|a| {
		a == "apply-update"
			|| a == "--yes"
			|| a == "-y"
			|| a == "--background-task"
	})
}

/// The one gate every backend goes through before writing a task definition.
///
/// Defence in depth: `build_check_args` is a constant, so this cannot fail
/// today — it exists so a future edit to those args cannot reach an OS
/// scheduler. The predicate itself is covered by
/// `the_check_only_gate_judges_argv_not_the_joined_string`.
fn checked_argv(cli: &Path, sidecar: &Path) -> Result<Vec<String>, String> {
	let argv = build_check_argv(cli, sidecar);
	if !schedule_is_check_only(&argv) {
		return Err("refusing to schedule a non-check command".into());
	}
	Ok(argv)
}

/// The sidecar the SCHEDULED CLI writes, so the root must be the shared one
/// (`aghub_core::paths::app_data_dir`, reached here through `aghub-api`):
/// `$AGHUB_DATA_DIR`, else `dirs::data_dir()/aghub`. Deliberately NOT Tauri's
/// `app_data_dir()`, which is identifier-scoped (`<data>/com.akrc.aghub`) and
/// would read a file the CLI never writes.
pub fn default_sidecar_path() -> PathBuf {
	aghub_api::default_app_data_dir().join(SIDECAR_NAME)
}

// ---------------------------------------------------------------------------
// Task payloads + registration backends
//
// EVERY backend is compiled on EVERY platform, so the two this build cannot
// dispatch to are dead code by design — hence the module-wide allow. That is
// the point: without it, the only machine that ever compiles `register_launchd`
// is a mac, and the only run that ever notices a wrong launchctl subcommand is
// a user's. Each backend takes its world through `Env` (a HOME and a command
// runner) so the tests below drive all three on Linux against a temp dir and a
// recording runner. What injection cannot prove is the last step — whether
// launchd / Task Scheduler accepts the definition we wrote — so that still
// needs a real machine.
// ---------------------------------------------------------------------------

pub mod schedule_backend {
	#![allow(dead_code)]

	use super::*;

	pub const SYSTEMD_SERVICE: &str = "aghub-skillcheck.service";
	pub const SYSTEMD_TIMER: &str = "aghub-skillcheck.timer";
	pub const LAUNCHD_LABEL: &str = "com.aghub.skillcheck";
	pub const WINDOWS_TASK: &str = "aghub-skillcheck";

	fn shell_single_quote(value: &str) -> String {
		format!("'{}'", value.replace('\'', "'\\''"))
	}

	fn xml_escape(value: &str) -> String {
		value
			.replace('&', "&amp;")
			.replace('<', "&lt;")
			.replace('>', "&gt;")
	}

	pub fn systemd_service_unit(
		cli: &Path,
		sidecar: &Path,
	) -> Result<String, String> {
		let exec = checked_argv(cli, sidecar)?
			.iter()
			.map(|part| shell_single_quote(part))
			.collect::<Vec<_>>()
			.join(" ");
		Ok(format!(
		"[Unit]\nDescription=aghub skill update check (read-only)\n\n[Service]\nType=oneshot\nExecStart={exec}\n"
	))
	}

	pub fn systemd_timer_unit() -> String {
		format!(
		"[Unit]\nDescription=Daily aghub skill update check\n\n[Timer]\nOnCalendar=*-*-* {SCHEDULE_HOUR:02}:00:00\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
	)
	}

	pub fn launchd_plist(cli: &Path, sidecar: &Path) -> Result<String, String> {
		let args = checked_argv(cli, sidecar)?
			.iter()
			.map(|part| format!("\t\t<string>{}</string>\n", xml_escape(part)))
			.collect::<String>();
		Ok(format!(
		"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
		 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
		 <plist version=\"1.0\">\n\
		 <dict>\n\
		 \t<key>Label</key>\n\
		 \t<string>{LAUNCHD_LABEL}</string>\n\
		 \t<key>ProgramArguments</key>\n\
		 \t<array>\n{args}\t</array>\n\
		 \t<key>StartCalendarInterval</key>\n\
		 \t<dict>\n\
		 \t\t<key>Hour</key>\n\
		 \t\t<integer>{SCHEDULE_HOUR}</integer>\n\
		 \t\t<key>Minute</key>\n\
		 \t\t<integer>0</integer>\n\
		 \t</dict>\n\
		 \t<key>RunAtLoad</key>\n\
		 \t<false/>\n\
		 </dict>\n\
		 </plist>\n"
	))
	}

	/// Arguments for `schtasks` itself (the program is `schtasks`, not the CLI).
	/// `/TR` is ONE string that the task scheduler re-parses, so the program path
	/// is quoted inside it.
	/// `/TR` is ONE string that Task Scheduler re-parses with Windows
	/// command-line rules, so EVERY argument needs quoting — not only the
	/// program. `%LOCALAPPDATA%` under `C:\Users\First Last\...` would
	/// otherwise split the sidecar path into two argv entries, and the daily
	/// task would die on a clap error nobody ever sees.
	fn windows_quote(arg: &str) -> String {
		format!("\"{}\"", arg.replace('"', "\\\""))
	}

	pub fn schtasks_create_args(
		cli: &Path,
		sidecar: &Path,
	) -> Result<Vec<String>, String> {
		let argv = checked_argv(cli, sidecar)?;
		let run = argv
			.iter()
			.map(|part| windows_quote(part))
			.collect::<Vec<_>>()
			.join(" ");
		Ok(vec![
			"/Create".into(),
			"/TN".into(),
			WINDOWS_TASK.into(),
			"/TR".into(),
			run,
			"/SC".into(),
			"DAILY".into(),
			"/ST".into(),
			format!("{SCHEDULE_HOUR:02}:00"),
			"/F".into(),
		])
	}

	// ---------------------------------------------------------------------------
	// Process helpers
	// ---------------------------------------------------------------------------

	/// `Command` with the Windows console window suppressed — every one of these
	/// runs from the GUI app, and a flashing `cmd` window is a visible bug.
	pub fn quiet(program: &str) -> Command {
		#[allow(unused_mut)]
		let mut cmd = Command::new(program);
		#[cfg(windows)]
		{
			use std::os::windows::process::CommandExt;
			cmd.creation_flags(crate::CREATE_NO_WINDOW);
		}
		cmd
	}

	pub fn run(program: &str, args: &[&str]) -> Result<(), String> {
		let out = quiet(program)
			.args(args)
			.output()
			.map_err(|err| format!("{program} failed to start: {err}"))?;
		if out.status.success() {
			return Ok(());
		}
		let stderr = String::from_utf8_lossy(&out.stderr);
		let stdout = String::from_utf8_lossy(&out.stdout);
		let detail = if stderr.trim().is_empty() {
			stdout.trim().to_string()
		} else {
			stderr.trim().to_string()
		};
		// Program + its own stderr only. The ARGS carry paths WE generated —
		// the sidecar inside Windows `/TR`, the plist handed to
		// `launchctl bootstrap` — and this string reaches the renderer.
		Err(format!("{program} failed: {detail}"))
	}

	pub fn succeeds(program: &str, args: &[&str]) -> bool {
		quiet(program)
			.args(args)
			.output()
			.map(|out| out.status.success())
			.unwrap_or(false)
	}

	pub fn resolve_aghub_cli_path(
		explicit: Option<String>,
	) -> Result<PathBuf, String> {
		if let Some(path) = explicit {
			let path = PathBuf::from(path);
			// Absolute only: systemd/launchd/schtasks run the task from their
			// own working directory, so a relative path that resolves from the
			// desktop app would silently fail every scheduled run.
			if !path.is_absolute() {
				return Err(
					"the aghub-cli path must be absolute (the scheduler runs from a different directory)"
						.into(),
				);
			}
			if path.is_file() {
				return Ok(path);
			}
			return Err(format!("aghub-cli not found at {}", path.display()));
		}
		let finder = if cfg!(windows) { "where" } else { "which" };
		let output = quiet(finder)
			.arg("aghub-cli")
			.output()
			.map_err(|err| err.to_string())?;
		if !output.status.success() {
			return Err(
			"aghub-cli not on PATH; install the CLI or set an explicit path"
				.into(),
		);
		}
		// `where` prints one match per line; take the first.
		let found = String::from_utf8_lossy(&output.stdout)
			.lines()
			.map(str::trim)
			.find(|line| !line.is_empty())
			.unwrap_or_default()
			.to_string();
		if found.is_empty() {
			return Err("aghub-cli not on PATH".into());
		}
		Ok(PathBuf::from(found))
	}

	pub fn home_dir() -> Result<PathBuf, String> {
		#[cfg(windows)]
		let var = "USERPROFILE";
		#[cfg(not(windows))]
		let var = "HOME";
		std::env::var_os(var)
			.map(PathBuf::from)
			.ok_or_else(|| format!("{var} is not set"))
	}

	/// Run a command, failing with its stderr.
	pub type RunFn<'a> = dyn Fn(&str, &[&str]) -> Result<(), String> + 'a;
	/// Run a command purely for its exit status.
	pub type ProbeFn<'a> = dyn Fn(&str, &[&str]) -> bool + 'a;

	pub struct Env<'a> {
		/// `$HOME` (`%USERPROFILE%` on Windows).
		pub home: PathBuf,
		pub run: &'a RunFn<'a>,
		pub probe: &'a ProbeFn<'a>,
	}

	// --- Linux: systemd --user timer -------------------------------------------

	fn systemd_dir(env: &Env) -> PathBuf {
		env.home.join(".config/systemd/user")
	}

	pub fn register_systemd(
		env: &Env,
		cli: &Path,
		sidecar: &Path,
	) -> Result<(), String> {
		let dir = systemd_dir(env);
		std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
		std::fs::write(
			dir.join(SYSTEMD_SERVICE),
			systemd_service_unit(cli, sidecar)?,
		)
		.map_err(|err| err.to_string())?;
		std::fs::write(dir.join(SYSTEMD_TIMER), systemd_timer_unit())
			.map_err(|err| err.to_string())?;
		(env.run)("systemctl", &["--user", "daemon-reload"])?;
		(env.run)("systemctl", &["--user", "enable", "--now", SYSTEMD_TIMER])
	}

	pub fn unregister_systemd(env: &Env) -> Result<(), String> {
		let _ = (env.run)(
			"systemctl",
			&["--user", "disable", "--now", SYSTEMD_TIMER],
		);
		let dir = systemd_dir(env);
		let _ = std::fs::remove_file(dir.join(SYSTEMD_TIMER));
		let _ = std::fs::remove_file(dir.join(SYSTEMD_SERVICE));
		// `disable` legitimately fails when nothing was registered, so its
		// error is not the verdict — the state AFTERWARDS is. Reporting
		// "disabled" while the OS still runs the job is the failure that
		// matters.
		if systemd_enabled(env) {
			return Err(
				"the systemd timer is still enabled after removing it".into()
			);
		}
		Ok(())
	}

	pub fn systemd_enabled(env: &Env) -> bool {
		(env.probe)("systemctl", &["--user", "is-enabled", SYSTEMD_TIMER])
	}

	// --- macOS: launchd user agent ---------------------------------------------

	fn launchd_plist_path(env: &Env) -> PathBuf {
		env.home
			.join("Library/LaunchAgents")
			.join(format!("{LAUNCHD_LABEL}.plist"))
	}

	pub fn register_launchd(
		env: &Env,
		uid: &str,
		cli: &Path,
		sidecar: &Path,
	) -> Result<(), String> {
		let path = launchd_plist_path(env);
		let plist = launchd_plist(cli, sidecar)?;
		if let Some(parent) = path.parent() {
			std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
		}
		std::fs::write(&path, plist).map_err(|err| err.to_string())?;
		let domain = format!("gui/{uid}");
		// Not loaded yet on a first enable, so a failing bootout is expected.
		let _ = (env.run)(
			"launchctl",
			&["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
		);
		let bootstrapped = (env.run)(
			"launchctl",
			&["bootstrap", &domain, &path.to_string_lossy()],
		);
		if bootstrapped.is_err() {
			// `launchd_enabled` reads the plist's existence, so a plist left
			// behind by a failed bootstrap would report a schedule launchd
			// never loaded.
			let _ = std::fs::remove_file(&path);
		}
		bootstrapped
	}

	pub fn unregister_launchd(env: &Env, uid: &str) -> Result<(), String> {
		let label = format!("gui/{uid}/{LAUNCHD_LABEL}");
		let _ = (env.run)("launchctl", &["bootout", &label]);
		// Ask launchd BEFORE deleting the plist. Deleting it only stops the
		// NEXT login from loading the job; it does not unload one launchd
		// already holds — and `launchd_enabled` reads the plist's existence, so
		// deleting first would report "disabled" while the job keeps running.
		// Leaving the plist in place on failure keeps the UI honest.
		if (env.probe)("launchctl", &["print", &label]) {
			return Err(
				"launchd still has the job loaded after removing it".into()
			);
		}
		let _ = std::fs::remove_file(launchd_plist_path(env));
		Ok(())
	}

	/// The plist is the durable state: launchd reloads it at every login, so a
	/// present file means "scheduled" even before `launchctl` is asked.
	pub fn launchd_enabled(env: &Env) -> bool {
		launchd_plist_path(env).is_file()
	}

	// --- Windows: schtasks daily task ------------------------------------------

	pub fn register_schtasks(
		env: &Env,
		cli: &Path,
		sidecar: &Path,
	) -> Result<(), String> {
		let args = schtasks_create_args(cli, sidecar)?;
		let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
		(env.run)("schtasks", &borrowed)
	}

	pub fn unregister_schtasks(env: &Env) -> Result<(), String> {
		let _ = (env.run)("schtasks", &["/Delete", "/TN", WINDOWS_TASK, "/F"]);
		if schtasks_enabled(env) {
			return Err(
				"the scheduled task still exists after deleting it".into()
			);
		}
		Ok(())
	}

	pub fn schtasks_enabled(env: &Env) -> bool {
		(env.probe)("schtasks", &["/Query", "/TN", WINDOWS_TASK])
	}
}

pub use schedule_backend::*;

// --- the real environment + platform dispatch ------------------------------

fn real_env() -> Result<Env<'static>, String> {
	Ok(Env {
		home: home_dir()?,
		run: &run,
		probe: &succeeds,
	})
}

/// launchd's user domain is addressed as `gui/<uid>`; no env var reliably
/// carries the uid inside an app bundle.
#[cfg(target_os = "macos")]
fn current_uid() -> Result<String, String> {
	let out = quiet("id")
		.arg("-u")
		.output()
		.map_err(|err| format!("id -u failed to start: {err}"))?;
	let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
	if !out.status.success() || uid.is_empty() {
		return Err("could not resolve the current uid".into());
	}
	Ok(uid)
}

fn backend_register(cli: &Path, sidecar: &Path) -> Result<(), String> {
	#[cfg(target_os = "linux")]
	{
		register_systemd(&real_env()?, cli, sidecar)
	}
	#[cfg(target_os = "macos")]
	{
		register_launchd(&real_env()?, &current_uid()?, cli, sidecar)
	}
	#[cfg(target_os = "windows")]
	{
		register_schtasks(&real_env()?, cli, sidecar)
	}
	#[cfg(not(any(
		target_os = "linux",
		target_os = "macos",
		target_os = "windows"
	)))]
	{
		let _ = (cli, sidecar);
		Err("no OS schedule backend on this platform".into())
	}
}

fn backend_unregister() -> Result<(), String> {
	#[cfg(target_os = "linux")]
	{
		unregister_systemd(&real_env()?)
	}
	#[cfg(target_os = "macos")]
	{
		unregister_launchd(&real_env()?, &current_uid()?)
	}
	#[cfg(target_os = "windows")]
	{
		unregister_schtasks(&real_env()?)
	}
	#[cfg(not(any(
		target_os = "linux",
		target_os = "macos",
		target_os = "windows"
	)))]
	{
		Ok(())
	}
}

fn backend_enabled() -> bool {
	let Ok(env) = real_env() else {
		return false;
	};
	#[cfg(target_os = "linux")]
	{
		systemd_enabled(&env)
	}
	#[cfg(target_os = "macos")]
	{
		launchd_enabled(&env)
	}
	#[cfg(target_os = "windows")]
	{
		schtasks_enabled(&env)
	}
	#[cfg(not(any(
		target_os = "linux",
		target_os = "macos",
		target_os = "windows"
	)))]
	{
		let _ = env;
		false
	}
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCheckScheduleStatus {
	/// False on platforms with no registration backend; the UI hides the whole
	/// row rather than showing a switch that cannot work.
	pub supported: bool,
	pub enabled: bool,
	pub cli_path: Option<String>,
	pub sidecar_path: String,
}

// Every command here is `async` + `spawn_blocking`: a SYNC `#[tauri::command]`
// runs on the main (UI) thread, and these shell out to
// `which`/`systemctl`/`launchctl`/`schtasks` and touch the filesystem. A wedged
// OS command in a sync command freezes the webview (worked example and the rule
// itself: `commands/remote.rs`).

fn join_error(e: tauri::Error) -> String {
	format!("skill-check task failed to join: {e}")
}

#[tauri::command]
pub async fn resolve_aghub_cli(
	explicit: Option<String>,
) -> Result<String, String> {
	tauri::async_runtime::spawn_blocking(move || {
		resolve_aghub_cli_path(explicit).map(|path| path.display().to_string())
	})
	.await
	.map_err(join_error)?
}

fn schedule_status() -> SkillCheckScheduleStatus {
	SkillCheckScheduleStatus {
		supported: SCHEDULE_SUPPORTED,
		enabled: backend_enabled(),
		cli_path: resolve_aghub_cli_path(None)
			.ok()
			.map(|p| p.display().to_string()),
		sidecar_path: default_sidecar_path().display().to_string(),
	}
}

#[tauri::command]
pub async fn get_skill_check_schedule(
) -> Result<SkillCheckScheduleStatus, String> {
	tauri::async_runtime::spawn_blocking(schedule_status)
		.await
		.map_err(join_error)
}

#[tauri::command]
pub async fn set_skill_check_schedule(
	enabled: bool,
	cli_path: Option<String>,
) -> Result<SkillCheckScheduleStatus, String> {
	tauri::async_runtime::spawn_blocking(move || {
		if enabled {
			let cli = resolve_aghub_cli_path(cli_path)?;
			let sidecar = default_sidecar_path();
			if let Some(parent) = sidecar.parent() {
				std::fs::create_dir_all(parent)
					.map_err(|err| err.to_string())?;
			}
			backend_register(&cli, &sidecar)?;
		} else {
			backend_unregister()?;
		}
		Ok(schedule_status())
	})
	.await
	.map_err(join_error)?
}

#[tauri::command]
pub async fn get_last_skill_check() -> Result<Option<serde_json::Value>, String>
{
	tauri::async_runtime::spawn_blocking(|| {
		let path = default_sidecar_path();
		if !path.is_file() {
			return Ok(None);
		}
		let body =
			std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
		let value =
			serde_json::from_str(&body).map_err(|err| err.to_string())?;
		Ok(Some(value))
	})
	.await
	.map_err(join_error)?
}

#[cfg(test)]
mod tests {
	use super::*;

	fn cli() -> PathBuf {
		PathBuf::from("/usr/bin/aghub-cli")
	}

	fn sidecar() -> PathBuf {
		PathBuf::from("/tmp/isolated/skill-check-last.json")
	}

	/// Whatever a backend is about to hand the OS: it runs the CLI's check,
	/// never applies, and never launches the desktop bundle.
	fn assert_check_only_payload(payload: &str) {
		assert!(payload.contains("aghub-cli"), "{payload}");
		assert!(payload.contains("check"), "{payload}");
		assert!(payload.contains("--online"), "{payload}");
		assert!(!payload.contains("apply-update"), "{payload}");
		assert!(!payload.contains("--yes"), "{payload}");
		assert!(!payload.contains("--background-task"), "{payload}");
		// The desktop binary must never be the scheduled program.
		assert!(!payload.contains("aghub-desktop"), "{payload}");
		assert!(!payload.contains(".app/Contents"), "{payload}");
	}

	#[test]
	fn schedule_argv_is_check_online_and_never_apply() {
		let argv = build_check_argv(&cli(), &sidecar());
		assert_eq!(argv[0], "/usr/bin/aghub-cli");
		assert!(schedule_is_check_only(&argv));
		assert_check_only_payload(&argv.join(" "));
		assert!(argv.iter().any(|a| a == "-g"));
		assert_eq!(argv[0].rsplit('/').next().unwrap(), "aghub-cli");
	}

	#[test]
	fn schedule_is_check_only_rejects_apply_update() {
		let bad = vec![
			"aghub-cli".into(),
			"apply-update".into(),
			"skills".into(),
			"foo".into(),
			"--yes".into(),
		];
		assert!(!schedule_is_check_only(&bad));
	}

	#[test]
	fn systemd_units_run_the_cli_daily_and_only_check() {
		let unit = systemd_service_unit(&cli(), &sidecar()).unwrap();
		assert_check_only_payload(&unit);
		assert!(unit.contains("ExecStart='/usr/bin/aghub-cli'"), "{unit}");
		let timer = systemd_timer_unit();
		assert!(timer.contains("OnCalendar=*-*-* 09:00:00"), "{timer}");
		assert!(timer.contains("Persistent=true"), "{timer}");
	}

	#[test]
	fn launchd_plist_runs_the_cli_daily_and_only_checks() {
		let plist = launchd_plist(&cli(), &sidecar()).unwrap();
		assert_check_only_payload(&plist);
		assert!(
			plist.contains("<string>com.aghub.skillcheck</string>"),
			"{plist}"
		);
		assert!(
			plist.contains("<string>/usr/bin/aghub-cli</string>"),
			"{plist}"
		);
		assert!(
			plist.contains("<key>StartCalendarInterval</key>"),
			"{plist}"
		);
		assert!(plist.contains("<integer>9</integer>"), "{plist}");
		// RunAtLoad would fire a network check on every login.
		assert!(
			plist.contains("<key>RunAtLoad</key>\n\t<false/>"),
			"{plist}"
		);
	}

	#[test]
	fn launchd_plist_escapes_xml_in_paths() {
		let plist =
			launchd_plist(Path::new("/opt/a&b/aghub-cli"), &sidecar()).unwrap();
		assert!(plist.contains("/opt/a&amp;b/aghub-cli"), "{plist}");
		assert!(!plist.contains("/opt/a&b/"), "{plist}");
	}

	#[test]
	fn schtasks_args_are_a_daily_check_only_task() {
		let args = schtasks_create_args(&cli(), &sidecar()).unwrap();
		assert_check_only_payload(&args.join(" "));
		assert_eq!(args[0], "/Create");
		assert_eq!(args[2], WINDOWS_TASK);
		assert!(args.iter().any(|a| a == "DAILY"), "{args:?}");
		assert!(args.iter().any(|a| a == "09:00"), "{args:?}");
		// The program inside /TR must stay quoted: Program Files has a space.
		let run = &args[4];
		assert!(run.starts_with("\"/usr/bin/aghub-cli\" "), "{run}");
	}

	/// Task Scheduler re-parses `/TR`, so a Windows profile with a space
	/// (`C:\Users\First Last`) must not split into extra argv entries.
	#[test]
	fn schtasks_quotes_every_argument_not_just_the_program() {
		let args = schtasks_create_args(
			Path::new("C:\\Program Files\\aghub\\aghub-cli.exe"),
			Path::new("C:\\Users\\First Last\\AppData\\aghub\\last.json"),
		)
		.unwrap();
		let run = &args[4];
		assert!(
			run.contains(
				"\"C:\\Users\\First Last\\AppData\\aghub\\last.json\""
			),
			"the sidecar path must stay ONE argument: {run}"
		);
		assert!(
			run.starts_with("\"C:\\Program Files\\aghub\\aghub-cli.exe\" "),
			"{run}"
		);
		// Every token is quoted, so re-parsing yields exactly our argv.
		assert_eq!(run.matches('"').count() % 2, 0, "unbalanced quotes: {run}");
	}

	/// Declares `$log` (every OS command, in order) and `$env` (a temp HOME +
	/// a runner that records instead of executing). Lets the macOS and Windows
	/// registration paths be driven on Linux — the only place a wrong
	/// `launchctl` subcommand would otherwise surface is a mac user's machine.
	macro_rules! recording_env {
		($tmp:ident, $log:ident, $env:ident) => {
			let $log: std::cell::RefCell<Vec<String>> =
				std::cell::RefCell::new(Vec::new());
			let run = |program: &str, args: &[&str]| {
				$log.borrow_mut()
					.push(format!("{program} {}", args.join(" ")));
				Ok(())
			};
			// "the OS reports nothing scheduled" — the state after a removal,
			// which `unregister_*` now verifies instead of trusting the
			// removal command's exit code.
			let probe = |_: &str, _: &[&str]| false;
			let $env = Env {
				home: $tmp.path().to_path_buf(),
				run: &run,
				probe: &probe,
			};
		};
	}

	/// The gate judges ARGV. Each mutation below is a command that must never
	/// reach an OS scheduler; the last two are the false positives a substring
	/// test produced.
	#[test]
	fn the_check_only_gate_judges_argv_not_the_joined_string() {
		let ok = build_check_argv(&cli(), &sidecar());
		assert!(schedule_is_check_only(&ok));

		let mutate = |f: &dyn Fn(&mut Vec<String>)| {
			let mut argv = ok.clone();
			f(&mut argv);
			schedule_is_check_only(&argv)
		};
		assert!(!mutate(&|a| a[1] = "apply-update".into()), "mutating verb");
		assert!(!mutate(&|a| a.push("--yes".into())), "--yes");
		assert!(!mutate(&|a| a.push("-y".into())), "-y short form");
		assert!(
			!mutate(&|a| a.push("--background-task".into())),
			"desktop background task"
		);
		assert!(
			!mutate(&|a| a.retain(|x| x != "--online")),
			"an offline schedule is not the contract either"
		);
		assert!(!schedule_is_check_only(&[]), "empty argv");
		assert!(
			!schedule_is_check_only(&["aghub-cli".into()]),
			"program with no verb"
		);

		// A PATH that merely contains the words must NOT be refused: a real
		// data directory can be called anything.
		let awkward = build_check_argv(
			&cli(),
			Path::new("/home/u/apply-update/--yes/last.json"),
		);
		assert!(
			schedule_is_check_only(&awkward),
			"a path is data, not a verb: {awkward:?}"
		);
	}

	/// Turning the schedule OFF must not report success while the OS still has
	/// it. The verdict is the state afterwards, not the removal command's exit.
	#[test]
	fn unregister_reports_failure_when_the_os_still_has_the_job() {
		let tmp = tempfile::tempdir().unwrap();
		let run = |_: &str, _: &[&str]| Ok(());
		// The OS insists it is still scheduled.
		let probe = |_: &str, _: &[&str]| true;
		let env = Env {
			home: tmp.path().to_path_buf(),
			run: &run,
			probe: &probe,
		};
		assert!(unregister_systemd(&env).is_err());
		assert!(unregister_schtasks(&env).is_err());
		assert!(unregister_launchd(&env, "501").is_err());
	}

	#[test]
	fn systemd_register_writes_both_units_then_enables_the_timer() {
		let tmp = tempfile::tempdir().unwrap();
		recording_env!(tmp, log, env);

		register_systemd(&env, &cli(), &sidecar()).unwrap();

		let dir = tmp.path().join(".config/systemd/user");
		let service =
			std::fs::read_to_string(dir.join(SYSTEMD_SERVICE)).unwrap();
		let timer = std::fs::read_to_string(dir.join(SYSTEMD_TIMER)).unwrap();
		assert_check_only_payload(&service);
		assert!(timer.contains("OnCalendar=*-*-* 09:00:00"), "{timer}");
		assert_eq!(
			*log.borrow(),
			vec![
				"systemctl --user daemon-reload".to_string(),
				format!("systemctl --user enable --now {SYSTEMD_TIMER}"),
			]
		);

		log.borrow_mut().clear();
		unregister_systemd(&env).unwrap();
		assert!(!dir.join(SYSTEMD_SERVICE).exists());
		assert!(!dir.join(SYSTEMD_TIMER).exists());
		assert_eq!(
			*log.borrow(),
			vec![format!("systemctl --user disable --now {SYSTEMD_TIMER}")]
		);
	}

	#[test]
	fn launchd_register_writes_the_agent_plist_and_bootstraps_it() {
		let tmp = tempfile::tempdir().unwrap();
		recording_env!(tmp, log, env);

		assert!(!launchd_enabled(&env), "nothing scheduled yet");
		register_launchd(&env, "501", &cli(), &sidecar()).unwrap();

		let plist_path = tmp
			.path()
			.join("Library/LaunchAgents")
			.join(format!("{LAUNCHD_LABEL}.plist"));
		let plist = std::fs::read_to_string(&plist_path).unwrap();
		assert_check_only_payload(&plist);
		assert!(launchd_enabled(&env), "the plist IS the durable state");
		assert_eq!(
			*log.borrow(),
			vec![
				format!("launchctl bootout gui/501/{LAUNCHD_LABEL}"),
				format!("launchctl bootstrap gui/501 {}", plist_path.display()),
			]
		);

		log.borrow_mut().clear();
		unregister_launchd(&env, "501").unwrap();
		assert!(!plist_path.exists());
		assert!(!launchd_enabled(&env));
		assert_eq!(
			*log.borrow(),
			vec![format!("launchctl bootout gui/501/{LAUNCHD_LABEL}")]
		);
	}

	#[test]
	fn schtasks_register_creates_a_daily_task_and_delete_removes_it() {
		let tmp = tempfile::tempdir().unwrap();
		recording_env!(tmp, log, env);

		register_schtasks(&env, &cli(), &sidecar()).unwrap();
		let created = log.borrow()[0].clone();
		assert_check_only_payload(&created);
		assert!(created.starts_with("schtasks /Create /TN aghub-skillcheck"));
		assert!(created.contains("/SC DAILY"), "{created}");
		assert!(created.contains("/ST 09:00"), "{created}");
		assert!(created.ends_with("/F"), "{created}");

		log.borrow_mut().clear();
		unregister_schtasks(&env).unwrap();
		assert_eq!(
			*log.borrow(),
			vec![format!("schtasks /Delete /TN {WINDOWS_TASK} /F")]
		);
	}

	/// Mirror image of the bootstrap case: if launchd still holds the job, the
	/// plist must SURVIVE so `launchd_enabled` keeps reporting the truth.
	#[test]
	fn a_failed_launchd_unload_keeps_the_plist_so_the_ui_stays_honest() {
		let tmp = tempfile::tempdir().unwrap();
		let run = |_: &str, _: &[&str]| Ok(());
		// launchd says the job is still loaded.
		let probe = |_: &str, _: &[&str]| true;
		let env = Env {
			home: tmp.path().to_path_buf(),
			run: &run,
			probe: &probe,
		};
		let plist = tmp
			.path()
			.join("Library/LaunchAgents")
			.join(format!("{LAUNCHD_LABEL}.plist"));
		std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
		std::fs::write(&plist, "<plist/>").unwrap();

		unregister_launchd(&env, "501").unwrap_err();
		assert!(
			plist.exists(),
			"deleting the plist would report 'disabled' while the job runs"
		);
		assert!(launchd_enabled(&env));
	}

	#[test]
	fn a_failed_launchd_bootstrap_leaves_no_plist_claiming_to_be_scheduled() {
		let tmp = tempfile::tempdir().unwrap();
		let run = |_: &str, _: &[&str]| {
			Err("launchctl: Operation not permitted".to_string())
		};
		let probe = |_: &str, _: &[&str]| true;
		let env = Env {
			home: tmp.path().to_path_buf(),
			run: &run,
			probe: &probe,
		};

		register_launchd(&env, "501", &cli(), &sidecar()).unwrap_err();
		assert!(
			!launchd_enabled(&env),
			"a plist left behind would report a schedule launchd never loaded"
		);
	}

	#[test]
	fn an_explicit_cli_path_must_be_absolute() {
		// The scheduler runs from its own working directory, so a relative path
		// that resolves from the desktop app fails every scheduled run.
		let err =
			resolve_aghub_cli_path(Some("bin/aghub-cli".into())).unwrap_err();
		assert!(err.contains("absolute"), "{err}");
	}

	#[test]
	fn a_command_failure_never_echoes_the_paths_we_generated() {
		let tmp = tempfile::tempdir().unwrap();
		let run = |program: &str, args: &[&str]| {
			// Mirrors the real `run`'s message shape.
			let _ = args;
			Err(format!("{program} failed: denied"))
		};
		let probe = |_: &str, _: &[&str]| false;
		let env = Env {
			home: tmp.path().to_path_buf(),
			run: &run,
			probe: &probe,
		};
		let err = register_schtasks(&env, &cli(), &sidecar()).unwrap_err();
		assert!(
			!err.contains("skill-check-last.json"),
			"generated paths must not reach the renderer: {err}"
		);
	}

	/// REAL machine, REAL systemd: registers the timer in this user's own
	/// session, asserts systemd accepted it, then removes it. `#[ignore]` so it
	/// only ever runs when asked (`cargo test -p aghub -- --ignored`) — it
	/// touches `~/.config/systemd/user`, which no CI job should do.
	///
	/// This is the step injection cannot prove: whether the unit we write is
	/// one systemd will actually load.
	#[test]
	#[ignore = "touches the real systemd --user session"]
	#[cfg(target_os = "linux")]
	fn linux_systemd_registration_is_accepted_by_the_real_session() {
		let env = real_env().expect("HOME");
		assert!(
			!systemd_enabled(&env),
			"refusing to run: the timer is already registered"
		);
		let cli = resolve_aghub_cli_path(None).expect("aghub-cli on PATH");
		let sidecar = default_sidecar_path();

		let registered = register_systemd(&env, &cli, &sidecar);
		let enabled = systemd_enabled(&env);
		// Always clean up, even if the assertions below fail.
		let removed = unregister_systemd(&env);

		registered.expect("systemd must accept the unit");
		assert!(enabled, "systemctl is-enabled must see the timer");
		removed.expect("unregister");
		assert!(!systemd_enabled(&env), "the timer must be gone again");
	}

	/// The scheduled CLI writes into `dirs::data_dir()/aghub`. Reading Tauri's
	/// identifier-scoped `app_data_dir()` (or a hand-rolled XDG guess, which
	/// diverges on macOS/Windows) would report "never run" forever.
	#[test]
	fn sidecar_root_is_the_cli_app_data_dir_not_the_tauri_identifier_dir() {
		if std::env::var_os("AGHUB_DATA_DIR").is_some() {
			return; // override in force: both surfaces read the same var
		}
		let path = default_sidecar_path();
		assert!(
			path.ends_with("aghub/skill-check-last.json"),
			"sidecar must sit in the CLI app data root, got {}",
			path.display()
		);
		assert!(
			!path.to_string_lossy().contains("com.akrc.aghub"),
			"the Tauri identifier dir is not where the CLI writes: {}",
			path.display()
		);
	}
}
