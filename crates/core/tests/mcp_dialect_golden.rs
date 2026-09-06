//! What each agent actually writes to disk, and what it makes of a file it did
//! not write. One row per agent, driven off the registry.
//!
//! The round-trip suite next door cannot see any of this: it feeds an agent its
//! OWN output, so a dialect can rename every key it writes — `context_servers`
//! → `mcpServers`, `serverUrl` → `url`, `streamable-http` → `http` — and still
//! read itself back perfectly while the vendor's editor sees an empty config.
//! Worse, the format-level tests used to assert against dialect constants
//! DECLARED IN THE TEST FILE, so `gemini.rs` once lost `http_url_key` entirely
//! with every "gemini" assertion still green.
//!
//! So: the expectations here are text, and they are produced by
//! `descriptor.mcp_serialize_config` / `mcp_parse_config` — the exact function
//! pointers the descriptor ships. Nothing in this file can be satisfied by a
//! dialect that only the test knows about.
//!
//! `read_input` carries what serializing can never show. aghub always writes a
//! transport tag, so `untyped_remote` (how an UNTAGGED remote is read) and
//! `legacy_url_keys` (older spellings still read and stripped) are invisible to
//! the write goldens; deleting either is a data-loss regression that only a
//! parse of a foreign document catches.
//!
//! ## Changing a golden
//!
//! An edit here is an edit to a user's config file. If a diff appears you did
//! not intend, that is the bug — do not bless it. If you did intend it, name
//! the agent and the field in the commit message.

use aghub_agents::{
	AgentConfig, AgentDescriptor, McpServer, McpTransport, Result,
};
use aghub_core::registry;
use std::collections::{BTreeMap, HashMap};

struct Golden {
	id: &'static str,
	/// `serialize(CANONICAL, None)`, JSON normalised (see [`normalize`]).
	write: &'static str,
	/// `serialize(<one SSE server>, None)` — an agent with no SSE spelling
	/// must REFUSE, never silently downgrade.
	sse: &'static str,
	/// A config aghub did not write: untagged remotes, older URL spellings,
	/// both toggle fields in conflict.
	read_input: &'static str,
	/// [`render`] of parsing `read_input`.
	read: &'static str,
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// One env var and one header ON PURPOSE: `HashMap` iteration order is random
/// per process, so a second entry would make these goldens flap.
fn one(key: &str, value: &str) -> Option<HashMap<String, String>> {
	Some(HashMap::from([(key.to_string(), value.to_string())]))
}

fn canonical() -> AgentConfig {
	let mut off = McpServer::new(
		"off",
		McpTransport::Stdio {
			command: "run-off".into(),
			args: vec![],
			env: None,
			timeout: None,
		},
	);
	off.enabled = false;
	AgentConfig {
		mcps: vec![
			McpServer::new(
				"local",
				McpTransport::Stdio {
					command: "run-local".into(),
					args: vec!["--flag".into(), "value".into()],
					env: one("TOKEN", "secret"),
					timeout: None,
				},
			),
			McpServer::new(
				"remote",
				McpTransport::StreamableHttp {
					url: "https://example.test/mcp".into(),
					headers: one("Authorization", "Bearer t"),
					timeout: None,
				},
			),
			off,
		],
		skills: vec![],
		sub_agents: vec![],
	}
}

fn sse_only() -> AgentConfig {
	AgentConfig {
		mcps: vec![McpServer::new(
			"stream",
			McpTransport::Sse {
				url: "https://example.test/sse".into(),
				headers: None,
				timeout: None,
			},
		)],
		skills: vec![],
		sub_agents: vec![],
	}
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Sort every object key so the two `json_opencode` agents, whose serializer
/// emits servers in `HashMap` order, do not flap. Key NAMES and values — the
/// things a dialect controls — all survive; only their order is dropped.
fn sorted(value: serde_json::Value) -> serde_json::Value {
	match value {
		serde_json::Value::Object(map) => serde_json::Value::Object(
			map.into_iter()
				.map(|(key, value)| (key, sorted(value)))
				.collect::<BTreeMap<_, _>>()
				.into_iter()
				.collect(),
		),
		serde_json::Value::Array(items) => {
			serde_json::Value::Array(items.into_iter().map(sorted).collect())
		}
		other => other,
	}
}

fn normalize(text: &str) -> String {
	match serde_json::from_str::<serde_json::Value>(text) {
		Ok(value) => {
			serde_json::to_string_pretty(&sorted(value)).expect("re-emit json")
		}
		// TOML and YAML serializers already emit in a stable order.
		Err(_) => text.trim_end().to_string(),
	}
}

fn show(result: Result<String>) -> String {
	match result {
		Ok(text) => normalize(&text),
		Err(error) => format!("ERROR: {error}"),
	}
}

/// One line per server: name, transport, what it points at, on/off. `enabled`
/// is here because a toggle field the parser stops reading is otherwise
/// invisible on this side.
fn render(config: Result<AgentConfig>) -> String {
	let config = match config {
		Ok(config) => config,
		Err(error) => return format!("ERROR: {error}"),
	};
	let mut lines: Vec<String> = config
		.mcps
		.iter()
		.map(|mcp| {
			let (kind, target) = match &mcp.transport {
				McpTransport::Stdio { command, args, .. } => {
					("stdio", format!("{command} {}", args.join(" ")))
				}
				McpTransport::Sse { url, .. } => ("sse", url.clone()),
				McpTransport::StreamableHttp { url, .. } => {
					("streamable-http", url.clone())
				}
			};
			format!(
				"{} {kind} {} enabled={}",
				mcp.name,
				target.trim_end(),
				mcp.enabled
			)
		})
		.collect();
	lines.sort();
	lines.join("\n")
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// `untagged-plain` / `untagged-sse` pin `untyped_remote`: with
/// `InferSseFromUrl` the `/sse/` path becomes SSE, with `StreamableHttp` it
/// does not. `http-key` pins Gemini's `http_url_key`, which outranks both `url`
/// and the path heuristic — and is FOREIGN to every other agent, so nobody else
/// may read it. `url` on an agent whose own spelling is `serverUrl` pins
/// `legacy_url_keys`: drop them and the entry has no URL at all.
/// `toggled-on` / `toggled-off` carry BOTH fields in conflict, so the three
/// `ToggleKey` values produce three different answers.
const MAP_READ: &str = r#"{
  "mcpServers": {
    "untagged-plain": { "url": "https://example.test/plain" },
    "untagged-sse": { "url": "https://example.test/sse/stream" },
    "http-key": {
      "url": "https://example.test/sse/legacy",
      "httpUrl": "https://example.test/preferred"
    },
    "toggled-on": { "command": "run-on", "disabled": true, "enabled": true },
    "toggled-off": { "command": "run-off", "disabled": false, "enabled": false }
  }
}"#;

/// Same body under Zed's own key. Spelled out rather than composed so the
/// fixture reads as the file Zed would actually find on disk.
const ZED_READ: &str = r#"{
  "context_servers": {
    "untagged-plain": { "url": "https://example.test/plain" },
    "untagged-sse": { "url": "https://example.test/sse/stream" },
    "http-key": {
      "url": "https://example.test/sse/legacy",
      "httpUrl": "https://example.test/preferred"
    },
    "toggled-on": { "command": "run-on", "disabled": true, "enabled": true },
    "toggled-off": { "command": "run-off", "disabled": false, "enabled": false }
  }
}"#;

const AMP_READ: &str = r#"{
  "amp": {
    "mcpServers": {
      "untagged-plain": { "url": "https://example.test/plain" },
      "untagged-sse": { "url": "https://example.test/sse/stream" },
      "http-key": {
        "url": "https://example.test/sse/legacy",
        "httpUrl": "https://example.test/preferred"
      },
      "toggled-on": { "command": "run-on", "disabled": true, "enabled": true },
      "toggled-off": {
        "command": "run-off",
        "disabled": false,
        "enabled": false
      }
    }
  }
}"#;

const OPENCLAW_READ: &str = r#"{
  "mcp": {
    "servers": {
      "untagged-plain": { "url": "https://example.test/plain" },
      "untagged-sse": { "url": "https://example.test/sse/stream" },
      "toggled-off": { "command": "run-off", "enabled": false }
    }
  }
}"#;

const OPENCODE_READ: &str = r#"{
  "mcp": {
    "untagged-plain": { "type": "remote", "url": "https://example.test/plain" },
    "untagged-sse": { "type": "remote", "url": "https://example.test/sse/stream" },
    "toggled-off": { "type": "local", "command": ["run-off"], "enabled": false }
  }
}"#;

const CODEX_READ: &str = r#"[mcp_servers.untagged-plain]
url = "https://example.test/plain"

[mcp_servers.untagged-sse]
url = "https://example.test/sse/stream"

[mcp_servers.toggled-off]
command = "run-off"
enabled = false
"#;

const GROK_READ: &str = r#"[mcp_servers.untagged-plain]
url = "https://example.test/plain"

[mcp_servers.tagged-sse]
type = "sse"
url = "https://example.test/sse/stream"

[mcp_servers.toggled-off]
command = "run-off"
enabled = false
"#;

const MISTRAL_READ: &str = r#"[[mcp_servers]]
name = "untagged-plain"
url = "https://example.test/plain"

[[mcp_servers]]
name = "toggled-off"
command = "run-off"
disabled = true
"#;

const HERMES_READ: &str = r#"mcp_servers:
  untagged-plain:
    url: https://example.test/plain
  toggled-off:
    command: run-off
    enabled: false
"#;

// ── The table ────────────────────────────────────────────────────────────────

macro_rules! row {
	($id:literal, $read_input:expr, $write:expr, $sse:expr, $read:expr) => {
		Golden {
			id: $id,
			write: $write,
			sse: $sse,
			read_input: $read_input,
			read: $read,
		}
	};
}

/// `mcpServers` + `type: stdio|http`, no toggle: the default dialect, verbatim.
///
/// Shared by: claude, gemini, copilot, cursor, trae, augmentcode, warp.
const WRITE_TYPE_TAGGED: &str = r#"{
  "mcpServers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "env": {
        "TOKEN": "secret"
      },
      "type": "stdio"
    },
    "remote": {
      "headers": {
        "Authorization": "Bearer t"
      },
      "type": "http",
      "url": "https://example.test/mcp"
    }
  }
}"#;

/// `mcp` map, `type: local|remote`, command as an argv array, native `enabled`.
///
/// Shared by: opencode, kilocode.
const WRITE_OPENCODE: &str = r#"{
  "mcp": {
    "local": {
      "command": [
        "run-local",
        "--flag",
        "value"
      ],
      "enabled": true,
      "environment": {
        "TOKEN": "secret"
      },
      "type": "local"
    },
    "off": {
      "command": [
        "run-off"
      ],
      "enabled": false,
      "type": "local"
    },
    "remote": {
      "enabled": true,
      "headers": {
        "Authorization": "Bearer t"
      },
      "type": "remote",
      "url": "https://example.test/mcp"
    }
  }
}"#;

/// The default dialect plus a persisted `disabled` field, so the off server survives the write.
///
/// Shared by: kiro, factory.
const WRITE_TYPE_TAGGED_TOGGLE: &str = r#"{
  "mcpServers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "disabled": false,
      "env": {
        "TOKEN": "secret"
      },
      "type": "stdio"
    },
    "off": {
      "command": "run-off",
      "disabled": true,
      "type": "stdio"
    },
    "remote": {
      "disabled": false,
      "headers": {
        "Authorization": "Bearer t"
      },
      "type": "http",
      "url": "https://example.test/mcp"
    }
  }
}"#;

/// `type: sse` — the native spelling.
///
/// Shared by: claude, gemini, copilot, cursor, trae, augmentcode, warp.
const SSE_TYPE_TAGGED: &str = r#"{
  "mcpServers": {
    "stream": {
      "type": "sse",
      "url": "https://example.test/sse"
    }
  }
}"#;

/// No SSE spelling, so the write is REFUSED. Silently downgrading it to HTTP would rewrite the user's server behind their back.
///
/// Shared by: codex, opencode, kilocode.
const SSE_REFUSED: &str = r#"ERROR: Invalid configuration: MCP server 'stream' uses SSE, which this agent's config format cannot express; use streamable HTTP instead"#;

/// `type: sse` alongside the persisted toggle.
///
/// Shared by: cline, kiro, roocode, factory.
const SSE_TYPE_TAGGED_TOGGLE: &str = r#"{
  "mcpServers": {
    "stream": {
      "disabled": false,
      "type": "sse",
      "url": "https://example.test/sse"
    }
  }
}"#;

/// No toggle field, and an untagged `/sse/` URL is read as SSE. `httpUrl` is foreign here, so `url` wins on the `http-key` entry.
///
/// Shared by: claude, cursor, windsurf, trae, zed, augmentcode, warp.
const READ_INFERS_SSE: &str = r#"http-key sse https://example.test/sse/legacy enabled=true
toggled-off stdio run-off enabled=true
toggled-on stdio run-on enabled=true
untagged-plain streamable-http https://example.test/plain enabled=true
untagged-sse sse https://example.test/sse/stream enabled=true"#;

/// One remote shape, so nothing infers SSE; the native on/off field is honoured.
///
/// Shared by: codex, openclaw, opencode, kilocode.
const READ_HTTP_ONLY_TOGGLE: &str = r#"toggled-off stdio run-off enabled=false
untagged-plain streamable-http https://example.test/plain enabled=true
untagged-sse streamable-http https://example.test/sse/stream enabled=true"#;

/// As above, but `disabled` is the dialect's own field, so it outranks the stale `enabled` beside it — `toggled-on` reads as OFF.
///
/// Shared by: cline, antigravity, kiro, roocode, amp, factory.
const READ_INFERS_SSE_TOGGLE: &str = r#"http-key sse https://example.test/sse/legacy enabled=true
toggled-off stdio run-off enabled=true
toggled-on stdio run-on enabled=false
untagged-plain streamable-http https://example.test/plain enabled=true
untagged-sse sse https://example.test/sse/stream enabled=true"#;

/// `untyped_remote: StreamableHttp` — an untagged remote is HTTP even when the path says `/sse/`. Deleting that field is invisible to every write.
///
/// Shared by: copilot, kimi.
const READ_ALWAYS_HTTP: &str = r#"http-key streamable-http https://example.test/sse/legacy enabled=true
toggled-off stdio run-off enabled=true
toggled-on stdio run-on enabled=true
untagged-plain streamable-http https://example.test/plain enabled=true
untagged-sse streamable-http https://example.test/sse/stream enabled=true"#;

const GOLDENS: &[Golden] = &[
	row!(
		"claude",
		MAP_READ,
		WRITE_TYPE_TAGGED,
		SSE_TYPE_TAGGED,
		READ_INFERS_SSE
	),
	row!(
		"codex",
		CODEX_READ,
		r#"[mcp_servers.local]
args = [
    "--flag",
    "value",
]
command = "run-local"

[mcp_servers.local.env]
TOKEN = "secret"

[mcp_servers.off]
command = "run-off"
enabled = false

[mcp_servers.remote]
url = "https://example.test/mcp"

[mcp_servers.remote.http_headers]
Authorization = "Bearer t""#,
		SSE_REFUSED,
		READ_HTTP_ONLY_TOGGLE
	),
	row!(
		"openclaw",
		OPENCLAW_READ,
		r#"{
  "mcp": {
    "servers": {
      "local": {
        "args": [
          "--flag",
          "value"
        ],
        "command": "run-local",
        "enabled": true,
        "env": {
          "TOKEN": "secret"
        },
        "transport": "stdio"
      },
      "off": {
        "command": "run-off",
        "enabled": false,
        "transport": "stdio"
      },
      "remote": {
        "enabled": true,
        "headers": {
          "Authorization": "Bearer t"
        },
        "transport": "streamable-http",
        "url": "https://example.test/mcp"
      }
    }
  }
}"#,
		r#"{
  "mcp": {
    "servers": {
      "stream": {
        "enabled": true,
        "transport": "sse",
        "url": "https://example.test/sse"
      }
    }
  }
}"#,
		READ_HTTP_ONLY_TOGGLE
	),
	row!(
		"opencode",
		OPENCODE_READ,
		WRITE_OPENCODE,
		SSE_REFUSED,
		READ_HTTP_ONLY_TOGGLE
	),
	row!(
		"gemini",
		MAP_READ,
		WRITE_TYPE_TAGGED,
		SSE_TYPE_TAGGED,
		r#"http-key streamable-http https://example.test/preferred enabled=true
toggled-off stdio run-off enabled=true
toggled-on stdio run-on enabled=true
untagged-plain streamable-http https://example.test/plain enabled=true
untagged-sse sse https://example.test/sse/stream enabled=true"#
	),
	row!(
		"cline",
		MAP_READ,
		r#"{
  "mcpServers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "disabled": false,
      "env": {
        "TOKEN": "secret"
      },
      "type": "stdio"
    },
    "off": {
      "command": "run-off",
      "disabled": true,
      "type": "stdio"
    },
    "remote": {
      "disabled": false,
      "headers": {
        "Authorization": "Bearer t"
      },
      "type": "streamableHttp",
      "url": "https://example.test/mcp"
    }
  }
}"#,
		SSE_TYPE_TAGGED_TOGGLE,
		READ_INFERS_SSE_TOGGLE
	),
	row!(
		"copilot",
		MAP_READ,
		WRITE_TYPE_TAGGED,
		SSE_TYPE_TAGGED,
		READ_ALWAYS_HTTP
	),
	row!(
		"cursor",
		MAP_READ,
		WRITE_TYPE_TAGGED,
		SSE_TYPE_TAGGED,
		READ_INFERS_SSE
	),
	row!(
		"antigravity",
		MAP_READ,
		r#"{
  "mcpServers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "disabled": false,
      "env": {
        "TOKEN": "secret"
      },
      "type": "stdio"
    },
    "off": {
      "command": "run-off",
      "disabled": true,
      "type": "stdio"
    },
    "remote": {
      "disabled": false,
      "headers": {
        "Authorization": "Bearer t"
      },
      "serverUrl": "https://example.test/mcp",
      "type": "http"
    }
  }
}"#,
		r#"{
  "mcpServers": {
    "stream": {
      "disabled": false,
      "serverUrl": "https://example.test/sse",
      "type": "sse"
    }
  }
}"#,
		READ_INFERS_SSE_TOGGLE
	),
	row!(
		"kiro",
		MAP_READ,
		WRITE_TYPE_TAGGED_TOGGLE,
		SSE_TYPE_TAGGED_TOGGLE,
		READ_INFERS_SSE_TOGGLE
	),
	row!(
		"windsurf",
		MAP_READ,
		r#"{
  "mcpServers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "env": {
        "TOKEN": "secret"
      },
      "type": "stdio"
    },
    "remote": {
      "headers": {
        "Authorization": "Bearer t"
      },
      "serverUrl": "https://example.test/mcp",
      "type": "http"
    }
  }
}"#,
		r#"{
  "mcpServers": {
    "stream": {
      "serverUrl": "https://example.test/sse",
      "type": "sse"
    }
  }
}"#,
		READ_INFERS_SSE
	),
	row!(
		"trae",
		MAP_READ,
		WRITE_TYPE_TAGGED,
		SSE_TYPE_TAGGED,
		READ_INFERS_SSE
	),
	row!(
		"zed",
		ZED_READ,
		r#"{
  "context_servers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "env": {
        "TOKEN": "secret"
      },
      "type": "stdio"
    },
    "remote": {
      "headers": {
        "Authorization": "Bearer t"
      },
      "type": "http",
      "url": "https://example.test/mcp"
    }
  }
}"#,
		r#"{
  "context_servers": {
    "stream": {
      "type": "sse",
      "url": "https://example.test/sse"
    }
  }
}"#,
		READ_INFERS_SSE
	),
	row!(
		"roocode",
		MAP_READ,
		r#"{
  "mcpServers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "disabled": false,
      "env": {
        "TOKEN": "secret"
      },
      "type": "stdio"
    },
    "off": {
      "command": "run-off",
      "disabled": true,
      "type": "stdio"
    },
    "remote": {
      "disabled": false,
      "headers": {
        "Authorization": "Bearer t"
      },
      "type": "streamable-http",
      "url": "https://example.test/mcp"
    }
  }
}"#,
		SSE_TYPE_TAGGED_TOGGLE,
		READ_INFERS_SSE_TOGGLE
	),
	row!(
		"kimi",
		MAP_READ,
		r#"{
  "mcpServers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "env": {
        "TOKEN": "secret"
      },
      "transport": "stdio"
    },
    "remote": {
      "headers": {
        "Authorization": "Bearer t"
      },
      "transport": "http",
      "url": "https://example.test/mcp"
    }
  }
}"#,
		r#"{
  "mcpServers": {
    "stream": {
      "transport": "sse",
      "url": "https://example.test/sse"
    }
  }
}"#,
		READ_ALWAYS_HTTP
	),
	row!(
		"mistral",
		MISTRAL_READ,
		r#"[[mcp_servers]]
args = ["--flag", "value"]
command = "run-local"
disabled = false
name = "local"
transport = "stdio"

[mcp_servers.env]
TOKEN = "secret"

[[mcp_servers]]
disabled = false
name = "remote"
transport = "streamable-http"
url = "https://example.test/mcp"

[mcp_servers.auth]
type = "static"

[mcp_servers.auth.headers]
Authorization = "Bearer t"

[[mcp_servers]]
command = "run-off"
disabled = true
name = "off"
transport = "stdio""#,
		r#"ERROR: Invalid configuration: Mistral Vibe MCP server `stream` uses SSE, which this agent's config format cannot express; use streamable HTTP instead"#,
		r#"ERROR: Invalid configuration: Mistral Vibe MCP server `untagged-plain` field `transport` must be a string"#
	),
	row!(
		"augmentcode",
		MAP_READ,
		WRITE_TYPE_TAGGED,
		SSE_TYPE_TAGGED,
		READ_INFERS_SSE
	),
	row!(
		"kilocode",
		OPENCODE_READ,
		WRITE_OPENCODE,
		SSE_REFUSED,
		READ_HTTP_ONLY_TOGGLE
	),
	row!(
		"amp",
		AMP_READ,
		r#"{
  "amp": {
    "mcpServers": {
      "local": {
        "args": [
          "--flag",
          "value"
        ],
        "command": "run-local",
        "disabled": false,
        "env": {
          "TOKEN": "secret"
        }
      },
      "off": {
        "command": "run-off",
        "disabled": true
      },
      "remote": {
        "disabled": false,
        "headers": {
          "Authorization": "Bearer t"
        },
        "transport": "http",
        "url": "https://example.test/mcp"
      }
    }
  }
}"#,
		r#"{
  "amp": {
    "mcpServers": {
      "stream": {
        "disabled": false,
        "transport": "sse",
        "url": "https://example.test/sse"
      }
    }
  }
}"#,
		READ_INFERS_SSE_TOGGLE
	),
	row!(
		"factory",
		MAP_READ,
		WRITE_TYPE_TAGGED_TOGGLE,
		SSE_TYPE_TAGGED_TOGGLE,
		READ_INFERS_SSE_TOGGLE
	),
	row!(
		"warp",
		MAP_READ,
		WRITE_TYPE_TAGGED,
		SSE_TYPE_TAGGED,
		READ_INFERS_SSE
	),
	row!(
		"hermes",
		HERMES_READ,
		r#"mcp_servers:
  local:
    command: run-local
    args:
    - --flag
    - value
    env:
      TOKEN: secret
    enabled: true
  remote:
    url: https://example.test/mcp
    headers:
      Authorization: Bearer t
    enabled: true
  off:
    command: run-off
    args: []
    enabled: false"#,
		r#"mcp_servers:
  stream:
    url: https://example.test/sse
    transport: sse
    enabled: true"#,
		r#"toggled-off stdio run-off enabled=false
untagged-plain streamable-http https://example.test/plain enabled=true"#
	),
	row!(
		"grok",
		GROK_READ,
		r#"[mcp_servers.local]
args = ["--flag", "value"]
command = "run-local"
enabled = true

[mcp_servers.local.env]
TOKEN = "secret"

[mcp_servers.off]
args = []
command = "run-off"
enabled = false

[mcp_servers.remote]
enabled = true
url = "https://example.test/mcp"

[mcp_servers.remote.headers]
Authorization = "Bearer t""#,
		r#"[mcp_servers.stream]
enabled = true
type = "sse"
url = "https://example.test/sse""#,
		r#"tagged-sse sse https://example.test/sse/stream enabled=true
toggled-off stdio run-off enabled=false
untagged-plain streamable-http https://example.test/plain enabled=true"#
	),
	// omp is the FIRST `ToggleKey::Enabled` agent in `json_map`, and the only
	// one whose tag is `transport`. Every shared const above therefore spells
	// something omp does not, so it gets its own three — derived from the
	// vendor contract, NOT from running the serializer:
	//   "transport" in e && (e.transport === "stdio" | "sse" | "http")
	//   e.transport ?? (e.command ? "stdio" : e.url ? "http" : "stdio")
	// The `off` server MUST survive as `enabled: false`; a no-toggle dialect
	// drops it entirely, which would silently remount a server the user
	// switched off the next time aghub saved.
	row!(
		"omp",
		MAP_READ,
		r#"{
  "mcpServers": {
    "local": {
      "args": [
        "--flag",
        "value"
      ],
      "command": "run-local",
      "enabled": true,
      "env": {
        "TOKEN": "secret"
      },
      "transport": "stdio"
    },
    "off": {
      "command": "run-off",
      "enabled": false,
      "transport": "stdio"
    },
    "remote": {
      "enabled": true,
      "headers": {
        "Authorization": "Bearer t"
      },
      "transport": "http",
      "url": "https://example.test/mcp"
    }
  }
}"#,
		r#"{
  "mcpServers": {
    "stream": {
      "enabled": true,
      "transport": "sse",
      "url": "https://example.test/sse"
    }
  }
}"#,
		// `enabled` is read and `disabled` ignored, so `toggled-off` (which
		// carries BOTH, in conflict) is OFF and `toggled-on` is ON — the
		// opposite of what a `ToggleKey::Disabled` agent makes of the same file.
		r#"http-key streamable-http https://example.test/sse/legacy enabled=true
toggled-off stdio run-off enabled=false
toggled-on stdio run-on enabled=true
untagged-plain streamable-http https://example.test/plain enabled=true
untagged-sse streamable-http https://example.test/sse/stream enabled=true"#
	),
];

// ── Assertions ───────────────────────────────────────────────────────────────

/// Same filter as `mcp_dialect_roundtrip`: an agent that CLAIMS a transport
/// must have a golden, whatever fn pointers it happens to hold.
fn agents_with_mcp() -> impl Iterator<Item = &'static AgentDescriptor> {
	registry::iter_all().filter(|descriptor| {
		descriptor.capabilities.mcp.stdio || descriptor.capabilities.mcp.remote
	})
}

fn golden(id: &str) -> Option<&'static Golden> {
	GOLDENS.iter().find(|row| row.id == id)
}

#[test]
fn every_mcp_agent_has_a_golden_and_no_row_is_stale() {
	for descriptor in agents_with_mcp() {
		assert!(
			golden(descriptor.id).is_some(),
			"'{}' claims MCP support but has no golden row — a new agent is \
			 not covered until someone writes down what it puts on disk",
			descriptor.id
		);
	}
	for row in GOLDENS {
		assert!(
			agents_with_mcp().any(|descriptor| descriptor.id == row.id),
			"golden row '{}' names no MCP-capable agent",
			row.id
		);
	}
	let mut seen = std::collections::BTreeSet::new();
	for row in GOLDENS {
		assert!(seen.insert(row.id), "duplicate golden row for '{}'", row.id);
	}
}

fn report(agent: &str, what: &str, expected: &str, actual: &str) -> String {
	format!(
		"\n=== {agent} :: {what} ===\n--- golden ---\n{expected}\n--- actual \
		 ---\n{actual}"
	)
}

#[test]
fn each_agent_writes_exactly_what_its_vendor_reads() {
	let mut wrong = Vec::new();
	for descriptor in agents_with_mcp() {
		let Some(row) = golden(descriptor.id) else {
			continue;
		};
		let serialize =
			descriptor.mcp_serialize_config.expect("claimed serializer");
		let write = show(serialize(&canonical(), None));
		if write != row.write {
			wrong.push(report(descriptor.id, "write", row.write, &write));
		}
		let sse = show(serialize(&sse_only(), None));
		if sse != row.sse {
			wrong.push(report(descriptor.id, "sse", row.sse, &sse));
		}
	}
	assert!(
		wrong.is_empty(),
		"{} golden(s) broke. This text is what lands in the user's config \
		 file — a renamed key or a changed transport tag means the vendor's \
		 own editor stops seeing the server. Do not bless a diff you did not \
		 intend.{}",
		wrong.len(),
		wrong.concat()
	);
}

#[test]
fn each_agent_reads_a_document_it_did_not_write() {
	let mut wrong = Vec::new();
	for descriptor in agents_with_mcp() {
		let Some(row) = golden(descriptor.id) else {
			continue;
		};
		let parse = descriptor.mcp_parse_config.expect("claimed parser");
		let read = render(parse(row.read_input));
		if read != row.read {
			wrong.push(report(descriptor.id, "read", row.read, &read));
		}
	}
	assert!(
		wrong.is_empty(),
		"{} golden(s) broke. This is the ONLY guard for untyped_remote, \
		 legacy_url_keys and http_url_key — none of them can be seen from the \
		 write side, and losing one silently drops or re-points a server the \
		 user already had.{}",
		wrong.len(),
		wrong.concat()
	);
}
