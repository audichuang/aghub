//! Remote filesystem helpers used by the desktop VM project picker.
//!
//! The UI needs to browse directories on a remote SSH target. Keep the SSH
//! command composition and output parsing in this tauri-free crate so it can be
//! unit-tested with [`crate::test_support::MockRunner`].

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ssh::{
	build_ssh_args, shell_quote_single, CommandOutput, CommandRunner,
	Connection,
};

/// A selectable remote directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDirectoryEntry {
	pub name: String,
	pub path: String,
}

/// Directories under a remote path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDirectoryListing {
	pub path: String,
	pub entries: Vec<RemoteDirectoryEntry>,
}

/// Failure while listing a remote directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteDirectoryError {
	Unreachable { stderr: String },
	NotDirectory { message: String },
	CommandFailed { message: String },
	Parse { message: String },
}

impl fmt::Display for RemoteDirectoryError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			RemoteDirectoryError::Unreachable { stderr } => {
				write!(f, "remote is unreachable: {stderr}")
			}
			RemoteDirectoryError::NotDirectory { message } => {
				write!(f, "{message}")
			}
			RemoteDirectoryError::CommandFailed { message } => {
				write!(f, "{message}")
			}
			RemoteDirectoryError::Parse { message } => write!(f, "{message}"),
		}
	}
}

impl std::error::Error for RemoteDirectoryError {}

/// Compose a remote bash command that lists immediate child directories.
pub fn build_remote_list_directories_cmd(path: &str) -> String {
	format!(
		"input={}; \
	     if [ -z \"$input\" ] || [ \"$input\" = '~' ]; then \
	         dir=\"$HOME\"; \
	     elif [ \"${{input#~/}}\" != \"$input\" ]; then \
	         dir=\"$HOME/${{input#~/}}\"; \
	     else \
	         dir=\"$input\"; \
	     fi; \
	     if [ ! -d \"$dir\" ]; then \
	         printf 'not a directory: %s\\n' \"$dir\" >&2; exit 2; \
	     fi; \
	     cd \"$dir\" || exit 2; \
	     pwd_path=$(pwd -P) || exit 2; \
	     printf 'PWD\\t%s\\n' \"$pwd_path\"; \
	     parent=$(dirname -- \"$pwd_path\"); \
	     if [ \"$parent\" != \"$pwd_path\" ]; then \
	         printf 'DIR\\t..\\t%s\\n' \"$parent\"; \
	     fi; \
	     find \"$pwd_path\" -mindepth 1 -maxdepth 1 -type d -print \
	         2>/dev/null | LC_ALL=C sort | while IFS= read -r entry; do \
	         name=${{entry##*/}}; \
	         printf 'DIR\\t%s\\t%s\\n' \"$name\" \"$entry\"; \
	     done",
		shell_quote_single(path)
	)
}

/// List a remote directory over SSH.
pub fn list_remote_directories<R: CommandRunner>(
	runner: &R,
	conn: &Connection,
	path: &str,
) -> Result<RemoteDirectoryListing, RemoteDirectoryError> {
	let remote_cmd = build_remote_list_directories_cmd(path);
	let args = build_ssh_args(conn, &remote_cmd);
	let out = runner.run("ssh", &args).map_err(|e| {
		RemoteDirectoryError::CommandFailed {
			message: e.to_string(),
		}
	})?;

	if is_transport_failure(out.status_code) {
		return Err(RemoteDirectoryError::Unreachable { stderr: out.stderr });
	}
	if out.status_code == Some(2) {
		return Err(RemoteDirectoryError::NotDirectory {
			message: nonzero_message("remote directory list", &out),
		});
	}
	if out.status_code != Some(0) {
		return Err(RemoteDirectoryError::CommandFailed {
			message: nonzero_message("remote directory list", &out),
		});
	}

	parse_remote_directory_listing(&out.stdout)
}

/// Parse the tab-delimited stdout emitted by [`build_remote_list_directories_cmd`].
pub fn parse_remote_directory_listing(
	stdout: &str,
) -> Result<RemoteDirectoryListing, RemoteDirectoryError> {
	let mut path = None;
	let mut entries = Vec::new();

	for line in stdout.lines() {
		if let Some(rest) = line.strip_prefix("PWD\t") {
			if !rest.is_empty() {
				path = Some(rest.to_string());
			}
			continue;
		}

		if let Some(rest) = line.strip_prefix("DIR\t") {
			let mut parts = rest.splitn(2, '\t');
			let name = parts.next().unwrap_or_default();
			let entry_path = parts.next().unwrap_or_default();
			if name.is_empty() || entry_path.is_empty() {
				return Err(RemoteDirectoryError::Parse {
					message: format!("invalid directory row: {line}"),
				});
			}
			entries.push(RemoteDirectoryEntry {
				name: name.to_string(),
				path: entry_path.to_string(),
			});
		}
	}

	let Some(path) = path else {
		return Err(RemoteDirectoryError::Parse {
			message: "remote directory output did not include PWD".to_string(),
		});
	};

	Ok(RemoteDirectoryListing { path, entries })
}

fn is_transport_failure(status_code: Option<i32>) -> bool {
	matches!(status_code, Some(255) | None)
}

fn nonzero_message(step: &str, out: &CommandOutput) -> String {
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_support::MockRunner;

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

	fn args_as_str(args: &[String]) -> Vec<&str> {
		args.iter().map(|s| s.as_str()).collect()
	}

	#[test]
	fn list_cmd_expands_empty_path_to_home() {
		let cmd = build_remote_list_directories_cmd("");
		assert!(cmd.contains("input=''"));
		assert!(cmd.contains("dir=\"$HOME\""));
		assert!(cmd.contains("find \"$pwd_path\" -mindepth 1"));
	}

	#[test]
	fn list_cmd_quotes_hostile_path() {
		let cmd = build_remote_list_directories_cmd("~/x' ; rm -rf /");
		assert!(cmd.contains("input='~/x'\\'' ; rm -rf /'"));
	}

	#[test]
	fn parse_listing_reads_pwd_and_dirs() {
		let listing = parse_remote_directory_listing(
			"PWD\t/home/alice\nDIR\t..\t/home\nDIR\tsrc\t/home/alice/src\n",
		)
		.unwrap();
		assert_eq!(listing.path, "/home/alice");
		assert_eq!(
			listing.entries,
			vec![
				RemoteDirectoryEntry {
					name: "..".to_string(),
					path: "/home".to_string(),
				},
				RemoteDirectoryEntry {
					name: "src".to_string(),
					path: "/home/alice/src".to_string(),
				},
			]
		);
	}

	#[test]
	fn parse_listing_rejects_missing_pwd() {
		let err = parse_remote_directory_listing("DIR\tsrc\t/home/a/src\n")
			.unwrap_err();
		assert!(matches!(err, RemoteDirectoryError::Parse { .. }));
	}

	#[test]
	fn list_remote_directories_runs_ssh_and_parses_output() {
		let remote_cmd = build_remote_list_directories_cmd("~");
		let args = build_ssh_args(&conn(), &remote_cmd);
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: Some(0),
				stdout: "PWD\t/home/alice\nDIR\tsrc\t/home/alice/src\n"
					.to_string(),
				stderr: String::new(),
			},
		);

		let listing = list_remote_directories(&runner, &conn(), "~").unwrap();

		assert_eq!(listing.path, "/home/alice");
		assert_eq!(listing.entries[0].path, "/home/alice/src");
		assert_eq!(runner.calls()[0].program, "ssh");
	}

	#[test]
	fn list_remote_directories_reports_not_directory() {
		let remote_cmd = build_remote_list_directories_cmd("/nope");
		let args = build_ssh_args(&conn(), &remote_cmd);
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: Some(2),
				stdout: String::new(),
				stderr: "not a directory: /nope".to_string(),
			},
		);

		let err =
			list_remote_directories(&runner, &conn(), "/nope").unwrap_err();
		assert!(matches!(err, RemoteDirectoryError::NotDirectory { .. }));
	}
}
