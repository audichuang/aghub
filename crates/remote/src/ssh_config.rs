//! Local OpenSSH config discovery helpers.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MAX_INCLUDE_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigHost {
	pub alias: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub host_name: Option<String>,
}

#[derive(Default)]
struct HostBlock {
	aliases: Vec<String>,
	host_name: Option<String>,
}

pub fn read_default_ssh_config_hosts() -> Vec<SshConfigHost> {
	let Some(home) = dirs::home_dir() else {
		return Vec::new();
	};
	read_ssh_config_hosts(&home.join(".ssh").join("config"))
}

pub fn parse_ssh_config_hosts(config: &str) -> Vec<SshConfigHost> {
	let mut hosts = Vec::new();
	let mut seen = HashSet::new();
	parse_config_content(config, &mut hosts, &mut seen, |_pattern, _, _| {});
	hosts
}

pub fn read_ssh_config_hosts(path: &Path) -> Vec<SshConfigHost> {
	let home = dirs::home_dir().unwrap_or_else(|| {
		path.parent()
			.map(Path::to_path_buf)
			.unwrap_or_else(|| PathBuf::from("."))
	});
	let mut hosts = Vec::new();
	let mut seen = HashSet::new();
	let mut visited = HashSet::new();
	read_config_file(path, &home, &mut hosts, &mut seen, &mut visited, 0);
	hosts
}

fn read_config_file(
	path: &Path,
	home: &Path,
	hosts: &mut Vec<SshConfigHost>,
	seen: &mut HashSet<String>,
	visited: &mut HashSet<PathBuf>,
	depth: usize,
) {
	if depth > MAX_INCLUDE_DEPTH {
		return;
	}
	let identity =
		fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
	if !visited.insert(identity) {
		return;
	}
	let Ok(config) = fs::read_to_string(path) else {
		return;
	};
	let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
	parse_config_content(&config, hosts, seen, |pattern, hosts, seen| {
		for include_path in expand_include_paths(pattern, base_dir, home) {
			read_config_file(
				&include_path,
				home,
				hosts,
				seen,
				visited,
				depth + 1,
			);
		}
	});
}

fn parse_config_content<F>(
	config: &str,
	hosts: &mut Vec<SshConfigHost>,
	seen: &mut HashSet<String>,
	mut include: F,
) where
	F: FnMut(&str, &mut Vec<SshConfigHost>, &mut HashSet<String>),
{
	let mut current = HostBlock::default();
	for raw in config.lines() {
		let line = strip_comment(raw).trim();
		if line.is_empty() {
			continue;
		}
		let mut parts = line.split_whitespace();
		let Some(keyword) = parts.next() else {
			continue;
		};
		match keyword.to_ascii_lowercase().as_str() {
			"host" => {
				flush_host_block(&mut current, hosts, seen);
				current.aliases = parts
					.filter(|alias| is_selectable_alias(alias))
					.map(clean_token)
					.collect();
				current.host_name = None;
			}
			"match" => {
				flush_host_block(&mut current, hosts, seen);
			}
			"hostname" if current.host_name.is_none() => {
				current.host_name = parts.next().map(clean_token);
			}
			"include" => {
				flush_host_block(&mut current, hosts, seen);
				for pattern in parts {
					include(pattern, hosts, seen);
				}
			}
			_ => {}
		}
	}
	flush_host_block(&mut current, hosts, seen);
}

fn flush_host_block(
	block: &mut HostBlock,
	hosts: &mut Vec<SshConfigHost>,
	seen: &mut HashSet<String>,
) {
	for alias in block.aliases.drain(..) {
		if seen.insert(alias.clone()) {
			hosts.push(SshConfigHost {
				alias,
				host_name: block.host_name.clone(),
			});
		}
	}
	block.host_name = None;
}

fn strip_comment(line: &str) -> &str {
	match line.find('#') {
		Some(index) => &line[..index],
		None => line,
	}
}

fn is_selectable_alias(alias: &str) -> bool {
	!alias.starts_with('!')
		&& !alias.contains('*')
		&& !alias.contains('?')
		&& !alias.is_empty()
}

fn clean_token(token: &str) -> String {
	token.trim_matches('"').to_string()
}

fn expand_include_paths(
	pattern: &str,
	base_dir: &Path,
	home: &Path,
) -> Vec<PathBuf> {
	let path = expand_home(pattern, home);
	let path = if path.is_absolute() {
		path
	} else {
		base_dir.join(path)
	};
	let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
		return vec![path];
	};
	if !file_name.contains('*') && !file_name.contains('?') {
		return vec![path];
	}
	let Some(parent) = path.parent() else {
		return Vec::new();
	};
	let Ok(entries) = fs::read_dir(parent) else {
		return Vec::new();
	};
	let mut matches: Vec<PathBuf> = entries
		.flatten()
		.filter_map(|entry| {
			let name = entry.file_name();
			let name = name.to_str()?;
			wildcard_match(file_name, name).then(|| entry.path())
		})
		.collect();
	matches.sort();
	matches
}

fn expand_home(pattern: &str, home: &Path) -> PathBuf {
	if pattern == "~" {
		return home.to_path_buf();
	}
	if let Some(rest) = pattern.strip_prefix("~/") {
		return home.join(rest);
	}
	PathBuf::from(pattern)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
	wildcard_match_inner(pattern.as_bytes(), value.as_bytes(), 0, 0, None, None)
}

fn wildcard_match_inner(
	pattern: &[u8],
	value: &[u8],
	mut pi: usize,
	mut vi: usize,
	mut star: Option<usize>,
	mut match_after_star: Option<usize>,
) -> bool {
	while vi < value.len() {
		if pi < pattern.len()
			&& (pattern[pi] == b'?' || pattern[pi] == value[vi])
		{
			pi += 1;
			vi += 1;
		} else if pi < pattern.len() && pattern[pi] == b'*' {
			star = Some(pi);
			match_after_star = Some(vi);
			pi += 1;
		} else if let (Some(star_index), Some(next_match)) =
			(star, match_after_star)
		{
			pi = star_index + 1;
			vi = next_match + 1;
			match_after_star = Some(vi);
		} else {
			return false;
		}
	}
	while pi < pattern.len() && pattern[pi] == b'*' {
		pi += 1;
	}
	pi == pattern.len()
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	#[test]
	fn parses_plain_host_aliases_with_display_hostname() {
		let hosts = parse_ssh_config_hosts(
			r#"
Host vm-dev
	HostName 10.0.0.5
	User ubuntu

Host staging prod
	HostName app.example.com
"#,
		);

		assert_eq!(
			hosts,
			vec![
				SshConfigHost {
					alias: "vm-dev".to_string(),
					host_name: Some("10.0.0.5".to_string()),
				},
				SshConfigHost {
					alias: "staging".to_string(),
					host_name: Some("app.example.com".to_string()),
				},
				SshConfigHost {
					alias: "prod".to_string(),
					host_name: Some("app.example.com".to_string()),
				},
			]
		);
	}

	#[test]
	fn ignores_wildcards_negations_and_duplicates() {
		let hosts = parse_ssh_config_hosts(
			r#"
Host *
	User ubuntu

Host !blocked vm-* ?one good-host
	HostName ignored.example.com

Host good-host
	HostName first.example.com
"#,
		);

		assert_eq!(
			hosts,
			vec![SshConfigHost {
				alias: "good-host".to_string(),
				host_name: Some("ignored.example.com".to_string()),
			}]
		);
	}

	#[test]
	fn read_hosts_follows_literal_include_relative_to_config_dir() {
		let dir = tempfile::tempdir().unwrap();
		let config = dir.path().join("config");
		let included = dir.path().join("remotes.conf");
		fs::write(
			&config,
			r#"
Host root
	HostName root.example.com

Include remotes.conf
"#,
		)
		.unwrap();
		fs::write(
			&included,
			r#"
Host included
	HostName included.example.com
"#,
		)
		.unwrap();

		assert_eq!(
			read_ssh_config_hosts(&config),
			vec![
				SshConfigHost {
					alias: "root".to_string(),
					host_name: Some("root.example.com".to_string()),
				},
				SshConfigHost {
					alias: "included".to_string(),
					host_name: Some("included.example.com".to_string()),
				},
			]
		);
	}
}
