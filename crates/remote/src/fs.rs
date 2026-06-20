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
///
/// Output is NUL-delimited records (requires bash on the remote):
///   - First record: `PWD\t<canonical-path>`
///   - Optional second record: `<canonical-path>/..` (omitted at root)
///   - Remaining records: absolute paths of immediate child directories
///
/// Sorting is done in Rust after parsing to avoid relying on `sort -z`,
/// which is GNU-only and not available on BSD/macOS remotes.
pub fn build_remote_list_directories_cmd(path: &str) -> String {
	format!(
		"input={}; \
		if [ -z \"$input\" ] || [ \"$input\" = '~' ]; then dir=\"$HOME\"; \
		elif [ \"${{input#~/}}\" != \"$input\" ]; then \
		dir=\"$HOME/${{input#~/}}\"; \
		else dir=\"$input\"; fi; \
		if [ ! -d \"$dir\" ]; then \
		printf 'not a directory: %s\\n' \"$dir\" >&2; exit 2; fi; \
		cd \"$dir\" || exit 2; \
		pwd_path=$(pwd -P) || exit 2; \
		printf 'PWD\\t%s\\0' \"$pwd_path\"; \
		if [ \"$pwd_path\" != / ]; then \
		printf '%s\\0' \"$pwd_path/..\"; fi; \
		find \"$pwd_path\" -mindepth 1 -maxdepth 1 -type d -print0 \
		2>/dev/null | while IFS= read -r -d '' entry; do \
		printf '%s\\0' \"$entry\"; done",
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

/// Parse the NUL-delimited stdout emitted by
/// [`build_remote_list_directories_cmd`].
///
/// Records are separated by `\0`. The first record has the form
/// `PWD\t<path>`; every subsequent record is an absolute directory path.
/// The parent entry (`..`) is sorted first; remaining entries are sorted
/// by name using byte order (equivalent to `LC_ALL=C sort`).
pub fn parse_remote_directory_listing(
	stdout: &str,
) -> Result<RemoteDirectoryListing, RemoteDirectoryError> {
	let mut path: Option<String> = None;
	let mut parent: Option<RemoteDirectoryEntry> = None;
	let mut entries: Vec<RemoteDirectoryEntry> = Vec::new();

	for record in stdout.split('\0') {
		if record.is_empty() {
			continue;
		}
		if let Some(rest) = record.strip_prefix("PWD\t") {
			if !rest.is_empty() {
				path = Some(rest.to_string());
			}
			continue;
		}
		// Every other record is an absolute directory path.
		let name = record
			.rsplit('/')
			.next()
			.filter(|s| !s.is_empty())
			.ok_or_else(|| RemoteDirectoryError::Parse {
				message: format!(
					"could not derive basename from path: {record}"
				),
			})?
			.to_string();
		let entry = RemoteDirectoryEntry {
			name: name.clone(),
			path: record.to_string(),
		};
		if name == ".." {
			parent = Some(entry);
		} else {
			entries.push(entry);
		}
	}

	let Some(path) = path else {
		return Err(RemoteDirectoryError::Parse {
			message: "remote directory output did not include PWD".to_string(),
		});
	};

	// Sort regular entries by name (byte order = LC_ALL=C sort).
	entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

	// Parent `..` always comes first.
	if let Some(p) = parent {
		entries.insert(0, p);
	}

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
		let input = std::str::from_utf8(
			b"PWD\t/home/alice\0/home/alice/..\0/home/alice/src\0",
		)
		.unwrap();
		let listing = parse_remote_directory_listing(input).unwrap();
		assert_eq!(listing.path, "/home/alice");
		assert_eq!(
			listing.entries,
			vec![
				RemoteDirectoryEntry {
					name: "..".to_string(),
					path: "/home/alice/..".to_string(),
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
		let input = std::str::from_utf8(b"/home/a/src\0").unwrap();
		let err = parse_remote_directory_listing(input).unwrap_err();
		assert!(matches!(err, RemoteDirectoryError::Parse { .. }));
	}

	#[test]
	fn parse_listing_reads_nul_dir_name_with_space() {
		let input =
			std::str::from_utf8(b"PWD\t/home/alice\0/home/alice/My Project\0")
				.unwrap();
		let listing = parse_remote_directory_listing(input).unwrap();
		assert_eq!(listing.entries.len(), 1);
		assert_eq!(listing.entries[0].name, "My Project");
	}

	#[test]
	fn parse_listing_reads_nul_dir_name_with_tab() {
		let input =
			std::str::from_utf8(b"PWD\t/home/alice\0/home/alice/tab\tname\0")
				.unwrap();
		let listing = parse_remote_directory_listing(input).unwrap();
		assert_eq!(listing.entries.len(), 1);
		assert_eq!(listing.entries[0].name, "tab\tname");
	}

	#[test]
	fn parse_listing_reads_nul_dir_name_with_trailing_newline() {
		let input =
			std::str::from_utf8(b"PWD\t/home/alice\0/home/alice/line\n\0")
				.unwrap();
		let listing = parse_remote_directory_listing(input).unwrap();
		assert_eq!(listing.entries.len(), 1);
		assert_eq!(listing.entries[0].name, "line\n");
	}

	#[test]
	fn parse_listing_reads_nul_unicode_dir_name() {
		let input = std::str::from_utf8(
			b"PWD\t/home/alice\0/home/alice/\xe7\x9b\xae\xe5\xbd\x95\0",
		)
		.unwrap();
		let listing = parse_remote_directory_listing(input).unwrap();
		assert_eq!(listing.entries.len(), 1);
		assert_eq!(listing.entries[0].name, "目录");
	}

	#[test]
	fn parse_listing_accepts_empty_nul_listing_with_pwd() {
		let input = std::str::from_utf8(b"PWD\t/home/alice\0").unwrap();
		let listing = parse_remote_directory_listing(input).unwrap();
		assert!(listing.entries.is_empty());
		assert_eq!(listing.path, "/home/alice");
	}

	#[test]
	fn parse_listing_sorts_entries_parent_first() {
		// zebra before apple in raw input; expect apple before zebra after
		// sort, with .. always first.
		let input = std::str::from_utf8(
			b"PWD\t/home/alice\0\
			  /home/alice/..\0\
			  /home/alice/zebra\0\
			  /home/alice/apple\0",
		)
		.unwrap();
		let listing = parse_remote_directory_listing(input).unwrap();
		assert_eq!(listing.entries.len(), 3);
		assert_eq!(listing.entries[0].name, "..");
		assert_eq!(listing.entries[1].name, "apple");
		assert_eq!(listing.entries[2].name, "zebra");
	}

	#[test]
	fn list_remote_directories_runs_ssh_and_parses_output() {
		let remote_cmd = build_remote_list_directories_cmd("~");
		let args = build_ssh_args(&conn(), &remote_cmd);
		let stdout =
			std::str::from_utf8(b"PWD\t/home/alice\0/home/alice/src\0")
				.unwrap()
				.to_string();
		let runner = MockRunner::new().script(
			"ssh",
			&args_as_str(&args),
			CommandOutput {
				status_code: Some(0),
				stdout,
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
