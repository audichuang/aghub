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

/// The agents whose native config has exactly ONE remote shape, so SSE has no
/// spelling there and writing it is refused. This list is EXHAUSTIVE and
/// asserted in both directions: an agent that starts refusing SSE without being
/// added here fails, and an agent listed here that quietly starts accepting it
/// fails too. Without that, "any error counts as a deliberate refusal" would let
/// a real regression pass as a skip.
const NO_NATIVE_SSE: &[&str] = &["codex", "opencode", "kilocode", "mistral"];

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

fn write(
	descriptor: &AgentDescriptor,
	server: &McpServer,
) -> aghub_agents::Result<String> {
	let serialize = descriptor.mcp_serialize_config.expect("serializer");
	serialize(&config_with(server.clone()), None)
}

fn read_back(descriptor: &AgentDescriptor, written: &str) -> McpServer {
	let parse = descriptor.mcp_parse_config.expect("parser");
	let config = parse(written).unwrap_or_else(|error| {
		panic!(
			"{} cannot read back its own output: {error}\n--- written ---\n{written}",
			descriptor.id
		)
	});
	config
		.mcps
		.into_iter()
		.find(|mcp| mcp.name == "probe")
		.unwrap_or_else(|| {
			panic!("{} dropped the server it just wrote", descriptor.id)
		})
}

fn agents_with_mcp() -> impl Iterator<Item = &'static AgentDescriptor> {
	registry::iter_all().filter(|descriptor| {
		descriptor.mcp_parse_config.is_some()
			&& descriptor.mcp_serialize_config.is_some()
	})
}

#[test]
fn every_agent_reads_back_the_transport_it_wrote() {
	for descriptor in agents_with_mcp() {
		let refuses_sse = NO_NATIVE_SSE.contains(&descriptor.id);
		for transport in [
			McpTransport::stdio("echo", vec!["--flag".to_string()]),
			// Deliberately NOT a `/sse` URL: a dialect that dropped the
			// transport tag used to survive this check only because the path
			// happened to spell "sse".
			McpTransport::sse("https://example.com/v1/messages"),
			McpTransport::streamable_http("https://example.com/v1/mcp"),
		] {
			let expected = transport_kind(&transport);
			let server = McpServer::new("probe", transport);
			let written = write(descriptor, &server);

			if expected == "sse" && refuses_sse {
				let error = written.expect_err(&format!(
					"{} is listed as having no native SSE but accepted one",
					descriptor.id
				));
				assert!(
					error.to_string().contains("cannot express"),
					"{} must say WHY it refused: {error}",
					descriptor.id
				);
				continue;
			}

			let written = written.unwrap_or_else(|error| {
				panic!("{} refused a {expected} server: {error}", descriptor.id)
			});
			let parsed = read_back(descriptor, &written);
			assert_eq!(
				transport_kind(&parsed.transport),
				expected,
				"{} silently rewrote a {expected} server as {}\n--- written ---\n{written}",
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
}

#[test]
fn the_no_native_sse_list_names_only_real_agents() {
	for id in NO_NATIVE_SSE {
		assert!(
			registry::iter_all().any(|descriptor| descriptor.id == *id),
			"`{id}` is not a registered agent — stale entry"
		);
	}
}

#[test]
fn an_agent_that_advertises_a_toggle_must_round_trip_a_disabled_server() {
	let mut checked = 0;
	for descriptor in agents_with_mcp() {
		if !descriptor.capabilities.mcp.enable_disable {
			continue;
		}
		let mut server =
			McpServer::new("probe", McpTransport::stdio("echo", vec![]));
		server.enabled = false;
		let written = write(descriptor, &server).unwrap_or_else(|error| {
			panic!(
				"{} advertises enable_disable but refused: {error}",
				descriptor.id
			)
		});
		let parsed = read_back(descriptor, &written);
		assert!(
			!parsed.enabled,
			"{} advertises enable_disable but read the server back as enabled\n--- written ---\n{written}",
			descriptor.id
		);
		checked += 1;
	}
	assert!(checked >= 8, "expected the toggle agents, got {checked}");
}

/// The disabled state has to agree with the DIALECT, not with the capability
/// bit: `enable_disable` gates the explicit enable/disable command (Factory,
/// for one, has a native `disabled` field but writes project toggles at user
/// scope, so aghub does not offer the command). What must never happen is a
/// writer and reader that disagree — writing `disabled` and reading back
/// enabled silently turns a server the user switched off back on.
#[test]
fn a_disabled_server_is_either_written_as_disabled_or_not_at_all() {
	for descriptor in agents_with_mcp() {
		let mut server =
			McpServer::new("probe", McpTransport::stdio("echo", vec![]));
		server.enabled = false;
		let written = write(descriptor, &server).unwrap_or_else(|error| {
			panic!("{} refused a stdio server: {error}", descriptor.id)
		});
		let parse = descriptor.mcp_parse_config.unwrap();
		let parsed = parse(&written).unwrap_or_else(|error| {
			panic!("{} cannot read back its own output: {error}", descriptor.id)
		});
		match parsed.mcps.iter().find(|mcp| mcp.name == "probe") {
			None => {} // omitted: the dialect has nowhere to put the state
			Some(mcp) => assert!(
				!mcp.enabled,
				"{} wrote a disabled server and read it back as ENABLED\n--- written ---\n{written}",
				descriptor.id
			),
		}
	}
}

#[test]
fn remote_capability_matches_what_the_dialect_can_actually_write() {
	for descriptor in agents_with_mcp() {
		let server = McpServer::new(
			"probe",
			McpTransport::streamable_http("https://example.com/v1/mcp"),
		);
		let wrote_http = write(descriptor, &server).is_ok();
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

/// The preflight that batches rely on must agree with the writer for every
/// agent and every transport — that agreement is what keeps a multi-agent
/// mutation from writing half its targets and then failing on the rest.
#[test]
fn transport_support_check_agrees_with_the_serializer() {
	for descriptor in agents_with_mcp() {
		for transport in [
			McpTransport::stdio("echo", vec![]),
			McpTransport::sse("https://example.com/v1/messages"),
			McpTransport::streamable_http("https://example.com/v1/mcp"),
		] {
			let server = McpServer::new("probe", transport.clone());
			assert_eq!(
				aghub_agents::descriptor::supports_mcp_transport(
					descriptor, &transport
				),
				write(descriptor, &server).is_ok(),
				"{}: preflight and writer disagree about {}",
				descriptor.id,
				transport_kind(&transport)
			);
		}
	}
}
