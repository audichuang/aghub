//! Every agent's MCP writer must be readable by its own reader.
//!
//! This is the guard for a whole class of silent corruption: a dialect whose
//! serializer drops the transport tag (or the on/off field) writes a file that
//! its own parser reads back as something ELSE, so an unrelated `aghub mcp add`
//! quietly rewrites the user's SSE server as streamable HTTP, or re-enables a
//! server they disabled. Refusing to write is fine — changing the meaning is
//! not. Driven off the registry, so a new agent is covered the day it lands.

use aghub_agents::{AgentConfig, AgentDescriptor, McpServer, McpTransport};
use aghub_core::registry;

fn config_with(server: McpServer) -> AgentConfig {
	AgentConfig {
		mcps: vec![server],
		skills: vec![],
		sub_agents: vec![],
	}
}

fn transport_kind(transport: &McpTransport) -> &'static str {
	match transport {
		McpTransport::Stdio { .. } => "stdio",
		McpTransport::Sse { .. } => "sse",
		McpTransport::StreamableHttp { .. } => "streamable-http",
	}
}

/// `Ok(Some(parsed))` when the dialect wrote the server, `Ok(None)` when it
/// deliberately refused, `Err` when it failed in a way that is not a refusal.
fn round_trip(
	descriptor: &AgentDescriptor,
	server: &McpServer,
) -> Result<Option<McpServer>, String> {
	let (Some(parse), Some(serialize)) =
		(descriptor.mcp_parse_config, descriptor.mcp_serialize_config)
	else {
		return Ok(None);
	};
	let written = match serialize(&config_with(server.clone()), None) {
		Ok(written) => written,
		Err(error) => {
			let message = error.to_string();
			// A refusal must SAY it cannot represent the value; anything else
			// is a bug hiding behind an error type.
			if message.contains("cannot express")
				|| message.contains("unsupported")
			{
				return Ok(None);
			}
			return Err(format!(
				"{} failed to serialize: {message}",
				descriptor.id
			));
		}
	};
	let parsed = parse(&written).map_err(|error| {
		format!(
			"{} cannot read back its own output: {error}\n--- written ---\n{written}",
			descriptor.id
		)
	})?;
	Ok(parsed.mcps.into_iter().find(|mcp| mcp.name == server.name))
}

#[test]
fn every_agent_reads_back_the_transport_it_wrote() {
	let mut checked = 0;
	for descriptor in registry::iter_all() {
		for transport in [
			McpTransport::stdio("echo", vec!["--flag".to_string()]),
			// Deliberately NOT a `/sse` URL: a dialect that dropped the
			// transport tag used to survive this test only because the path
			// happened to spell "sse".
			McpTransport::sse("https://example.com/v1/messages"),
			McpTransport::streamable_http("https://example.com/v1/mcp"),
		] {
			let expected = transport_kind(&transport);
			let server = McpServer::new("probe", transport);
			let parsed = round_trip(descriptor, &server).unwrap();
			let Some(parsed) = parsed else {
				continue; // no MCP support, or an explicit refusal
			};
			checked += 1;
			assert_eq!(
				transport_kind(&parsed.transport),
				expected,
				"{} silently rewrote a {expected} server as {}",
				descriptor.id,
				transport_kind(&parsed.transport)
			);
			assert!(
				parsed.enabled,
				"{} lost the enabled state of a live server",
				descriptor.id
			);
		}
	}
	assert!(
		checked > 20,
		"expected broad coverage, only checked {checked}"
	);
}

#[test]
fn an_agent_that_advertises_a_toggle_must_round_trip_a_disabled_server() {
	for descriptor in registry::iter_all() {
		if !descriptor.capabilities.mcp.enable_disable {
			continue;
		}
		let mut server =
			McpServer::new("probe", McpTransport::stdio("echo", vec![]));
		server.enabled = false;
		let parsed =
			round_trip(descriptor, &server).unwrap().unwrap_or_else(|| {
				panic!(
				"{} advertises enable_disable but dropped a disabled server",
				descriptor.id
			)
			});
		assert!(
			!parsed.enabled,
			"{} advertises enable_disable but read the server back as enabled",
			descriptor.id
		);
	}
}

#[test]
fn remote_capability_matches_what_the_dialect_can_actually_write() {
	for descriptor in registry::iter_all() {
		if descriptor.mcp_serialize_config.is_none() {
			continue;
		}
		let server = McpServer::new(
			"probe",
			McpTransport::streamable_http("https://example.com/v1/mcp"),
		);
		let wrote_http = round_trip(descriptor, &server).unwrap().is_some();
		assert_eq!(
			wrote_http,
			descriptor.capabilities.mcp.remote,
			"{} advertises remote={} but writing streamable HTTP {}",
			descriptor.id,
			descriptor.capabilities.mcp.remote,
			if wrote_http {
				"succeeded"
			} else {
				"was refused"
			}
		);
	}
}
