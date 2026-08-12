use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};

// Antigravity spells the remote endpoint `serverUrl` and toggles with
// `disabled`; its explicit WebSocket option has no counterpart in the
// normalized model (rejected on read).
// The `type` tag stays even where the vendor docs only show it for stdio:
// dropping it makes SSE indistinguishable from streamable HTTP on the next
// read, and v2.13.3 already wrote it — removing it would strand every
// config that release produced.
json_map_dialect!(json_map::Dialect {
	discriminator: Some(json_map::Discriminator {
		key: "type",
		stdio: "stdio",
		sse: "sse",
		http: "http",
	}),
	url_key: "serverUrl",
	toggle_key: json_map::ToggleKey::Disabled,
	..json_map::MCP_SERVERS
});

define_mcp_paths! {
	global: ".gemini/config/mcp_config.json",
	project: ".agents/mcp_config.json",
	data_dir: ".gemini/antigravity",
	strategy: parse_mcp_config, serialize_mcp_config,
}

define_skill_paths! {
	global: ".gemini/antigravity/skills",
	project: ".agents/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "antigravity",
	display_name: "Antigravity",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
	load_mcps,
	save_mcps,
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: Some(mcp_project_path),
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: true,
			},
			universal: false,
		},
		mcp: McpCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: true,
			},
			stdio: true,
			remote: true,
			enable_disable: true,
		},
		sub_agents: SubAgentCapabilities {
			scopes: ScopeSupport {
				global: false,
				project: false,
			},
		},
	},
	global_skill_paths: Some(GlobalSkillPaths {
		read: global_skills_paths,
		write: global_skill_write_path,
	}),
	project_skill_paths: Some(ProjectSkillPaths {
		read: project_skills_paths,
		write: project_skill_write_path,
	}),
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	cli_name: "antigravity",
	validate_args: &["--version"],
	project_markers: &[".agents/mcp_config.json"],
	skills_cli_name: Some("antigravity"),
};

#[cfg(test)]
mod tests {
	use super::*;

	// Compile-time pin: the capability the tests below rely on.
	const _: () = {
		assert!(DESCRIPTOR.capabilities.mcp.remote);
		assert!(DESCRIPTOR.capabilities.mcp.enable_disable);
	};
	use crate::{AgentConfig, McpServer, McpTransport};
	use std::path::Path;

	#[test]
	fn antigravity_mcp_paths_match_runtime() {
		let home = home_dir().expect("home directory should resolve");
		assert_eq!(
			(DESCRIPTOR.mcp_global_path.unwrap())(),
			Some(home.join(".gemini/config/mcp_config.json"))
		);
		assert_eq!(
			(DESCRIPTOR.mcp_project_path.unwrap())(Path::new("/workspace")),
			Some(Path::new("/workspace/.agents/mcp_config.json").to_path_buf())
		);
		assert_eq!(DESCRIPTOR.project_markers, &[".agents/mcp_config.json"]);
	}

	#[test]
	fn antigravity_uses_server_url_and_keeps_the_two_remotes_apart() {
		let mut config = AgentConfig::new();
		config.mcps = vec![
			McpServer::new(
				"api",
				McpTransport::streamable_http("https://example.test/mcp"),
			),
			// Deliberately NOT a `/sse` path: without the `type` tag this
			// would read back as streamable HTTP.
			McpServer::new(
				"events",
				McpTransport::sse("https://example.test/v1/messages"),
			),
		];

		let output = (DESCRIPTOR.mcp_serialize_config.unwrap())(&config, None)
			.expect("Antigravity MCP config should serialize");
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(
			value["mcpServers"]["api"]["serverUrl"],
			"https://example.test/mcp"
		);
		assert!(value["mcpServers"]["api"].get("url").is_none());

		let reparsed = (DESCRIPTOR.mcp_parse_config.unwrap())(&output).unwrap();
		let events = reparsed.mcps.iter().find(|m| m.name == "events").unwrap();
		assert!(
			matches!(events.transport, McpTransport::Sse { .. }),
			"SSE must survive the round trip, got {:?}",
			events.transport
		);
	}

	#[test]
	fn antigravity_roundtrip_preserves_disabled_and_unmanaged_fields() {
		let original = r#"{
			"theme": "dark",
			"mcpServers": {
				"local": {
					"command": "uvx",
					"args": ["example-server"],
					"env": {"TOKEN": "secret"},
					"cwd": "/workspace/service",
					"auth": {"audience": "example"},
					"disabled": true,
					"disabledTools": ["dangerous"]
				}
			}
		}"#;
		let parse = DESCRIPTOR.mcp_parse_config.unwrap();
		let config =
			parse(original).expect("Antigravity MCP config should parse");
		assert_eq!(config.mcps.len(), 1);
		assert!(!config.mcps[0].enabled);
		assert!(matches!(
			&config.mcps[0].transport,
			McpTransport::Stdio { command, args, env, .. }
				if command == "uvx"
					&& args == &["example-server"]
					&& env.as_ref().and_then(|env| env.get("TOKEN"))
						.map(String::as_str)
						== Some("secret")
		));

		let output =
			(DESCRIPTOR.mcp_serialize_config.unwrap())(&config, Some(original))
				.expect("Antigravity MCP config should serialize");
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		let local = &value["mcpServers"]["local"];

		assert_eq!(local["disabled"], true);
		assert_eq!(local["cwd"], "/workspace/service");
		assert_eq!(local["auth"]["audience"], "example");
		assert_eq!(local["disabledTools"][0], "dangerous");
		assert_eq!(value["theme"], "dark");
	}
}
