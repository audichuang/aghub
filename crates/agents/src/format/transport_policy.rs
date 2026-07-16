//! Shared MCP transport semantics for the strict preserve-and-merge dialects
//! (Grok TOML, Hermes YAML).
//!
//! Each dialect owns its SYNTAX: it walks its native `Value`, checks key
//! PRESENCE, and extracts only the selected transport branch's fields (with
//! per-field "must be a string/array/table" errors). The INVARIANTS that must
//! stay identical across the dialects — and that already drifted once (the
//! mixed-key rule was added to Grok, then hand-ported to Hermes) — live HERE:
//! [`reject_mixed_transport`], the `url`→Sse/StreamableHttp `type` split in
//! [`remote_transport`], the missing-transport error, and the serialize key set
//! ([`transport_keys`]) + key/value choice ([`transport_fields`]).
//!
//! This is deliberately NOT a `ConfigDoc` trait abstracting the whole document
//! (which would be a leaky seam hiding little); the shared surface is a handful
//! of pure functions over primitives + the neutral [`FieldValue`]. The dialects
//! keep their original phase order (validate `enabled` → reject mixed families
//! → dispatch on presence → extract the chosen branch), so error behaviour on
//! malformed input is byte-identical to before the extraction.

use crate::errors::{ConfigError, Result};
use crate::models::McpTransport;
use std::collections::HashMap;

/// Reject a server that mixes stdio-family (`command`/`args`/`env`) with
/// remote-family (`url`/`headers`[/`type`]) keys. Called on key PRESENCE before
/// any field is type-extracted, so a mixed entry is rejected regardless of the
/// field values (preserving the original error precedence). `single_remote`
/// drops `type` from the remote family in the message.
pub fn reject_mixed_transport(
	has_stdio: bool,
	has_remote: bool,
	name: &str,
	dialect: &str,
	single_remote: bool,
) -> Result<()> {
	if has_stdio && has_remote {
		let remote = if single_remote {
			"url/headers"
		} else {
			"url/headers/type"
		};
		return Err(ConfigError::InvalidConfig(format!(
			"{dialect} MCP server `{name}` mixes stdio keys (command/args/env) with remote keys ({remote})"
		)));
	}
	Ok(())
}

/// Build the remote transport for a `url`-based server. When `single_remote`
/// (Hermes) the dialect has one remote transport and `type` is not part of it,
/// so every remote is StreamableHttp. Otherwise (Grok) `type = "sse"` selects
/// SSE, a missing `type` or `"http"` selects StreamableHttp, and anything else
/// is an error.
pub fn remote_transport(
	url: String,
	headers: Option<HashMap<String, String>>,
	type_key: Option<String>,
	single_remote: bool,
	name: &str,
	dialect: &str,
) -> Result<McpTransport> {
	if single_remote {
		return Ok(McpTransport::StreamableHttp {
			url,
			headers,
			timeout: None,
		});
	}
	match type_key.as_deref() {
		Some("sse") => Ok(McpTransport::Sse {
			url,
			headers,
			timeout: None,
		}),
		None | Some("http") => Ok(McpTransport::StreamableHttp {
			url,
			headers,
			timeout: None,
		}),
		Some(other) => Err(ConfigError::InvalidConfig(format!(
			"{dialect} MCP server `{name}` has unknown `type` `{other}`"
		))),
	}
}

/// The error for a server with neither `command` nor `url`.
pub fn missing_transport_error(name: &str, dialect: &str) -> ConfigError {
	ConfigError::InvalidConfig(format!(
		"{dialect} MCP server `{name}` has neither `command` nor `url`"
	))
}

/// A native-agnostic transport field value; the dialect adapter converts it to
/// its `Value` type when writing the config back.
pub enum FieldValue {
	Str(String),
	List(Vec<String>),
	Map(HashMap<String, String>),
}

/// The transport-owned keys to strip from an existing entry before writing the
/// current transport (so a changed transport leaves no stale key): 6 for a
/// `type`-aware dialect, 5 for a single-remote one.
pub fn transport_keys(single_remote: bool) -> &'static [&'static str] {
	if single_remote {
		&["command", "args", "env", "url", "headers"]
	} else {
		&["command", "args", "env", "url", "headers", "type"]
	}
}

/// The transport keys + values to WRITE for a server, in insertion order. The
/// adapter converts each [`FieldValue`] to its native `Value` and inserts it.
/// Grok `Sse` adds `type="sse"`; `StreamableHttp` and every single-remote
/// transport omit `type`. `enabled` is written separately by the adapter.
pub fn transport_fields(
	transport: &McpTransport,
	single_remote: bool,
) -> Vec<(&'static str, FieldValue)> {
	let mut out = Vec::new();
	match transport {
		McpTransport::Stdio {
			command, args, env, ..
		} => {
			out.push(("command", FieldValue::Str(command.clone())));
			out.push(("args", FieldValue::List(args.clone())));
			if let Some(env) = env {
				out.push(("env", FieldValue::Map(env.clone())));
			}
		}
		McpTransport::Sse { url, headers, .. } => {
			out.push(("url", FieldValue::Str(url.clone())));
			if !single_remote {
				out.push(("type", FieldValue::Str("sse".to_string())));
			}
			if let Some(headers) = headers {
				out.push(("headers", FieldValue::Map(headers.clone())));
			}
		}
		McpTransport::StreamableHttp { url, headers, .. } => {
			out.push(("url", FieldValue::Str(url.clone())));
			if let Some(headers) = headers {
				out.push(("headers", FieldValue::Map(headers.clone())));
			}
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reject_mixed_only_when_both_families_present() {
		assert!(reject_mixed_transport(true, true, "m", "Grok", false).is_err());
		assert!(reject_mixed_transport(true, false, "s", "Grok", false).is_ok());
		assert!(reject_mixed_transport(false, true, "r", "Grok", false).is_ok());
		// The remote-family wording differs by dialect.
		let g = format!(
			"{:?}",
			reject_mixed_transport(true, true, "m", "Grok", false).unwrap_err()
		);
		assert!(g.contains("url/headers/type"));
		let h = format!(
			"{:?}",
			reject_mixed_transport(true, true, "m", "Hermes", true)
				.unwrap_err()
		);
		assert!(h.contains("url/headers)") && !h.contains("type"));
	}

	#[test]
	fn remote_transport_splits_sse_and_http_only_when_not_single_remote() {
		let sse = remote_transport(
			"u".into(),
			None,
			Some("sse".into()),
			false,
			"r",
			"Grok",
		)
		.unwrap();
		assert!(matches!(sse, McpTransport::Sse { .. }));

		let http = remote_transport("u".into(), None, None, false, "r", "Grok")
			.unwrap();
		assert!(matches!(http, McpTransport::StreamableHttp { .. }));

		// single_remote ignores `type` entirely — always StreamableHttp.
		let one = remote_transport(
			"u".into(),
			None,
			Some("sse".into()),
			true,
			"r",
			"Hermes",
		)
		.unwrap();
		assert!(matches!(one, McpTransport::StreamableHttp { .. }));
	}

	#[test]
	fn unknown_type_is_rejected_only_when_not_single_remote() {
		assert!(remote_transport(
			"u".into(),
			None,
			Some("grpc".into()),
			false,
			"r",
			"Grok"
		)
		.is_err());
		// single_remote never inspects `type`, so it cannot be "unknown".
		assert!(remote_transport(
			"u".into(),
			None,
			Some("grpc".into()),
			true,
			"r",
			"Hermes"
		)
		.is_ok());
	}

	fn keys(fields: &[(&'static str, FieldValue)]) -> Vec<&'static str> {
		fields.iter().map(|(k, _)| *k).collect()
	}
	fn str_of(fields: &[(&'static str, FieldValue)], key: &str) -> String {
		match fields.iter().find(|(k, _)| *k == key) {
			Some((_, FieldValue::Str(s))) => s.clone(),
			_ => panic!("field `{key}` is missing or not a string"),
		}
	}

	#[test]
	fn transport_keys_are_the_full_owned_set() {
		assert_eq!(
			transport_keys(false),
			["command", "args", "env", "url", "headers", "type"]
		);
		assert_eq!(
			transport_keys(true),
			["command", "args", "env", "url", "headers"]
		);
	}

	#[test]
	fn serialize_fields_stdio_carry_exact_values_and_omit_absent_env() {
		let env = HashMap::from([("K".to_string(), "V".to_string())]);
		let stdio = McpTransport::Stdio {
			command: "mycmd".into(),
			args: vec!["--a".into(), "--b".into()],
			env: Some(env.clone()),
			timeout: None,
		};
		let fields = transport_fields(&stdio, false);
		assert_eq!(keys(&fields), ["command", "args", "env"]);
		assert_eq!(str_of(&fields, "command"), "mycmd");
		match fields.iter().find(|(k, _)| *k == "args") {
			Some((_, FieldValue::List(l))) => assert_eq!(l, &["--a", "--b"]),
			_ => panic!("args must be a non-empty list with the exact values"),
		}
		match fields.iter().find(|(k, _)| *k == "env") {
			Some((_, FieldValue::Map(m))) => assert_eq!(m, &env),
			_ => panic!("env must carry the exact map"),
		}
		// env is OMITTED (not written empty) when None.
		let no_env = McpTransport::Stdio {
			command: "c".into(),
			args: vec![],
			env: None,
			timeout: None,
		};
		assert_eq!(
			keys(&transport_fields(&no_env, false)),
			["command", "args"],
			"absent env must not emit an `env` key"
		);
	}

	#[test]
	fn serialize_fields_sse_writes_type_and_values_only_for_type_aware_dialect()
	{
		let headers = HashMap::from([("H".to_string(), "1".to_string())]);
		let sse = McpTransport::Sse {
			url: "https://x/sse".into(),
			headers: Some(headers.clone()),
			timeout: None,
		};
		let grok = transport_fields(&sse, false);
		assert_eq!(keys(&grok), ["url", "type", "headers"]);
		assert_eq!(str_of(&grok, "url"), "https://x/sse");
		assert_eq!(str_of(&grok, "type"), "sse");
		match grok.iter().find(|(k, _)| *k == "headers") {
			Some((_, FieldValue::Map(m))) => assert_eq!(m, &headers),
			_ => panic!("headers must carry the exact map"),
		}
		// single-remote: url + headers, never `type`.
		assert_eq!(keys(&transport_fields(&sse, true)), ["url", "headers"]);
	}

	#[test]
	fn serialize_fields_http_is_url_only_and_omits_absent_headers() {
		let http = McpTransport::StreamableHttp {
			url: "u".into(),
			headers: None,
			timeout: None,
		};
		let fields = transport_fields(&http, false);
		assert_eq!(
			keys(&fields),
			["url"],
			"StreamableHttp: url only — never type, no empty headers"
		);
		assert_eq!(str_of(&fields, "url"), "u");
	}
}
