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
// Unit FILE names: only the Linux backend writes them. The unit CONTENT
// builders below stay platform-independent so their tests run everywhere.
#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE: &str = "aghub-skillcheck.service";
#[cfg(target_os = "linux")]
const SYSTEMD_TIMER: &str = "aghub-skillcheck.timer";
#[allow(dead_code)] // one backend per platform; the rest are tested, not called
const LAUNCHD_LABEL: &str = "com.aghub.skillcheck";
#[allow(dead_code)] // one backend per platform; the rest are tested, not called
const WINDOWS_TASK: &str = "aghub-skillcheck";

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

pub fn schedule_is_check_only(argv: &[String]) -> bool {
	let hay = argv.join(" ");
	hay.contains("check")
		&& hay.contains("--online")
		&& !hay.contains("apply-update")
		&& !argv.iter().any(|a| a == "--yes")
		&& !hay.contains("--background-task")
}

/// The one gate every backend goes through before writing a task definition.
fn checked_argv(cli: &Path, sidecar: &Path) -> Result<Vec<String>, String> {
	let argv = build_check_argv(cli, sidecar);
	if !schedule_is_check_only(&argv) {
		return Err("refusing to schedule a non-check command".into());
	}
	Ok(argv)
}

/// The sidecar the SCHEDULED CLI writes, so the root must be the CLI's
/// (`aghub-cli`'s `commands::app_data_dir`): `$AGHUB_DATA_DIR`, else
/// `dirs::data_dir()/aghub`. Deliberately NOT Tauri's `app_data_dir()`, which
/// is identifier-scoped (`<data>/com.akrc.aghub`) and would read a file the CLI
/// never writes.
pub fn default_sidecar_path() -> PathBuf {
	if let Some(dir) = std::env::var_os("AGHUB_DATA_DIR") {
		return PathBuf::from(dir).join(SIDECAR_NAME);
	}
	aghub_api::default_app_data_dir().join(SIDECAR_NAME)
}

// ---------------------------------------------------------------------------
// Task payloads (pure; compiled and tested on every platform)
// ---------------------------------------------------------------------------

fn shell_single_quote(value: &str) -> String {
	format!("'{}'", value.replace('\'', "'\\''"))
}

#[allow(dead_code)] // one backend per platform; the rest are tested, not called
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

#[allow(dead_code)] // one backend per platform; the rest are tested, not called
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
#[allow(dead_code)] // one backend per platform; the rest are tested, not called
pub fn schtasks_create_args(
	cli: &Path,
	sidecar: &Path,
) -> Result<Vec<String>, String> {
	let argv = checked_argv(cli, sidecar)?;
	let (program, rest) = argv.split_first().expect("argv[0] is the program");
	let run = format!("\"{}\" {}", program, rest.join(" "));
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
fn quiet(program: &str) -> Command {
	#[allow(unused_mut)]
	let mut cmd = Command::new(program);
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		cmd.creation_flags(crate::CREATE_NO_WINDOW);
	}
	cmd
}

#[allow(dead_code)] // used by the platform backends only
fn run(program: &str, args: &[&str]) -> Result<(), String> {
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
	Err(format!("{program} {} failed: {detail}", args.join(" ")))
}

#[allow(dead_code)] // used by the platform backends only
fn succeeds(program: &str, args: &[&str]) -> bool {
	quiet(program)
		.args(args)
		.output()
		.map(|out| out.status.success())
		.unwrap_or(false)
}

fn resolve_aghub_cli_path(explicit: Option<String>) -> Result<PathBuf, String> {
	if let Some(path) = explicit {
		let path = PathBuf::from(path);
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

#[allow(dead_code)] // used by the platform backends only
fn home_dir() -> Result<PathBuf, String> {
	#[cfg(windows)]
	let var = "USERPROFILE";
	#[cfg(not(windows))]
	let var = "HOME";
	std::env::var_os(var)
		.map(PathBuf::from)
		.ok_or_else(|| format!("{var} is not set"))
}

// ---------------------------------------------------------------------------
// Linux — systemd --user timer
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod backend {
	use super::*;

	fn unit_dir() -> Result<PathBuf, String> {
		Ok(home_dir()?.join(".config/systemd/user"))
	}

	pub fn register(cli: &Path, sidecar: &Path) -> Result<(), String> {
		let dir = unit_dir()?;
		std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
		std::fs::write(
			dir.join(SYSTEMD_SERVICE),
			systemd_service_unit(cli, sidecar)?,
		)
		.map_err(|err| err.to_string())?;
		std::fs::write(dir.join(SYSTEMD_TIMER), systemd_timer_unit())
			.map_err(|err| err.to_string())?;
		run("systemctl", &["--user", "daemon-reload"])?;
		run("systemctl", &["--user", "enable", "--now", SYSTEMD_TIMER])
	}

	pub fn unregister() -> Result<(), String> {
		let _ =
			run("systemctl", &["--user", "disable", "--now", SYSTEMD_TIMER]);
		if let Ok(dir) = unit_dir() {
			let _ = std::fs::remove_file(dir.join(SYSTEMD_TIMER));
			let _ = std::fs::remove_file(dir.join(SYSTEMD_SERVICE));
		}
		Ok(())
	}

	pub fn is_enabled() -> bool {
		succeeds("systemctl", &["--user", "is-enabled", SYSTEMD_TIMER])
	}
}

// ---------------------------------------------------------------------------
// macOS — launchd user agent
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod backend {
	use super::*;

	fn plist_path() -> Result<PathBuf, String> {
		Ok(home_dir()?
			.join("Library/LaunchAgents")
			.join(format!("{LAUNCHD_LABEL}.plist")))
	}

	/// launchd's user domain is addressed as `gui/<uid>`; no env var reliably
	/// carries the uid inside an app bundle.
	fn gui_domain() -> Result<String, String> {
		let out = quiet("id")
			.arg("-u")
			.output()
			.map_err(|err| format!("id -u failed to start: {err}"))?;
		let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
		if !out.status.success() || uid.is_empty() {
			return Err("could not resolve the current uid".into());
		}
		Ok(format!("gui/{uid}"))
	}

	pub fn register(cli: &Path, sidecar: &Path) -> Result<(), String> {
		let path = plist_path()?;
		let plist = launchd_plist(cli, sidecar)?;
		if let Some(parent) = path.parent() {
			std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
		}
		std::fs::write(&path, plist).map_err(|err| err.to_string())?;
		let domain = gui_domain()?;
		// Not loaded yet on a first enable, so a failing bootout is expected.
		let _ = run(
			"launchctl",
			&["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
		);
		run(
			"launchctl",
			&["bootstrap", &domain, &path.to_string_lossy()],
		)
	}

	pub fn unregister() -> Result<(), String> {
		if let Ok(domain) = gui_domain() {
			let _ = run(
				"launchctl",
				&["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
			);
		}
		if let Ok(path) = plist_path() {
			let _ = std::fs::remove_file(path);
		}
		Ok(())
	}

	/// The plist is the durable state: launchd reloads it at every login, so a
	/// present file means "scheduled" even before `launchctl` is asked.
	pub fn is_enabled() -> bool {
		plist_path().map(|path| path.is_file()).unwrap_or(false)
	}
}

// ---------------------------------------------------------------------------
// Windows — schtasks daily task
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod backend {
	use super::*;

	pub fn register(cli: &Path, sidecar: &Path) -> Result<(), String> {
		let args = schtasks_create_args(cli, sidecar)?;
		let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
		run("schtasks", &borrowed)
	}

	pub fn unregister() -> Result<(), String> {
		let _ = run("schtasks", &["/Delete", "/TN", WINDOWS_TASK, "/F"]);
		Ok(())
	}

	pub fn is_enabled() -> bool {
		succeeds("schtasks", &["/Query", "/TN", WINDOWS_TASK])
	}
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "macos",
	target_os = "windows"
)))]
mod backend {
	use super::*;

	pub fn register(_cli: &Path, _sidecar: &Path) -> Result<(), String> {
		Err("no OS schedule backend on this platform".into())
	}

	pub fn unregister() -> Result<(), String> {
		Ok(())
	}

	pub fn is_enabled() -> bool {
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

#[tauri::command]
pub fn resolve_aghub_cli(explicit: Option<String>) -> Result<String, String> {
	resolve_aghub_cli_path(explicit).map(|path| path.display().to_string())
}

#[tauri::command]
pub fn get_skill_check_schedule() -> SkillCheckScheduleStatus {
	SkillCheckScheduleStatus {
		supported: SCHEDULE_SUPPORTED,
		enabled: backend::is_enabled(),
		cli_path: resolve_aghub_cli_path(None)
			.ok()
			.map(|p| p.display().to_string()),
		sidecar_path: default_sidecar_path().display().to_string(),
	}
}

#[tauri::command]
pub fn set_skill_check_schedule(
	enabled: bool,
	cli_path: Option<String>,
) -> Result<SkillCheckScheduleStatus, String> {
	if enabled {
		let cli = resolve_aghub_cli_path(cli_path)?;
		let sidecar = default_sidecar_path();
		if let Some(parent) = sidecar.parent() {
			std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
		}
		backend::register(&cli, &sidecar)?;
	} else {
		backend::unregister()?;
	}
	Ok(get_skill_check_schedule())
}

#[tauri::command]
pub fn get_last_skill_check() -> Result<Option<serde_json::Value>, String> {
	let path = default_sidecar_path();
	if !path.is_file() {
		return Ok(None);
	}
	let body = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
	let value = serde_json::from_str(&body).map_err(|err| err.to_string())?;
	Ok(Some(value))
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

	#[test]
	fn every_backend_payload_refuses_a_non_check_command() {
		// The guard lives in `checked_argv`, so proving it once per builder is
		// what stops a future edit from scheduling `apply-update`. The args
		// themselves are hardcoded, so the only way to trip the guard from a
		// test is a path that carries the forbidden word — contrived, but it
		// proves all three builders really route through the gate.
		let cli = Path::new("/usr/bin/aghub-cli");
		let evil = Path::new("/tmp/apply-update/skill-check-last.json");
		for payload in [
			systemd_service_unit(cli, evil).map(|_| ()),
			launchd_plist(cli, evil).map(|_| ()),
			schtasks_create_args(cli, evil).map(|_| ()),
		] {
			let err = payload.expect_err("a non-check payload must be refused");
			assert!(err.contains("non-check"), "{err}");
		}
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
