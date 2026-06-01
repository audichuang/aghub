//! Pure, unit-testable CLI argument parsing for the `aghub-api` binary.
//!
//! Kept dependency-free (hand-rolled parsing, no clap) so the binary stays
//! lean and the parser contract is locked by unit tests. The `AGHUB_API_PORT=`
//! line prefix is a cross-crate contract consumed by the desktop SSH bring-up
//! parser, so it lives here as a public constant and is asserted in tests.

use std::net::TcpListener;

/// Exact stdout line prefix the binary emits to report its chosen port.
///
/// The desktop remote-bring-up parser reads `AGHUB_API_PORT=<n>` from the
/// remote log to learn the VM-side port. Asserting this literal in both crates'
/// tests means any drift breaks a test rather than production.
pub const PORT_LINE_PREFIX: &str = "AGHUB_API_PORT=";

/// Parsed CLI configuration for the `aghub-api` binary.
///
/// The default (`port: 0`, `version: false`) means "no flags passed": pick a
/// free ephemeral port at runtime and start the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
	/// Port to bind. `0` means "pick a free ephemeral port at runtime".
	pub port: u16,
	/// Whether `--version`/`-V` was requested.
	pub version: bool,
}

/// Errors that can occur while parsing CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
	/// A flag that expects a value was given none (e.g. trailing `--port`).
	MissingValue(String),
	/// A port value was not a valid `u16`.
	InvalidPort(String),
	/// An unrecognized flag/argument was encountered.
	UnknownFlag(String),
}

impl std::fmt::Display for ParseError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			ParseError::MissingValue(flag) => {
				write!(f, "missing value for {flag}")
			}
			ParseError::InvalidPort(value) => {
				write!(f, "invalid port value: {value}")
			}
			ParseError::UnknownFlag(flag) => {
				write!(f, "unknown flag: {flag}")
			}
		}
	}
}

impl std::error::Error for ParseError {}

/// Parse CLI arguments (including `argv[0]`, which is skipped).
///
/// Supports `--port <n>`, `--port=<n>`, `--version`, and `-V`. Returns
/// [`ParseError`] for unknown flags, missing values, or out-of-range/non-numeric
/// ports.
pub fn parse_args(args: Vec<String>) -> Result<Config, ParseError> {
	let mut config = Config::default();
	let mut iter = args.into_iter().skip(1);

	while let Some(arg) = iter.next() {
		match arg.as_str() {
			"--version" | "-V" => config.version = true,
			"--port" => {
				let value = iter.next().ok_or_else(|| {
					ParseError::MissingValue("--port".to_string())
				})?;
				config.port = parse_port(&value)?;
			}
			other if other.starts_with("--port=") => {
				let value = &other["--port=".len()..];
				config.port = parse_port(value)?;
			}
			other => return Err(ParseError::UnknownFlag(other.to_string())),
		}
	}

	Ok(config)
}

fn parse_port(value: &str) -> Result<u16, ParseError> {
	value
		.parse::<u16>()
		.map_err(|_| ParseError::InvalidPort(value.to_string()))
}

/// Version string printed by `--version`, e.g. `"aghub-api 1.1.1"`.
pub fn version_string() -> String {
	format!("aghub-api {}", env!("CARGO_PKG_VERSION"))
}

/// Pick a free TCP port by binding `127.0.0.1:0` and reading the assigned port.
///
/// Mirrors the desktop `find_available_port` helper. The listener is dropped
/// before returning, so the port is immediately re-bindable (with the usual
/// TOCTOU caveat).
pub fn pick_free_port() -> std::io::Result<u16> {
	let listener = TcpListener::bind("127.0.0.1:0")?;
	let port = listener.local_addr()?.port();
	Ok(port)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn args(items: &[&str]) -> Vec<String> {
		std::iter::once("aghub-api")
			.chain(items.iter().copied())
			.map(String::from)
			.collect()
	}

	#[test]
	fn empty_args_yields_default_config() {
		let config = parse_args(args(&[])).expect("parse");
		assert_eq!(config.port, 0);
		assert!(!config.version);
		assert_eq!(config, Config::default());
	}

	#[test]
	fn port_with_space_separated_value() {
		let config = parse_args(args(&["--port", "7777"])).expect("parse");
		assert_eq!(config.port, 7777);
		assert!(!config.version);
	}

	#[test]
	fn port_with_equals_value() {
		let config = parse_args(args(&["--port=7777"])).expect("parse");
		assert_eq!(config.port, 7777);
		assert!(!config.version);
	}

	#[test]
	fn version_long_flag_sets_version() {
		let config = parse_args(args(&["--version"])).expect("parse");
		assert!(config.version);
	}

	#[test]
	fn version_short_flag_sets_version() {
		let config = parse_args(args(&["-V"])).expect("parse");
		assert!(config.version);
	}

	#[test]
	fn non_numeric_port_is_error() {
		let err = parse_args(args(&["--port", "abc"])).unwrap_err();
		assert_eq!(err, ParseError::InvalidPort("abc".to_string()));
	}

	#[test]
	fn out_of_range_port_is_error() {
		let err = parse_args(args(&["--port", "70000"])).unwrap_err();
		assert_eq!(err, ParseError::InvalidPort("70000".to_string()));
	}

	#[test]
	fn equals_non_numeric_port_is_error() {
		let err = parse_args(args(&["--port=abc"])).unwrap_err();
		assert_eq!(err, ParseError::InvalidPort("abc".to_string()));
	}

	#[test]
	fn port_with_no_value_is_error() {
		let err = parse_args(args(&["--port"])).unwrap_err();
		assert_eq!(err, ParseError::MissingValue("--port".to_string()));
	}

	#[test]
	fn unknown_flag_is_error() {
		let err = parse_args(args(&["--bogus"])).unwrap_err();
		assert_eq!(err, ParseError::UnknownFlag("--bogus".to_string()));
	}

	#[test]
	fn version_string_matches_cargo_pkg_version() {
		let expected = format!("aghub-api {}", env!("CARGO_PKG_VERSION"));
		assert_eq!(version_string(), expected);
		assert!(version_string().starts_with("aghub-api "));
	}

	#[test]
	fn version_string_is_one_one_one() {
		// Workspace version is pinned to 1.1.1.
		assert_eq!(version_string(), "aghub-api 1.1.1");
	}

	#[test]
	fn port_line_prefix_is_locked_literal() {
		assert_eq!(PORT_LINE_PREFIX, "AGHUB_API_PORT=");
	}

	#[test]
	fn pick_free_port_is_nonzero_and_rebindable() {
		let port = pick_free_port().expect("pick free port");
		assert!(port > 0);
		// Immediately re-bindable after the helper drops its listener.
		let listener =
			TcpListener::bind(("127.0.0.1", port)).expect("re-bind freed port");
		drop(listener);
	}
}
