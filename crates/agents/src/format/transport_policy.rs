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

/// The key each dialect tags its remote transport with: Grok spells it `type`,
/// Hermes spells it `transport`.
pub fn remote_tag_key(single_remote: bool) -> &'static str {
	if single_remote {
		"transport"
	} else {
		"type"
	}
}

/// Build the remote transport for a `url`-based server. `"sse"` selects SSE, a
/// missing tag or `"http"` selects StreamableHttp, and anything else is an
/// error. `single_remote` (Hermes) additionally accepts the spelled-out
/// `"streamable-http"` and reports the mismatch against its own tag key.
pub fn remote_transport(
	url: String,
	headers: Option<HashMap<String, String>>,
	type_key: Option<String>,
	single_remote: bool,
	name: &str,
	dialect: &str,
) -> Result<McpTransport> {
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
		Some("streamable-http") if single_remote => {
			Ok(McpTransport::StreamableHttp {
				url,
				headers,
				timeout: None,
			})
		}
		Some(other) => Err(ConfigError::InvalidConfig(format!(
			"{dialect} MCP server `{name}` has unknown `{key}` `{other}`",
			key = remote_tag_key(single_remote)
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
/// current transport (so a changed transport leaves no stale key). Both dialects
/// own their remote tag key; only its spelling differs.
pub fn transport_keys(single_remote: bool) -> &'static [&'static str] {
	if single_remote {
		&["command", "args", "env", "url", "headers", "transport"]
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
			out.push((
				remote_tag_key(single_remote),
				FieldValue::Str("sse".to_string()),
			));
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
	fn remote_transport_splits_sse_and_http_in_both_dialects() {
		for (single_remote, dialect) in [(false, "Grok"), (true, "Hermes")] {
			let sse = remote_transport(
				"u".into(),
				None,
				Some("sse".into()),
				single_remote,
				"r",
				dialect,
			)
			.unwrap();
			assert!(
				matches!(sse, McpTransport::Sse { .. }),
				"{dialect} must keep an explicit SSE tag"
			);

			let untagged = remote_transport(
				"u".into(),
				None,
				None,
				single_remote,
				"r",
				dialect,
			)
			.unwrap();
			assert!(matches!(untagged, McpTransport::StreamableHttp { .. }));
		}

		// Only Hermes spells streamable HTTP out in full.
		assert!(remote_transport(
			"u".into(),
			None,
			Some("streamable-http".into()),
			true,
			"r",
			"Hermes"
		)
		.is_ok());
	}

	#[test]
	fn unknown_tag_is_rejected_in_both_dialects() {
		for (single_remote, dialect, key) in
			[(false, "Grok", "type"), (true, "Hermes", "transport")]
		{
			let error = remote_transport(
				"u".into(),
				None,
				Some("grpc".into()),
				single_remote,
				"r",
				dialect,
			)
			.unwrap_err();
			let message = error.to_string();
			assert!(message.contains("grpc"), "got: {message}");
			assert!(
				message.contains(key),
				"{dialect} must name its own tag key: {message}"
			);
		}
	}

	#[test]
	fn remote_tag_key_is_per_dialect() {
		assert_eq!(remote_tag_key(false), "type");
		assert_eq!(remote_tag_key(true), "transport");
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
			["command", "args", "env", "url", "headers", "transport"]
		);
		// Every key a dialect WRITES must also be one it STRIPS, or a changed
		// transport leaves the old tag behind.
		for single_remote in [false, true] {
			for transport in [
				McpTransport::stdio("c", vec![]),
				McpTransport::sse("u"),
				McpTransport::streamable_http("u"),
			] {
				for (key, _) in transport_fields(&transport, single_remote) {
					assert!(
						transport_keys(single_remote).contains(&key),
						"`{key}` is written but never stripped"
					);
				}
			}
		}
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
	fn serialize_fields_sse_writes_each_dialects_own_tag() {
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
		// Hermes spells the same tag `transport` — it must not be dropped, or
		// the server reads back as streamable HTTP.
		let hermes = transport_fields(&sse, true);
		assert_eq!(keys(&hermes), ["url", "transport", "headers"]);
		assert_eq!(str_of(&hermes, "transport"), "sse");
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
