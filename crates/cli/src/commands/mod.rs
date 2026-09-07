pub mod add;
pub mod apply_update;
pub mod check;
pub mod coverage;
pub mod delete;
pub mod disable;
pub mod doctor;
pub mod enable;
pub mod get;
pub mod inference;
pub mod plugin;
pub mod prune;
pub mod repair;
pub mod skill_usage;
pub mod source;
pub mod transfer;
pub mod update;

use aghub_core::models::McpTransport;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

/// Print a JSON value as an aligned `key  value` block, or as pretty JSON when
/// `json` is set.
///
/// Nested objects/arrays render as their compact JSON on one line — the views
/// this serves (`describe`, a provider record) are flat, and a real YAML
/// dependency would buy nothing. Null fields are omitted: "author: null" is
/// noise a human never wants, while `--json` still carries the key.
pub(crate) fn print_value(value: &serde_json::Value, json: bool) -> Result<()> {
	if json {
		println!("{}", serde_json::to_string_pretty(value)?);
		return Ok(());
	}
	let Some(map) = value.as_object() else {
		println!("{value}");
		return Ok(());
	};
	let width = map
		.iter()
		.filter(|(_, v)| !v.is_null())
		.map(|(k, _)| k.len())
		.max()
		.unwrap_or(0);
	for (key, val) in map {
		if val.is_null() {
			continue;
		}
		let rendered = match val {
			serde_json::Value::String(s) => s.clone(),
			other => other.to_string(),
		};
		println!("{key:<width$}  {rendered}");
	}
	Ok(())
}

/// When set, the CLI stores inference API keys in this plaintext JSON file
/// instead of the OS keyring. Test-only: it lets headless CI exercise the full
/// inference path where no keyring (linux secret-service/dbus) is available.
const TEST_CREDENTIAL_FILE_ENV: &str = "AGHUB_TEST_CREDENTIAL_FILE";

/// The skill locks a read-only command is about to REPORT ON, read exactly
/// once and failing CLOSED.
///
/// The lock read paths fail OPEN to an empty lock, deliberately, so one corrupt
/// file does not break every query. But `check` and `doctor` present the lock's
/// CONTENTS as their answer, and an empty view there reads as "nothing is
/// installed" — they answered `[]` on exit 0 with an empty stderr for a
/// `skills-lock.json` full of entries they simply could not parse, and `doctor`
/// went on to classify the still-present skills `untracked` and recommend
/// deleting them.
///
/// This returns the PARSED locks rather than a verdict. A predicate-only probe
/// left each command to read the file a second time through the fail-open
/// reader, and a non-aghub writer (an editor, `npx skills`) that truncates it
/// between the two reads puts the empty answer back — the same bug, in a
/// narrower window. One read, consumed directly.
///
/// `None` means the scope was not requested, NOT that its file is missing: an
/// absent lock parses as an empty one, exactly as the fail-open reader would
/// have produced.
#[derive(Debug, Default)]
pub(crate) struct LockSnapshot {
	pub global: Option<skill::lock::SkillLockFile>,
	pub project: Option<skill::lock::local::LocalSkillLockFile>,
}

/// See [`LockSnapshot`].
pub(crate) fn read_locks_checked(
	want_global: bool,
	project_root: Option<&std::path::Path>,
) -> Result<LockSnapshot> {
	// Mapped to `ConfigError::Io` rather than passed through as a raw
	// `io::Error`: `report_failure` recovers the shared error code by
	// downcasting to `ConfigError`, and a bare `io::Error` in the anyhow chain
	// degrades `IO_ERROR` into the `CLI_ERROR` fallback.
	let typed = |error: std::io::Error| {
		anyhow::Error::from(aghub_core::errors::ConfigError::Io(error))
	};
	Ok(LockSnapshot {
		global: want_global
			.then(skill::lock::read_global_lock_checked)
			.transpose()
			.map_err(typed)?,
		project: project_root
			.map(|root| skill::lock::local::read_local_lock_checked(Some(root)))
			.transpose()
			.map_err(typed)?,
	})
}

/// App data directory shared by the CLI, desktop, and HTTP API.
///
/// One formula, owned by [`aghub_core::paths::app_data_dir`]: `$AGHUB_DATA_DIR`
/// else `dirs::data_dir()/aghub`. All three surfaces open the same SQLite db and
/// credential-keyring namespace, so a provider added by the desktop is listed by
/// the CLI and their shared keyring key still has a row to belong to.
pub(crate) fn app_data_dir() -> PathBuf {
	aghub_core::paths::app_data_dir()
}

/// Credential backend the CLI uses for inference API keys: the OS keyring in
/// production, or a plaintext file when `$AGHUB_TEST_CREDENTIAL_FILE` is set.
///
/// A runtime enum (not generics) so [`inference_store`] returns one concrete
/// type regardless of which backend the environment selects.
#[derive(Debug, Clone)]
pub(crate) enum CliCredentialStore {
	Native(aghub_inference::NativeCredentialStore),
	File(aghub_inference::FileCredentialStore),
}

impl aghub_inference::CredentialStore for CliCredentialStore {
	fn get_api_key(
		&self,
		provider_id: &str,
	) -> aghub_inference::Result<Option<String>> {
		match self {
			Self::Native(s) => s.get_api_key(provider_id),
			Self::File(s) => s.get_api_key(provider_id),
		}
	}

	fn set_api_key(
		&self,
		provider_id: &str,
		api_key: &str,
	) -> aghub_inference::Result<()> {
		match self {
			Self::Native(s) => s.set_api_key(provider_id, api_key),
			Self::File(s) => s.set_api_key(provider_id, api_key),
		}
	}

	fn delete_api_key(&self, provider_id: &str) -> aghub_inference::Result<()> {
		match self {
			Self::Native(s) => s.delete_api_key(provider_id),
			Self::File(s) => s.delete_api_key(provider_id),
		}
	}
}

/// Inference provider store rooted at [`app_data_dir`].
///
/// Uses the native keyring backend in production, so providers created via the
/// desktop or API are visible to the CLI. When `$AGHUB_TEST_CREDENTIAL_FILE` is
/// set, a plaintext file backs the keys instead (headless test runs) — debug
/// builds only, like every other test hook: a release binary must never let a
/// stray env var silently redirect API keys to a plaintext file.
pub(crate) fn inference_store(
) -> aghub_inference::InferenceProviderStore<CliCredentialStore> {
	let test_credential_file = if cfg!(debug_assertions) {
		std::env::var_os(TEST_CREDENTIAL_FILE_ENV)
	} else {
		None
	};
	let credentials = match test_credential_file {
		Some(path) => CliCredentialStore::File(
			aghub_inference::FileCredentialStore::new(path),
		),
		None => {
			CliCredentialStore::Native(aghub_inference::NativeCredentialStore)
		}
	};
	aghub_inference::InferenceProviderStore::with_credentials(
		app_data_dir(),
		credentials,
	)
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

	/// Serialize the env-mutating app-data-dir tests so they don't race each
	/// other's `AGHUB_DATA_DIR` set/remove.
	fn env_lock() -> std::sync::MutexGuard<'static, ()> {
		static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
		LOCK.lock().unwrap_or_else(|e| e.into_inner())
	}

	/// The CLI half of the cross-surface parity pin: the CLI, the API and the
	/// desktop must open ONE app data root, or a provider added in the desktop
	/// UI is invisible to `aghub-cli inference list` while both surfaces share
	/// one keyring namespace.
	///
	/// Compares the real functions, not two hand-written literals — the old
	/// version of this test re-spelled the formula, so it could not notice the
	/// API resolving somewhere else. The API half is
	/// `aghub_api::tests::default_app_data_dir_matches_core_seam`; both pin
	/// against `aghub_core::paths::app_data_dir`, which is where the two meet
	/// (`aghub-cli` is bin-only, so nothing can link this function directly).
	#[test]
	fn app_data_dir_matches_core_seam() {
		let _g = env_lock();
		let restore = std::env::var_os(aghub_core::paths::DATA_DIR_ENV);

		std::env::remove_var(aghub_core::paths::DATA_DIR_ENV);
		assert_eq!(app_data_dir(), aghub_core::paths::app_data_dir());
		assert_eq!(
			app_data_dir().file_name().and_then(|n| n.to_str()),
			Some("aghub")
		);

		std::env::set_var(
			aghub_core::paths::DATA_DIR_ENV,
			"/tmp/aghub-seam-xyz",
		);
		assert_eq!(app_data_dir(), aghub_core::paths::app_data_dir());
		assert_eq!(app_data_dir(), PathBuf::from("/tmp/aghub-seam-xyz"));

		match restore {
			Some(v) => std::env::set_var(aghub_core::paths::DATA_DIR_ENV, v),
			None => std::env::remove_var(aghub_core::paths::DATA_DIR_ENV),
		}
	}

	#[test]
	fn app_data_dir_honors_override_env() {
		// `$AGHUB_DATA_DIR` takes precedence so tests can pin the SQLite root to a
		// throwaway dir without guessing each platform's dirs::data_dir().
		let _g = env_lock();
		let restore = std::env::var_os(aghub_core::paths::DATA_DIR_ENV);
		std::env::set_var(
			aghub_core::paths::DATA_DIR_ENV,
			"/tmp/aghub-override-xyz",
		);
		assert_eq!(
			app_data_dir(),
			std::path::PathBuf::from("/tmp/aghub-override-xyz")
		);
		match restore {
			Some(v) => std::env::set_var(aghub_core::paths::DATA_DIR_ENV, v),
			None => std::env::remove_var(aghub_core::paths::DATA_DIR_ENV),
		}
	}

	#[test]
	fn inference_store_is_rooted_at_app_data_dir() {
		// Hold the shared env lock: this reads `app_data_dir()` (which consults
		// `$AGHUB_DATA_DIR`), and `app_data_dir_honors_override_env` mutates that
		// var. Without the lock they race under `cargo test --workspace`'s heavier
		// parallel load (green locally / `-p`, flaky on the CI matrix).
		let _g = env_lock();
		let store = inference_store();
		assert_eq!(store.app_data_dir(), app_data_dir().as_path());
	}
}
