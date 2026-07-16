//! Cross-dialect CONTRACT test for the strict preserve-and-merge MCP dialects
//! (Grok TOML, Hermes YAML). Both delegate their transport semantics to
//! `format::transport_policy`; this harness pins the shared invariants against
//! BOTH dialects so neither can drift silently (the harness `format/mod.rs`
//! asks for). The per-dialect unit tests remain the fine-grained coverage.
//!
//! It covers, for each dialect: stdio dispatch + `enabled` default, a KEPT
//! disabled server, a REMOTE server (and the `type` split — Grok writes
//! `type="sse"`, single-remote Hermes omits it), preserve-and-merge of an
//! unrelated top-level key + an unowned per-server field, round-trip stability,
//! and the mixed-key rejection PRECEDENCE (rejected before field type errors).

use aghub_agents::format::{toml_grok, yaml_hermes};
use aghub_agents::models::{AgentConfig, McpTransport};

/// Invariants on a parsed config with `keep-on` (stdio, default enabled),
/// `keep-off` (`enabled: false`), and `remote` (a url-based server).
fn assert_parsed_invariants(cfg: &AgentConfig, dialect: &str) {
	let on = cfg
		.mcps
		.iter()
		.find(|m| m.name == "keep-on")
		.unwrap_or_else(|| panic!("{dialect}: keep-on server missing"));
	assert!(
		on.enabled,
		"{dialect}: missing `enabled` must default to true"
	);
	// Assert the command + args VALUES survived, not just the variant.
	match &on.transport {
		McpTransport::Stdio { command, args, .. } => {
			assert_eq!(command, "cmd", "{dialect}: keep-on command value");
			assert_eq!(
				args,
				&["--x".to_string()],
				"{dialect}: keep-on args value"
			);
		}
		_ => panic!("{dialect}: keep-on `command` must map to Stdio"),
	}
	let off = cfg
		.mcps
		.iter()
		.find(|m| m.name == "keep-off")
		.unwrap_or_else(|| panic!("{dialect}: keep-off server missing"));
	assert!(
		!off.enabled,
		"{dialect}: a disabled server must be kept (enabled=false), not dropped"
	);
	assert!(
		cfg.mcps.iter().any(|m| m.name == "remote"),
		"{dialect}: remote server missing"
	);
}

/// The `url` value of a server, whatever remote variant it parsed to.
fn remote_url<'a>(cfg: &'a AgentConfig, name: &str) -> &'a str {
	match &cfg.mcps.iter().find(|m| m.name == name).unwrap().transport {
		McpTransport::Sse { url, .. }
		| McpTransport::StreamableHttp { url, .. } => url,
		McpTransport::Stdio { .. } => panic!("{name} is not a remote server"),
	}
}

#[test]
fn grok_honors_the_strict_dialect_contract() {
	let valid = r#"
unrelated_top = "keep-me"

[mcp_servers.keep-on]
command = "cmd"
args = ["--x"]
note = "unowned"

[mcp_servers.keep-off]
command = "other"
enabled = false

[mcp_servers.remote]
url = "https://x/sse"
type = "sse"
"#;

	let cfg = toml_grok::parse(valid).expect("grok: valid config parses");
	assert_parsed_invariants(&cfg, "grok");
	// Grok keeps the SSE/HTTP distinction.
	let remote = cfg.mcps.iter().find(|m| m.name == "remote").unwrap();
	assert!(
		matches!(remote.transport, McpTransport::Sse { .. }),
		"grok: `type=sse` must parse to Sse"
	);
	assert_eq!(
		remote_url(&cfg, "remote"),
		"https://x/sse",
		"grok: remote url value"
	);

	// Mixed keys are rejected, and BEFORE any field type error — a server that
	// is both mixed AND malformed must report the mixed-key error.
	let mixed_and_malformed = r#"
[mcp_servers.bad]
command = 5
url = "https://y"
"#;
	let err = toml_grok::parse(mixed_and_malformed).unwrap_err();
	assert!(
		format!("{err:?}").contains("mixes stdio keys"),
		"grok: mixed-key rejection must precede field type errors, got {err:?}"
	);

	let out = toml_grok::serialize(&cfg, Some(valid)).expect("grok serializes");
	assert!(
		out.contains("unrelated_top"),
		"grok: an unrelated top-level key must survive serialize"
	);
	assert!(
		out.contains("note"),
		"grok: an unowned per-server field must survive serialize"
	);
	assert!(
		out.contains("type = \"sse\""),
		"grok: an Sse server must serialize `type = \"sse\"`"
	);
	// Round-trip is stable and keeps the Sse distinction.
	let reparsed = toml_grok::parse(&out).expect("grok re-parses");
	assert_parsed_invariants(&reparsed, "grok");
	assert!(matches!(
		reparsed
			.mcps
			.iter()
			.find(|m| m.name == "remote")
			.unwrap()
			.transport,
		McpTransport::Sse { .. }
	));
	assert_eq!(
		remote_url(&reparsed, "remote"),
		"https://x/sse",
		"grok: remote url must survive round-trip"
	);
}

#[test]
fn hermes_honors_the_strict_dialect_contract() {
	let valid = r#"
unrelated_top: keep-me
mcp_servers:
  keep-on:
    command: cmd
    args: ["--x"]
    note: unowned
  keep-off:
    command: other
    enabled: false
  remote:
    url: https://x/mcp
"#;

	let cfg = yaml_hermes::parse(valid).expect("hermes: valid config parses");
	assert_parsed_invariants(&cfg, "hermes");
	// Hermes has a single remote transport.
	let remote = cfg.mcps.iter().find(|m| m.name == "remote").unwrap();
	assert!(
		matches!(remote.transport, McpTransport::StreamableHttp { .. }),
		"hermes: a url server must be StreamableHttp"
	);
	assert_eq!(
		remote_url(&cfg, "remote"),
		"https://x/mcp",
		"hermes: remote url value"
	);

	let mixed_and_malformed = r#"
mcp_servers:
  bad:
    command: 5
    url: https://y
"#;
	let err = yaml_hermes::parse(mixed_and_malformed).unwrap_err();
	assert!(
		format!("{err:?}").contains("mixes stdio keys"),
		"hermes: mixed-key rejection must precede field type errors, got {err:?}"
	);

	let out =
		yaml_hermes::serialize(&cfg, Some(valid)).expect("hermes serializes");
	assert!(
		out.contains("unrelated_top"),
		"hermes: an unrelated top-level key must survive serialize"
	);
	assert!(
		out.contains("note"),
		"hermes: an unowned per-server field must survive serialize"
	);
	assert!(
		!out.contains("type:"),
		"hermes: a single-remote dialect must never write a `type` key, got:\n{out}"
	);
	let reparsed = yaml_hermes::parse(&out).expect("hermes re-parses");
	assert_parsed_invariants(&reparsed, "hermes");
	assert_eq!(
		remote_url(&reparsed, "remote"),
		"https://x/mcp",
		"hermes: remote url must survive round-trip"
	);
}
