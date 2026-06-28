pub mod add;
pub mod apply_update;
pub mod check;
pub mod coverage;
pub mod delete;
pub mod disable;
pub mod enable;
pub mod get;
pub mod inference;
pub mod plugin;
pub mod prune;
pub mod source;
pub mod transfer;
pub mod update;

use aghub_core::models::McpTransport;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

/// App data directory shared by the CLI, desktop, and HTTP API.
///
/// Byte-identical to `api::default_app_data_dir` so all three surfaces open the
/// same SQLite db and credential-keyring namespace — a key stored by the
/// desktop is readable by the CLI and vice-versa.
// ponytail: mirrors api::default_app_data_dir; keep in sync.
pub(crate) fn app_data_dir() -> PathBuf {
	dirs::data_dir()
		.unwrap_or_else(std::env::temp_dir)
		.join("aghub")
}

/// Inference provider store rooted at [`app_data_dir`].
///
/// Reuses the native credential backend, so providers created via the desktop
/// or API are visible to the CLI without any extra keyring wiring.
pub(crate) fn inference_store() -> aghub_inference::InferenceProviderStore {
	aghub_inference::InferenceProviderStore::new(app_data_dir())
}

/// Parse MCP transport from command-line arguments.
///
/// Only the clap-shaped string parsing (`KEY:VALUE` headers, `KEY=VALUE` env)
/// lives here; the branching + compatibility/timeout validation lives in the
/// shared [`McpTransport::from_inputs`] constructor so the CLI and API agree.
pub fn parse_mcp_transport(
	command: Option<String>,
	url: Option<String>,
	transport_type: &str,
	headers: Vec<String>,
	env_vars: Vec<String>,
	timeout: Option<u64>,
) -> Result<Option<McpTransport>> {
	McpTransport::from_inputs(
		command,
		url,
		transport_type,
		parse_headers(headers)?,
		parse_env_vars(env_vars)?,
		timeout,
	)
	.map_err(|e| anyhow::anyhow!(e.to_string()))
}

/// Parse environment variables from `KEY=VALUE` format.
///
/// Rejects malformed entries (missing `=` or empty key) with an actionable
/// error instead of silently dropping them, so a typo'd `--env` never
/// disappears unnoticed before the shared [`McpTransport::from_inputs`] seam.
pub fn parse_env_vars(
	env_vars: Vec<String>,
) -> Result<Option<HashMap<String, String>>> {
	if env_vars.is_empty() {
		return Ok(None);
	}
	let mut env_map = HashMap::new();
	for env_var in env_vars {
		let Some((key, value)) = env_var.split_once('=') else {
			return Err(anyhow::anyhow!(
				"--env must be KEY=VALUE, got '{env_var}'"
			));
		};
		if key.is_empty() {
			return Err(anyhow::anyhow!(
				"--env must be KEY=VALUE, got '{env_var}'"
			));
		}
		env_map.insert(key.to_string(), value.to_string());
	}
	Ok(Some(env_map))
}

/// Parse HTTP headers from `KEY:VALUE` format.
///
/// Rejects malformed entries (missing `:` or empty key) with an actionable
/// error instead of silently dropping them.
pub fn parse_headers(
	headers: Vec<String>,
) -> Result<Option<HashMap<String, String>>> {
	if headers.is_empty() {
		return Ok(None);
	}
	let mut map = HashMap::new();
	for header in headers {
		let Some((key, value)) = header.split_once(':') else {
			return Err(anyhow::anyhow!(
				"--header must be KEY:VALUE, got '{header}'"
			));
		};
		let key = key.trim();
		if key.is_empty() {
			return Err(anyhow::anyhow!(
				"--header must be KEY:VALUE, got '{header}'"
			));
		}
		map.insert(key.to_string(), value.trim().to_string());
	}
	Ok(Some(map))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_env_vars_empty_is_none() {
		assert!(parse_env_vars(vec![]).unwrap().is_none());
	}

	#[test]
	fn parse_env_vars_rejects_missing_equals() {
		let err = parse_env_vars(vec!["BAD".to_string()]).unwrap_err();
		assert!(err.to_string().contains("KEY=VALUE"));
		assert!(err.to_string().contains("BAD"));
	}

	#[test]
	fn parse_env_vars_rejects_empty_key() {
		assert!(parse_env_vars(vec!["=v".to_string()]).is_err());
	}

	#[test]
	fn parse_env_vars_keeps_value_with_equals() {
		let m = parse_env_vars(vec!["K=a=b".to_string()]).unwrap().unwrap();
		assert_eq!(m.get("K").unwrap(), "a=b");
	}

	#[test]
	fn parse_headers_empty_is_none() {
		assert!(parse_headers(vec![]).unwrap().is_none());
	}

	#[test]
	fn parse_headers_rejects_missing_colon() {
		let err = parse_headers(vec!["bad".to_string()]).unwrap_err();
		assert!(err.to_string().contains("KEY:VALUE"));
		assert!(err.to_string().contains("bad"));
	}

	#[test]
	fn parse_headers_rejects_empty_key() {
		assert!(parse_headers(vec![":v".to_string()]).is_err());
	}

	#[test]
	fn parse_headers_trims_and_keeps_value_colon() {
		let m = parse_headers(vec!["X-Foo: http://h".to_string()])
			.unwrap()
			.unwrap();
		assert_eq!(m.get("X-Foo").unwrap(), "http://h");
	}

	#[test]
	fn app_data_dir_matches_api_default_formula() {
		// Must stay byte-identical to api::default_app_data_dir so the CLI,
		// desktop, and API share one SQLite db + keyring namespace.
		let expected = dirs::data_dir()
			.unwrap_or_else(std::env::temp_dir)
			.join("aghub");
		assert_eq!(app_data_dir(), expected);
	}

	#[test]
	fn app_data_dir_ends_in_aghub() {
		assert_eq!(
			app_data_dir().file_name().and_then(|n| n.to_str()),
			Some("aghub")
		);
	}

	#[test]
	fn inference_store_is_rooted_at_app_data_dir() {
		let store = inference_store();
		assert_eq!(store.app_data_dir(), app_data_dir().as_path());
	}
}
