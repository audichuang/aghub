//! The MCP decisions that are the same for every dialect, expressed as DATA the
//! dialect declares rather than code it re-states.
//!
//! Each dialect owns its SYNTAX: it walks its native `Value`, checks key
//! PRESENCE, and extracts only the selected transport branch's fields (with
//! per-field "must be a string/array/table" errors). What lives HERE is the
//! handful of answers that must not differ between them:
//!
//! - [`RemoteVocabulary`] — the words a dialect has for a remote transport.
//!   [`RemoteVocabulary::writes_sse`] is DERIVED from `sse` being non-empty, so
//!   [`RemoteVocabulary::refuse_unwritable`] is the only place that decides WHEN
//!   an SSE server must be refused. Declaring is only half of it: a dialect
//!   still has to CALL it (all seven do, at the top of their serialize loop).
//!   Declaring `sse: ""` without the call does not refuse — it writes an EMPTY
//!   tag, which the dialect's own reader then rejects or calls some other
//!   transport. `mcp_dialect_roundtrip` is what forces the call, NOT
//!   `mcp_dialect_decisions`: emptying Grok's `sse` and neutering the call
//!   leaves the decisions table green (its row says `Spelled`, and writing an
//!   empty tag still "succeeds"), while
//!   `every_agent_reads_back_the_transport_it_wrote` fails with `grok cannot
//!   read back its own output`. That guard is registry-driven and bidirectional
//!   — its `NO_NATIVE_SSE` list also fails an agent that starts refusing SSE
//!   without being listed — so a new dialect is covered the day it lands. The
//!   decisions table forces the other four answers, not this one.
//! - [`reject_mixed_transport`] — an entry carrying both families is refused,
//!   and the message is built from the keys the dialect ACTUALLY probes.
//! - [`remote_transport`] / [`missing_transport_error`] — the `url` → Sse /
//!   StreamableHttp split and the no-transport error.
//! - [`OwnedKeys`] + [`transport_fields`] — which keys a transport owns, and
//!   which key/value pairs to write, over the neutral [`FieldValue`].
//!
//! ## Why the parameters name FACTS, never dialects
//!
//! The previous shape passed a single `single_remote: bool`. One bit had to
//! answer four independent questions — is the tag spelled `type` or `transport`,
//! is SSE spellable at all, which remote keys appear in the mixed-entry message,
//! and is `streamable-http` readable — and it only fit because the two dialects
//! it was written for happened to answer all four the same way. The third
//! dialect broke the collinearity: `json_openclaw` writes `transport: "sse"` yet
//! passed `single_remote: true`, purely to borrow a message. `toml_mistral` and
//! `json_opencode` passed the same lie for the same reason. A parameter that
//! names a fact cannot be borrowed like that.
//!
//! This is deliberately NOT a `ConfigDoc` trait abstracting the whole document
//! (which would be a leaky seam hiding little). Strip loops, write order, phase
//! order (validate `enabled` → reject mixed families → dispatch on presence →
//! extract the chosen branch) and every emitter stay in the dialects, so error
//! behaviour on malformed input is byte-identical to before the extraction.
//!
//! The companion half is `crates/core/tests/mcp_dialect_decisions.rs`: a shared
//! function nobody is FORCED to call does not propagate — the sixth adversarial
//! review found the same half-parsed mixed entry in three dialects at once, all
//! of which could have called `reject_mixed_transport` since the first review.

use crate::errors::{ConfigError, Result};
use crate::models::McpTransport;
use std::collections::HashMap;

/// The words a dialect has for a remote transport.
///
/// An EMPTY string means "this dialect has no such word", which is a fact about
/// the vendor's format, not an instruction about where to `return Err`. Declare
/// what you can spell; the refusals follow.
pub struct RemoteVocabulary {
	/// The per-server key the remote transport is tagged with (`type`,
	/// `transport`). Empty when the dialect has no tag at all (Codex writes a
	/// bare `url`), in which case only [`refuse_unwritable`] applies.
	///
	/// [`refuse_unwritable`]: RemoteVocabulary::refuse_unwritable
	pub tag_key: &'static str,
	/// How this dialect spells SSE. EMPTY means it has no word for SSE, so
	/// aghub must refuse to write one instead of downgrading it to HTTP behind
	/// the user's back.
	pub sse: &'static str,
	/// How it spells streamable HTTP when it WRITES one. EMPTY means it writes
	/// a bare `url` with no tag at all (Grok, Hermes, Codex) — which is a fact
	/// about the format, not an omission.
	pub http: &'static str,
	/// Tag values READ as streamable HTTP, besides an absent tag. Only Hermes
	/// understands the spelled-out `streamable-http`; Grok would reject it.
	pub http_read_aliases: &'static [&'static str],
}

impl RemoteVocabulary {
	/// Whether the dialect can round-trip an SSE server. DERIVED from the
	/// spelling, never restated by the dialect — the same shape
	/// `json_map::Dialect::writes_sse` has used since the first review.
	pub const fn writes_sse(&self) -> bool {
		!self.sse.is_empty()
	}

	/// Refuse to write a transport this dialect has no word for.
	///
	/// The sentence lives here and nowhere else; `subject` is the caller's own
	/// noun phrase because the dialects quote server names differently
	/// (`MCP server 'x'` with single quotes, `` Mistral Vibe MCP server `x` ``
	/// with backticks and a vendor prefix). The condition is NOT the caller's to
	/// restate.
	pub fn refuse_unwritable(
		&self,
		transport: &McpTransport,
		subject: &str,
	) -> Result<()> {
		if matches!(transport, McpTransport::Sse { .. }) && !self.writes_sse() {
			return Err(ConfigError::InvalidConfig(format!(
				"{subject} uses SSE, which this agent's config format cannot \
				 express; use streamable HTTP instead"
			)));
		}
		Ok(())
	}
}

/// The per-server keys a transport owns, so a changed transport leaves no stale
/// key behind.
///
/// This is ONLY the key names. The strip LOOP stays in each dialect: Grok clears
/// both families before writing either (which is what clears an `env` that went
/// from `Some` to `None`, since [`transport_fields`] emits no `env` key then),
/// while Codex and Mistral clear the opposite family and then remove individual
/// fields. Folding those into one shared loop would need a new "strip both or
/// strip the other" flag — exactly the kind of collapsed bit this module exists
/// to remove.
pub struct OwnedKeys {
	pub stdio: &'static [&'static str],
	pub remote: &'static [&'static str],
}

impl OwnedKeys {
	/// Both families, stdio first — the strip set for a dialect that clears
	/// everything before writing the current transport.
	pub fn transport(&self) -> impl Iterator<Item = &'static str> + '_ {
		self.stdio.iter().chain(self.remote).copied()
	}
}

/// Reject a server that mixes stdio-family keys with remote-family keys.
///
/// `stdio_probe` / `remote_probe` are the keys the CALLER actually looks at, and
/// the message is built from them — so a dialect that only probes `command` and
/// `url` cannot claim in its error message to have checked `args`, `env` and
/// `headers` too. `present` is the dialect's own key-presence test, so this
/// runs before any field is type-extracted and a mixed entry is rejected
/// regardless of the field values (preserving the original error precedence).
///
/// A mixed entry must FAIL rather than half-parse: serialization rebuilds the
/// server from the parsed half, so "ignore the other one" DELETES it.
///
/// ## How wide to probe: PROBE ⊇ BLANKET STRIP, or own the normalisation
///
/// A dialect that strips BOTH families before writing either deletes every key
/// in its strip set, so anything in there it does not probe is a key it deletes
/// without ever having read one. Three shapes exist, and only the first pays
/// nothing for it:
///
/// - **Grok** strips `OwnedKeys::transport()` and probes those exact two lists.
///   Probe == strip, so widening either widens both — deliberately, since the
///   alternative is silent deletion. Pinned from both sides, by
///   `an_unowned_vendor_table_is_readable_and_survives_a_rewrite` (remote) and
///   `a_remote_entry_with_an_unowned_stdio_side_key_stays_readable` (stdio).
/// - **Hermes** strips the same way but leaves its TAG key out of the probe
///   (`REMOTE_PROBE`), so a vendor `transport: stdio` on a stdio entry stays
///   readable and is normalised away on the rewrite. One key, deliberate, and
///   pinned by `a_stdio_entry_that_spells_out_its_transport_tag_stays_readable`;
///   the probe/strip pair itself by
///   `a_remote_entry_with_an_unowned_stdio_side_key_stays_readable` (stdio) and
///   `serialize_preserves_per_server_extra_fields` (remote). Each pin covers
///   the unowned key it names, not every future one — the point is that a
///   widening edit cannot land without turning something red.
/// - **OpenClaw** strips both families (`TRANSPORT_KEYS`) while probing only
///   `command` × `url`, so an inert cross-family key — `headers` on a stdio
///   entry, `env` on a remote one — is dropped on the next save without ever
///   having been read. That ceiling predates this module and is NOT licensed by
///   the rule above; it is written down rather than papered over, because
///   closing it by widening the probe would refuse whole files that read fine
///   today. Widening `TRANSPORT_KEYS` further widens the loss, not the guard.
///
/// Codex, Mistral and OpenCode strip only the OPPOSITE family (OpenCode strips
/// nothing at all — serde's catch-all keeps what it never modelled), so their
/// `command` × `url` probe costs nothing: an inert leftover is simply kept.
///
/// Do NOT read any of this as "always probe everything". Every dialect's real
/// probe pair has a test in its own module that goes RED if the probe widens,
/// because widening refuses the WHOLE file — every one of that agent's MCP
/// servers — over a key that does nothing for the transport the entry declares.
pub fn reject_mixed_transport(
	stdio_probe: &[&str],
	remote_probe: &[&str],
	present: impl Fn(&str) -> bool,
	name: &str,
	dialect: &str,
) -> Result<()> {
	let has_stdio = stdio_probe.iter().any(|key| present(key));
	let has_remote = remote_probe.iter().any(|key| present(key));
	if has_stdio && has_remote {
		return Err(ConfigError::InvalidConfig(format!(
			"{dialect} MCP server `{name}` mixes stdio keys ({stdio}) with \
			 remote keys ({remote})",
			stdio = stdio_probe.join("/"),
			remote = remote_probe.join("/")
		)));
	}
	Ok(())
}

/// Build the remote transport for a `url`-based server. The dialect's own SSE
/// spelling selects SSE, a missing tag or one of its `http_read_aliases` selects
/// StreamableHttp, and anything else is an error naming the dialect's own tag
/// key.
pub fn remote_transport(
	url: String,
	headers: Option<HashMap<String, String>>,
	tag: Option<String>,
	vocab: &RemoteVocabulary,
	name: &str,
	dialect: &str,
) -> Result<McpTransport> {
	match tag.as_deref() {
		Some(tag) if vocab.writes_sse() && tag == vocab.sse => {
			Ok(McpTransport::Sse {
				url,
				headers,
				timeout: None,
			})
		}
		None => Ok(McpTransport::StreamableHttp {
			url,
			headers,
			timeout: None,
		}),
		Some(tag) if vocab.http_read_aliases.contains(&tag) => {
			Ok(McpTransport::StreamableHttp {
				url,
				headers,
				timeout: None,
			})
		}
		Some(other) => Err(ConfigError::InvalidConfig(format!(
			"{dialect} MCP server `{name}` has unknown `{key}` `{other}`",
			key = vocab.tag_key
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

/// The transport keys + values to WRITE for a server, in insertion order (which
/// Hermes' YAML output preserves verbatim — do not reorder these pushes). The
/// adapter converts each [`FieldValue`] to its native `Value` and inserts it.
/// `StreamableHttp` writes no tag in either dialect; `enabled` is written
/// separately by the adapter.
///
/// Callers must have run [`RemoteVocabulary::refuse_unwritable`] first: a
/// dialect with no SSE spelling has nothing to put in the tag.
pub fn transport_fields(
	transport: &McpTransport,
	vocab: &RemoteVocabulary,
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
			out.push((vocab.tag_key, FieldValue::Str(vocab.sse.to_string())));
			if let Some(headers) = headers {
				out.push(("headers", FieldValue::Map(headers.clone())));
			}
		}
		McpTransport::StreamableHttp { url, headers, .. } => {
			out.push(("url", FieldValue::Str(url.clone())));
			// A dialect with no word for streamable HTTP writes a bare `url`.
			// DERIVED, so a dialect that later gains one does not have to
			// remember to start emitting it here.
			if !vocab.http.is_empty() {
				out.push((
					vocab.tag_key,
					FieldValue::Str(vocab.http.to_string()),
				));
			}
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
	use crate::format::{toml_grok, yaml_hermes};

	const GROK_STDIO: &[&str] = &["command", "args", "env"];
	const GROK_REMOTE: &[&str] = &["url", "headers", "type"];
	const HERMES_REMOTE: &[&str] = &["url", "headers"];

	fn mixed(
		stdio: &[&str],
		remote: &[&str],
		present: &[&str],
		dialect: &str,
	) -> Result<()> {
		reject_mixed_transport(
			stdio,
			remote,
			|key| present.contains(&key),
			"m",
			dialect,
		)
	}

	#[test]
	fn reject_mixed_only_when_both_families_present() {
		assert!(mixed(GROK_STDIO, GROK_REMOTE, &["command", "url"], "Grok")
			.is_err());
		assert!(mixed(GROK_STDIO, GROK_REMOTE, &["command"], "Grok").is_ok());
		assert!(mixed(GROK_STDIO, GROK_REMOTE, &["url"], "Grok").is_ok());
		// The remote-family wording is DERIVED from the keys the dialect
		// probes, so it cannot claim to have checked a key it never read.
		let g = format!(
			"{:?}",
			mixed(GROK_STDIO, GROK_REMOTE, &["command", "url"], "Grok")
				.unwrap_err()
		);
		assert!(g.contains("url/headers/type"));
		let h = format!(
			"{:?}",
			mixed(GROK_STDIO, HERMES_REMOTE, &["command", "url"], "Hermes")
				.unwrap_err()
		);
		assert!(h.contains("url/headers)") && !h.contains("type"));
	}

	/// The vocabularies the dialects actually ship, borrowed through their own
	/// parsers rather than redeclared here — a constant declared in the test is
	/// how `gemini.rs` once lost `http_url_key` with every assertion still
	/// green.
	#[test]
	fn each_dialect_reads_its_own_remote_tag_spelling() {
		// Grok spells the tag `type`; Hermes spells it `transport`.
		assert!(matches!(
			toml_grok::parse("[mcp_servers.r]\nurl = \"u\"\ntype = \"sse\"\n")
				.unwrap()
				.mcps[0]
				.transport,
			McpTransport::Sse { .. }
		));
		assert!(matches!(
			yaml_hermes::parse(
				"mcp_servers:\n  r:\n    url: u\n    transport: sse\n"
			)
			.unwrap()
			.mcps[0]
				.transport,
			McpTransport::Sse { .. }
		));
		// An untagged remote is streamable HTTP in both.
		assert!(matches!(
			toml_grok::parse("[mcp_servers.r]\nurl = \"u\"\n")
				.unwrap()
				.mcps[0]
				.transport,
			McpTransport::StreamableHttp { .. }
		));
		assert!(matches!(
			yaml_hermes::parse("mcp_servers:\n  r:\n    url: u\n")
				.unwrap()
				.mcps[0]
				.transport,
			McpTransport::StreamableHttp { .. }
		));
		// Only Hermes lists `streamable-http` as a read alias.
		assert!(yaml_hermes::parse(
			"mcp_servers:\n  r:\n    url: u\n    transport: streamable-http\n"
		)
		.is_ok());
		assert!(toml_grok::parse(
			"[mcp_servers.r]\nurl = \"u\"\ntype = \"streamable-http\"\n"
		)
		.is_err());
	}

	#[test]
	fn unknown_tag_is_rejected_and_names_the_dialects_own_key() {
		let grok =
			toml_grok::parse("[mcp_servers.r]\nurl = \"u\"\ntype = \"grpc\"\n")
				.unwrap_err()
				.to_string();
		assert!(grok.contains("grpc") && grok.contains("type"), "{grok}");
		let hermes = yaml_hermes::parse(
			"mcp_servers:\n  r:\n    url: u\n    transport: grpc\n",
		)
		.unwrap_err()
		.to_string();
		assert!(
			hermes.contains("grpc") && hermes.contains("transport"),
			"{hermes}"
		);
	}

	#[test]
	fn refuse_unwritable_fires_only_where_sse_has_no_spelling() {
		let spells_sse = RemoteVocabulary {
			tag_key: "type",
			sse: "sse",
			http: "",
			http_read_aliases: &[],
		};
		let no_sse = RemoteVocabulary {
			tag_key: "type",
			sse: "",
			http: "",
			http_read_aliases: &[],
		};
		assert!(spells_sse.writes_sse());
		assert!(!no_sse.writes_sse());
		let sse = McpTransport::sse("u");
		assert!(spells_sse.refuse_unwritable(&sse, "MCP server 'x'").is_ok());
		let error = no_sse
			.refuse_unwritable(&sse, "MCP server 'x'")
			.unwrap_err()
			.to_string();
		assert!(
			error.ends_with(
				"MCP server 'x' uses SSE, which this agent's config format \
				 cannot express; use streamable HTTP instead"
			),
			"the subject is the caller's, the rest is ours: {error}"
		);
		// Nothing else is ever refused by vocabulary alone.
		for other in [
			McpTransport::stdio("c", vec![]),
			McpTransport::streamable_http("u"),
		] {
			assert!(no_sse.refuse_unwritable(&other, "MCP server 'x'").is_ok());
		}
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
	fn every_written_key_is_also_a_stripped_key() {
		let grok = RemoteVocabulary {
			tag_key: "type",
			sse: "sse",
			http: "",
			http_read_aliases: &["http"],
		};
		let hermes = RemoteVocabulary {
			tag_key: "transport",
			sse: "sse",
			http: "",
			http_read_aliases: &["http", "streamable-http"],
		};
		let owned = OwnedKeys {
			stdio: GROK_STDIO,
			remote: GROK_REMOTE,
		};
		let hermes_owned = OwnedKeys {
			stdio: GROK_STDIO,
			remote: &["url", "headers", "transport"],
		};
		assert_eq!(
			owned.transport().collect::<Vec<_>>(),
			["command", "args", "env", "url", "headers", "type"]
		);
		// Every key a dialect WRITES must also be one it STRIPS, or a changed
		// transport leaves the old tag behind.
		for (vocab, owned) in [(&grok, &owned), (&hermes, &hermes_owned)] {
			for transport in [
				McpTransport::stdio("c", vec![]),
				McpTransport::sse("u"),
				McpTransport::streamable_http("u"),
			] {
				for (key, _) in transport_fields(&transport, vocab) {
					assert!(
						owned.transport().any(|owned| owned == key),
						"`{key}` is written but never stripped"
					);
				}
			}
		}
	}

	#[test]
	fn serialize_fields_stdio_carry_exact_values_and_omit_absent_env() {
		let vocab = RemoteVocabulary {
			tag_key: "type",
			sse: "sse",
			http: "",
			http_read_aliases: &["http"],
		};
		let env = HashMap::from([("K".to_string(), "V".to_string())]);
		let stdio = McpTransport::Stdio {
			command: "mycmd".into(),
			args: vec!["--a".into(), "--b".into()],
			env: Some(env.clone()),
			timeout: None,
		};
		let fields = transport_fields(&stdio, &vocab);
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
			keys(&transport_fields(&no_env, &vocab)),
			["command", "args"],
			"absent env must not emit an `env` key"
		);
	}

	#[test]
	fn serialize_fields_sse_writes_each_dialects_own_tag() {
		let grok = RemoteVocabulary {
			tag_key: "type",
			sse: "sse",
			http: "",
			http_read_aliases: &["http"],
		};
		let hermes = RemoteVocabulary {
			tag_key: "transport",
			sse: "sse",
			http: "",
			http_read_aliases: &["http", "streamable-http"],
		};
		let headers = HashMap::from([("H".to_string(), "1".to_string())]);
		let sse = McpTransport::Sse {
			url: "https://x/sse".into(),
			headers: Some(headers.clone()),
			timeout: None,
		};
		let fields = transport_fields(&sse, &grok);
		assert_eq!(keys(&fields), ["url", "type", "headers"]);
		assert_eq!(str_of(&fields, "url"), "https://x/sse");
		assert_eq!(str_of(&fields, "type"), "sse");
		match fields.iter().find(|(k, _)| *k == "headers") {
			Some((_, FieldValue::Map(m))) => assert_eq!(m, &headers),
			_ => panic!("headers must carry the exact map"),
		}
		// Hermes spells the same tag `transport` — it must not be dropped, or
		// the server reads back as streamable HTTP.
		let fields = transport_fields(&sse, &hermes);
		assert_eq!(keys(&fields), ["url", "transport", "headers"]);
		assert_eq!(str_of(&fields, "transport"), "sse");
	}

	#[test]
	fn serialize_fields_http_is_url_only_and_omits_absent_headers() {
		let vocab = RemoteVocabulary {
			tag_key: "type",
			sse: "sse",
			http: "",
			http_read_aliases: &["http"],
		};
		let http = McpTransport::StreamableHttp {
			url: "u".into(),
			headers: None,
			timeout: None,
		};
		let fields = transport_fields(&http, &vocab);
		assert_eq!(
			keys(&fields),
			["url"],
			"StreamableHttp: url only — never type, no empty headers"
		);
		assert_eq!(str_of(&fields, "url"), "u");
	}
}
