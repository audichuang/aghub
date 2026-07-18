//! Pure, unit-testable CLI argument parsing for the `aghub-api` binary.
//!
//! Kept dependency-free (hand-rolled parsing, no clap) so the binary stays
//! lean and the parser contract is locked by unit tests. The `AGHUB_API_PORT=`
//! line prefix is a cross-crate contract consumed by the desktop SSH bring-up
//! parser, so it lives here as a public constant and is asserted in tests.

/// Exact stdout line prefix the binary emits to report its chosen port.
///
/// The desktop remote-bring-up parser reads `AGHUB_API_PORT=<n>` from the
/// remote log to learn the VM-side port. Asserting this literal in both crates'
/// tests means any drift breaks a test rather than production.
pub const PORT_LINE_PREFIX: &str = "AGHUB_API_PORT=";

/// Exact stdout line `--capabilities` emits, listing the wire features this
/// binary supports as a single space-separated, stable token list.
///
/// The desktop remote bring-up probes for this WITHOUT a running HTTP server
/// (an old binary lacks `--capabilities`, so the probe fails and the desktop
/// treats the remote as not-supporting the feature — see
/// `aghub_remote::ssh::CAPABILITY_LINE_PREFIX`). The line is a cross-crate
/// contract: `aghub-remote` matches the literal prefix + tokens, so any drift
/// breaks a test rather than production.
pub const CAPABILITIES_LINE_PREFIX: &str = "AGHUB_API_CAPABILITIES=";

/// Capability token advertising controller-side git-credential forwarding
/// (the `X-Aghub-Git-Tokens` header support added in this feature). Present in
/// the `--capabilities` line iff this binary honors the forward header.
pub const CAP_GIT_CREDENTIAL_FORWARDING: &str = "git-credential-forwarding";

/// Parsed CLI configuration for the `aghub-api` binary.
///
/// The default (all flags `false`) means "no flags passed": pick a free
/// ephemeral port at runtime and start the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
	/// Port to bind. `0` means "pick a free ephemeral port at runtime".
	pub port: u16,
	/// Whether `--version`/`-V` was requested.
	pub version: bool,
	/// Whether `--capabilities` was requested (print the capability line and
	/// exit without starting the server).
	pub capabilities: bool,
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
/// Supports `--port <n>`, `--port=<n>`, `--version`, `-V`, and
/// `--capabilities`. Returns [`ParseError`] for unknown flags, missing values,
/// or out-of-range/non-numeric ports.
pub fn parse_args(args: Vec<String>) -> Result<Config, ParseError> {
	let mut config = Config::default();
	let mut iter = args.into_iter().skip(1);

	while let Some(arg) = iter.next() {
		match arg.as_str() {
			"--version" | "-V" => config.version = true,
			"--capabilities" => config.capabilities = true,
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
	format!("aghub-api {}", crate::VERSION)
}

/// Capability line printed by `--capabilities`, e.g.
/// `"AGHUB_API_CAPABILITIES=git-credential-forwarding"`.
///
/// Space-separated tokens after the prefix. Today there is exactly one token
/// ([`CAP_GIT_CREDENTIAL_FORWARDING`]); future wire features append more.
pub fn capabilities_string() -> String {
	format!("{CAPABILITIES_LINE_PREFIX}{CAP_GIT_CREDENTIAL_FORWARDING}")
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
	fn capabilities_flag_sets_capabilities() {
		let config = parse_args(args(&["--capabilities"])).expect("parse");
		assert!(config.capabilities);
		assert!(!config.version);
		assert_eq!(config.port, 0);
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
	fn version_string_matches_crate_version() {
		// `crate::VERSION` is git-derived by build.rs (release tag /
		// `git describe` / `CARGO_PKG_VERSION` fallback — see build.rs), so
		// this asserts the wiring rather than a hardcoded literal.
		let expected = format!("aghub-api {}", crate::VERSION);
		assert_eq!(version_string(), expected);
		assert!(version_string().starts_with("aghub-api "));
	}

	#[test]
	fn port_line_prefix_is_locked_literal() {
		assert_eq!(PORT_LINE_PREFIX, "AGHUB_API_PORT=");
	}

	#[test]
	fn capabilities_line_prefix_and_token_are_locked_literals() {
		// Cross-crate contract: aghub-remote matches these literals when
		// probing over SSH. Any drift must break a test, not production.
		assert_eq!(CAPABILITIES_LINE_PREFIX, "AGHUB_API_CAPABILITIES=");
		assert_eq!(CAP_GIT_CREDENTIAL_FORWARDING, "git-credential-forwarding");
	}

	#[test]
	fn capabilities_string_advertises_credential_forwarding() {
		let line = capabilities_string();
		assert!(line.starts_with(CAPABILITIES_LINE_PREFIX), "{line}");
		assert!(line.contains(CAP_GIT_CREDENTIAL_FORWARDING), "{line}");
		assert_eq!(line, "AGHUB_API_CAPABILITIES=git-credential-forwarding");
	}
}
