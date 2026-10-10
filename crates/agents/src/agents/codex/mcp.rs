use crate::descriptor::*;
use std::path::{Path, PathBuf};

pub(super) fn global_path() -> Option<PathBuf> {
	global_dir().map(|root| root.join("config.toml"))
}

pub(super) fn global_dir() -> Option<PathBuf> {
	resolve_global_dir(
		std::env::var_os("CODEX_HOME")
			.filter(|value| !value.is_empty())
			.map(PathBuf::from),
		home_dir(),
	)
}

fn resolve_global_dir(
	codex_home: Option<PathBuf>,
	home: Option<PathBuf>,
) -> Option<PathBuf> {
	codex_home.or_else(|| home.map(|home| home.join(".codex")))
}

pub(super) fn project_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".codex/config.toml"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn global_path_prefers_codex_home() {
		assert_eq!(
			resolve_global_dir(
				Some(PathBuf::from("/custom/codex")),
				Some(PathBuf::from("/home/user")),
			),
			Some(PathBuf::from("/custom/codex"))
		);
	}

	#[test]
	fn global_path_defaults_under_home() {
		assert_eq!(
			resolve_global_dir(None, Some(PathBuf::from("/home/user"))),
			Some(PathBuf::from("/home/user/.codex"))
		);
	}
}
