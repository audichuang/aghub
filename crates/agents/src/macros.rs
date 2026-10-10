//! Macros for generating agent path helper functions.
//!
//! These macros reduce boilerplate in agent descriptor files by generating
//! common path functions.

// ── MCP Dialect Macro ────────────────────────────────────────────────────────

/// Declare an agent's map-based MCP dialect ONCE and generate the matching
/// `parse_mcp_config` / `serialize_mcp_config` pair from it.
///
/// Reading and writing must come from the same [`json_map::Dialect`], or an
/// agent can parse a transport/toggle it cannot write back and the next save
/// silently rewrites the user's config; this macro makes that unrepresentable.
///
/// ```rust,ignore
/// json_map_dialect!(json_map::Dialect {
///     toggle_key: json_map::ToggleKey::Disabled("disabled"),
///     ..json_map::MCP_SERVERS
/// });
/// ```
#[macro_export]
macro_rules! json_map_dialect {
	($dialect:expr) => {
		const MCP_DIALECT: $crate::format::json_map::Dialect = $dialect;

		fn parse_mcp_config(
			content: &str,
		) -> $crate::Result<$crate::AgentConfig> {
			$crate::format::json_map::parse(content, &MCP_DIALECT)
		}

		fn serialize_mcp_config(
			config: &$crate::AgentConfig,
			original: Option<&str>,
		) -> $crate::Result<String> {
			$crate::format::json_map::serialize(config, original, &MCP_DIALECT)
		}
	};
}

// ── MCP Path Macros ──────────────────────────────────────────────────────────

/// Macro to generate MCP path helper functions for an agent descriptor.
///
/// Generates the following functions:
/// - `mcp_global_path()` - returns global MCP config path
/// - `mcp_project_path(root)` - returns project MCP config path
/// - `global_data_dir()` - returns global data directory (parent of mcp_global)
///
/// # Symmetric variant (same base path for global and project)
/// ```rust,ignore
/// define_mcp_paths! {
///     symmetric: ".claude/settings.json",
/// }
/// ```
///
/// # Asymmetric variant (different paths for global and project)
/// ```rust,ignore
/// define_mcp_paths! {
///     global: ".codeium/windsurf/mcp_config.json",
///     project: ".windsurf/mcp_config.json",
///     data_dir: ".codeium/windsurf",
/// }
/// ```
#[macro_export]
macro_rules! define_mcp_paths {
	// Symmetric variant - same path relative to home and project
	(
		symmetric: $path:literal,
	) => {
		fn mcp_global_path() -> Option<std::path::PathBuf> {
			$crate::descriptor::home_dir().map(|home| home.join($path))
		}
		fn mcp_project_path(
			root: &std::path::Path,
		) -> Option<std::path::PathBuf> {
			Some(root.join($path))
		}
		fn global_data_dir() -> Option<std::path::PathBuf> {
			$crate::descriptor::home_dir().and_then(|home| {
				home.join($path).parent().map(|p| p.to_path_buf())
			})
		}
	};

	// Asymmetric variant - different paths for global and project
	(
		global: $global_path:literal,
		project: $project_path:literal,
		data_dir: $data_dir:literal,
	) => {
		fn mcp_global_path() -> Option<std::path::PathBuf> {
			$crate::descriptor::home_dir().map(|home| home.join($global_path))
		}
		fn mcp_project_path(
			root: &std::path::Path,
		) -> Option<std::path::PathBuf> {
			Some(root.join($project_path))
		}
		fn global_data_dir() -> Option<std::path::PathBuf> {
			$crate::descriptor::home_dir().map(|home| home.join($data_dir))
		}
	};
}

// ── Skill Path Macros ──────────────────────────────────────────────────────────

/// Macro to generate skill path helper functions for an agent descriptor.
///
/// Generates `global_skill_write_path()` and `project_skill_write_path(root)` —
/// the write slot per scope; an agent with no extra read dirs declares
/// `also_reads: None`.
///
/// # Symmetric variant (same path relative to home and project)
/// ```rust,ignore
/// define_skill_paths! {
///     symmetric: ".claude/skills",
/// }
/// ```
///
/// # Asymmetric variant (different paths for global and project)
/// ```rust,ignore
/// define_skill_paths! {
///     global: ".codeium/windsurf/skills",
///     project: ".windsurf/skills",
/// }
/// ```
#[macro_export]
macro_rules! define_skill_paths {
	// Symmetric variant - same path relative to home and project
	(
		symmetric: $path:literal,
	) => {
		fn global_skill_write_path() -> Option<std::path::PathBuf> {
			$crate::descriptor::home_dir().map(|home| home.join($path))
		}
		fn project_skill_write_path(
			root: &std::path::Path,
		) -> Option<std::path::PathBuf> {
			Some(root.join($path))
		}
	};

	// Asymmetric variant - different paths for global and project
	(
		global: $global_path:literal,
		project: $project_path:literal,
	) => {
		fn global_skill_write_path() -> Option<std::path::PathBuf> {
			$crate::descriptor::home_dir().map(|home| home.join($global_path))
		}
		fn project_skill_write_path(
			root: &std::path::Path,
		) -> Option<std::path::PathBuf> {
			Some(root.join($project_path))
		}
	};
}
