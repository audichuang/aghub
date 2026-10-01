#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
pub(super) fn test_has_git() -> bool {
	std::process::Command::new("git")
		.arg("--version")
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.status()
		.map(|status| status.success())
		.unwrap_or(false)
}

#[cfg(unix)]
pub(super) fn test_git(root: &Path, args: &[&str]) {
	let ok = std::process::Command::new("git")
		.arg("-C")
		.arg(root)
		.args(args)
		.env("GIT_CONFIG_GLOBAL", "/dev/null")
		.env("GIT_CONFIG_NOSYSTEM", "1")
		.env("HOME", root)
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.status()
		.expect("failed to spawn git")
		.success();
	assert!(ok, "git -C {} {} failed", root.display(), args.join(" "));
}
