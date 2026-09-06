//! The five questions every MCP dialect has to answer, answered side by side.
//!
//! ## What this file pins, and what it does NOT
//!
//! It pins the CATEGORY of each answer — refuse / preserve / fall back — never
//! the sentence. Two spellings of the mixed-entry refusal already coexist
//! (`cannot contain both command and url` in `json_map`, `mixes stdio keys` in
//! the strict dialects), so an assertion on wording would be an assertion on
//! which dialect happens to own the entry. THE BYTES ARE `mcp_dialect_golden`'S
//! JOB, ALWAYS. Verbatim messages belong there and in each module's own tests.
//!
//! ## Why it exists
//!
//! `reject_mixed_transport` has been in `format/` since the first adversarial
//! review, and the sixth review still found the same half-parsed mixed entry in
//! THREE dialects at once (`json_opencode`, `json_openclaw`, `toml_mistral`,
//! fixed by hand in each). A shared function nobody is forced to call does not
//! propagate. This table is the forcing function: it is driven off
//! `registry::iter_all()`, so a new agent has no row until someone writes down
//! what it does with a mixed entry, an unknown transport tag, a field the model
//! does not own, a value that does not fit, and an SSE server it cannot spell.
//!
//! A `NotApplicable` cell is allowed and must be paired with an absent fixture,
//! so "the dialect has no such concept" cannot be used to dodge a real answer.

use aghub_agents::{
	AgentConfig, AgentDescriptor, McpServer, McpTransport, Result,
};
use aghub_core::registry;

// ── The vocabulary ───────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Answer {
	/// The document is refused outright.
	Rejected,
	/// Refused, AND the message names the field that did not fit, so the user
	/// can find it. The payload is that field's name.
	RejectedNaming(&'static str),
	/// A field the model does not own survives a parse → serialize rewrite,
	/// STILL ATTACHED to the server that carried it.
	PreservedVerbatim,
	/// The text survived the rewrite but is no longer part of the server entry
	/// (it floated up to the document root, or landed on a different server).
	/// A cross-agent copy would leave it behind, and deleting the server would
	/// not delete it — so it is not "preserved" in any sense a user benefits
	/// from. No row may claim this — enforced by
	/// `no_row_may_declare_a_known_defect_shape`.
	NotAttachedToServer,
	/// The tag is ignored and the transport is inferred from which keys are
	/// present. Not a refusal: the user's declared tag is silently replaced on
	/// the next save. `json_map` does this; the hand-written dialects refuse.
	PresenceFallback,
	/// The entry was HALF-READ: something that carried transport meaning went
	/// nowhere, so the next save deletes it (the writers rebuild every server
	/// from the parsed half). No row may claim this — enforced by
	/// `no_row_may_declare_a_known_defect_shape`. It is the shape of the defect
	/// the sixth review found in three dialects at once, kept as a distinct
	/// observation so a regression says WHICH way the dialect went soft instead
	/// of just "not Rejected".
	DropsRemote,
	/// KNOWN-DEFECT SHAPE no row may claim (enforced by
	/// `no_row_may_declare_a_known_defect_shape`): the value was read without
	/// refusing, so it may have been silently rounded to fit. A `0.5`
	/// timeout read back as `1` is what once made `mcp_fit` call a lossy copy
	/// Exact, and `reconcile` then deleted the source holding the real value.
	Approximated,
	/// The dialect has a native word for this, so there is nothing to refuse.
	Spelled,
	/// The dialect has no such concept at all; the matching fixture is absent.
	NotApplicable,
}

/// One document per question, in the dialect's own syntax. Every entry carries
/// exactly ONE server, ONE env var and ONE header: `HashMap` iteration order is
/// random per process, and a second of anything makes these flap.
struct Fixtures {
	/// One entry carrying a stdio key AND a remote key, in the canonical
	/// `command` + `url` shape.
	///
	/// CEILING, stated so the `Rejected` column is not read as more than it is:
	/// each dialect probes only the keys IT names. Grok probes every key its
	/// serializer strips; Hermes does too except its tag key. Codex, Mistral
	/// and OpenCode probe `command` × `url` and strip only the opposite family
	/// (OpenCode strips nothing), so a stdio entry carrying an inert
	/// remote-family key — Codex `http_headers`, Mistral `api_key_env` — is
	/// kept, not refused. OpenClaw is the outlier: it probes `command` × `url`
	/// but blanket-strips BOTH families, so an inert `headers` on a stdio entry
	/// is silently dropped on the next save. That is a pre-existing ceiling
	/// recorded in `mcp_policy`'s probe doc, NOT something this column blesses.
	/// Widening any probe would refuse the WHOLE file over a key that does
	/// nothing for the transport the entry declares, so each dialect's real
	/// probe pair is pinned in its own module test instead.
	mixed: &'static str,
	/// One remote entry tagged with a transport word this dialect never wrote.
	unknown_tag: Option<&'static str>,
	/// One entry carrying `keep-me = "vendor-owned"`, which no model owns.
	unmodeled: &'static str,
	/// One entry whose timeout cannot be represented by the dialect's model.
	unfittable: Option<&'static str>,
}

// ── Fixtures, one set per structural family ─────────────────────────────────

const MAP: Fixtures = Fixtures {
	mixed: r#"{"mcpServers":{"m":{"command":"run","url":"https://example.test/mcp"}}}"#,
	unknown_tag: Some(
		r#"{"mcpServers":{"t":{"type":"grpc","url":"https://example.test/mcp"}}}"#,
	),
	unmodeled: r#"{"mcpServers":{"u":{"command":"run","keep-me":"vendor-owned"}}}"#,
	unfittable: None,
};

const ZED: Fixtures = Fixtures {
	mixed: r#"{"context_servers":{"m":{"command":"run","url":"https://example.test/mcp"}}}"#,
	unknown_tag: Some(
		r#"{"context_servers":{"t":{"type":"grpc","url":"https://example.test/mcp"}}}"#,
	),
	unmodeled: r#"{"context_servers":{"u":{"command":"run","keep-me":"vendor-owned"}}}"#,
	unfittable: None,
};

const AMP: Fixtures = Fixtures {
	mixed: r#"{"amp":{"mcpServers":{"m":{"command":"run","url":"https://example.test/mcp"}}}}"#,
	unknown_tag: Some(
		r#"{"amp":{"mcpServers":{"t":{"type":"grpc","url":"https://example.test/mcp"}}}}"#,
	),
	unmodeled: r#"{"amp":{"mcpServers":{"u":{"command":"run","keep-me":"vendor-owned"}}}}"#,
	unfittable: None,
};

const OPENCLAW: Fixtures = Fixtures {
	mixed: r#"{"mcp":{"servers":{"m":{"command":"run","url":"https://example.test/mcp"}}}}"#,
	unknown_tag: Some(
		r#"{"mcp":{"servers":{"t":{"transport":"grpc","url":"https://example.test/mcp"}}}}"#,
	),
	unmodeled: r#"{"mcp":{"servers":{"u":{"command":"run","keep-me":"vendor-owned"}}}}"#,
	unfittable: Some(
		r#"{"mcp":{"servers":{"v":{"url":"https://example.test/mcp","timeout":0.5}}}}"#,
	),
};

const OPENCODE: Fixtures = Fixtures {
	mixed: r#"{"mcp":{"m":{"command":["run"],"url":"https://example.test/mcp"}}}"#,
	unknown_tag: Some(
		r#"{"mcp":{"t":{"type":"grpc","url":"https://example.test/mcp"}}}"#,
	),
	unmodeled: r#"{"mcp":{"u":{"type":"local","command":["run"],"keep-me":"vendor-owned"}}}"#,
	unfittable: Some(
		r#"{"mcp":{"v":{"type":"remote","url":"https://example.test/mcp","timeout":0.5}}}"#,
	),
};

const CODEX: Fixtures = Fixtures {
	mixed: "[mcp_servers.m]\ncommand = \"run\"\nurl = \"https://example.test/mcp\"\n",
	unknown_tag: None,
	unmodeled: "[mcp_servers.u]\ncommand = \"run\"\nkeep-me = \"vendor-owned\"\n",
	unfittable: Some(
		"[mcp_servers.v]\ncommand = \"run\"\ntool_timeout_sec = 0.5\n",
	),
};

const GROK: Fixtures = Fixtures {
	mixed: "[mcp_servers.m]\ncommand = \"run\"\nurl = \"https://example.test/mcp\"\n",
	unknown_tag: Some(
		"[mcp_servers.t]\ntype = \"grpc\"\nurl = \"https://example.test/mcp\"\n",
	),
	unmodeled: "[mcp_servers.u]\ncommand = \"run\"\nkeep-me = \"vendor-owned\"\n",
	unfittable: None,
};

const MISTRAL: Fixtures = Fixtures {
	mixed: "[[mcp_servers]]\nname = \"m\"\ntransport = \"stdio\"\ncommand = \"run\"\nurl = \"https://example.test/mcp\"\n",
	unknown_tag: Some(
		"[[mcp_servers]]\nname = \"t\"\ntransport = \"grpc\"\nurl = \"https://example.test/mcp\"\n",
	),
	unmodeled: "[[mcp_servers]]\nname = \"u\"\ntransport = \"stdio\"\ncommand = \"run\"\nkeep-me = \"vendor-owned\"\n",
	unfittable: Some(
		"[[mcp_servers]]\nname = \"v\"\ntransport = \"stdio\"\ncommand = \"run\"\ntool_timeout_sec = -1\n",
	),
};

const HERMES: Fixtures = Fixtures {
	mixed: "mcp_servers:\n  m:\n    command: run\n    url: https://example.test/mcp\n",
	unknown_tag: Some(
		"mcp_servers:\n  t:\n    transport: grpc\n    url: https://example.test/mcp\n",
	),
	unmodeled: "mcp_servers:\n  u:\n    command: run\n    keep-me: vendor-owned\n",
	unfittable: None,
};

// ── The table ────────────────────────────────────────────────────────────────

struct Row {
	id: &'static str,
	fixtures: &'static Fixtures,
	mixed: Answer,
	unknown_tag: Answer,
	unmodeled: Answer,
	unfittable: Answer,
	unwritable_sse: Answer,
}

macro_rules! row {
	($id:literal, $fx:expr, $mixed:expr, $tag:expr, $unmodeled:expr, $unfit:expr, $sse:expr) => {
		Row {
			id: $id,
			fixtures: &$fx,
			mixed: $mixed,
			unknown_tag: $tag,
			unmodeled: $unmodeled,
			unfittable: $unfit,
			unwritable_sse: $sse,
		}
	};
}

use Answer::*;

const ROWS: &[Row] = &[
	// The `json_map` family: 16 agents, ONE parser, ONE serializer. Its unknown
	// tag answer is the odd one out — see `PresenceFallback`.
	row!(
		"claude",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"gemini",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"copilot",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"cursor",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"windsurf",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"trae",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"augmentcode",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"warp",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"cline",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"kiro",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"roocode",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"factory",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"kimi",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"antigravity",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"zed",
		ZED,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"amp",
		AMP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	// The hand-written dialects, one engine each.
	row!(
		"codex",
		CODEX,
		Rejected,
		NotApplicable,
		PreservedVerbatim,
		RejectedNaming("tool_timeout_sec"),
		Rejected
	),
	row!(
		"grok",
		GROK,
		Rejected,
		Rejected,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"hermes",
		HERMES,
		Rejected,
		Rejected,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
	row!(
		"mistral",
		MISTRAL,
		Rejected,
		Rejected,
		PreservedVerbatim,
		RejectedNaming("tool_timeout_sec"),
		Rejected
	),
	row!(
		"openclaw",
		OPENCLAW,
		Rejected,
		Rejected,
		PreservedVerbatim,
		RejectedNaming("timeout"),
		Spelled
	),
	// These two share `json_opencode`. Its `is_remote` used to recognise only
	// `type: "remote"`, so every other tag fell into the stdio branch with a
	// `None` command and the `url` was lost — the next save wrote
	// `command: [""]` over it. Refusing the tag is what the audit asks for
	// ("a server aghub cannot read is a server the next save DELETES").
	row!(
		"opencode",
		OPENCODE,
		Rejected,
		Rejected,
		PreservedVerbatim,
		Rejected,
		Rejected
	),
	row!(
		"kilocode",
		OPENCODE,
		Rejected,
		Rejected,
		PreservedVerbatim,
		Rejected,
		Rejected
	),
	// omp introduces no new dialect — it is `json_map` with a `transport` tag
	// and a native `enabled` toggle — and still owes a row: the requirement is
	// registry-driven precisely so "it's just the shared parser" cannot be the
	// reason nobody wrote down what it does.
	row!(
		"omp",
		MAP,
		Rejected,
		PresenceFallback,
		PreservedVerbatim,
		NotApplicable,
		Spelled
	),
];

// ── Probes ───────────────────────────────────────────────────────────────────

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

fn observe_mixed(parsed: Result<AgentConfig>) -> Answer {
	match parsed {
		Err(_) => Rejected,
		// Whatever it produced, it half-read the entry: the serializer rebuilds
		// the server from the parsed half, so the other half is now deleted.
		Ok(_) => DropsRemote,
	}
}

fn observe_unknown_tag(parsed: Result<AgentConfig>) -> Answer {
	let Ok(config) = parsed else {
		return Rejected;
	};
	match config.mcps.first().map(|mcp| &mcp.transport) {
		Some(McpTransport::Sse { .. })
		| Some(McpTransport::StreamableHttp { .. }) => PresenceFallback,
		// The url went nowhere: read as stdio, so the next save deletes it.
		Some(McpTransport::Stdio { .. }) | None => DropsRemote,
	}
}

/// `PreservedVerbatim` has to mean "still on THAT SERVER", and a whole-document
/// substring match cannot tell that from a field that ended up at the document
/// root — aghub's model has no room for either, so both survive the rewrite,
/// yet only the attached one travels with the server or dies with it.
///
/// The discriminator is to hand the dialect an EMPTY config over the rewritten
/// text: every dialect rebuilds its server container from the config it was
/// given (verified for all nine families), so a field inside the server entry
/// disappears with the entry and a detached one does not.
fn observe_unmodeled(
	descriptor: &AgentDescriptor,
	fixture: &str,
) -> (Answer, String) {
	let parse = descriptor.mcp_parse_config.expect("claimed parser");
	let serialize =
		descriptor.mcp_serialize_config.expect("claimed serializer");
	let rewritten =
		match parse(fixture).and_then(|c| serialize(&c, Some(fixture))) {
			Ok(text) => text,
			Err(error) => return (Rejected, format!("refused: {error}")),
		};
	if !(rewritten.contains("keep-me") && rewritten.contains("vendor-owned")) {
		return (DropsRemote, rewritten);
	}
	// A failure here is a harness problem, not a dialect answer: dropping every
	// server is the same code path `aghub mcp remove` takes. Panic loudly
	// rather than let it read as "preserved".
	let dropped = serialize(&AgentConfig::new(), Some(&rewritten))
		.unwrap_or_else(|error| {
			panic!(
				"'{}': dropping every server must still serialize: {error}",
				descriptor.id
			)
		});
	let answer = if dropped.contains("keep-me") {
		NotAttachedToServer
	} else {
		PreservedVerbatim
	};
	(
		answer,
		format!("{rewritten}\n--- with every server dropped ---\n{dropped}"),
	)
}

fn observe_unfittable(parsed: Result<AgentConfig>, expected: Answer) -> Answer {
	match parsed {
		Ok(_) => Approximated,
		Err(error) => match expected {
			// The claim is "the message names the field". Check exactly that,
			// and nothing about how the sentence is phrased.
			RejectedNaming(field) if error.to_string().contains(field) => {
				RejectedNaming(field)
			}
			_ => Rejected,
		},
	}
}

// ── Assertions ───────────────────────────────────────────────────────────────

/// Same filter as `mcp_dialect_golden` and `mcp_dialect_roundtrip`: an agent
/// that CLAIMS a transport owes an answer, whatever fn pointers it holds.
fn agents_with_mcp() -> impl Iterator<Item = &'static AgentDescriptor> {
	registry::iter_all().filter(|descriptor| {
		descriptor.capabilities.mcp.stdio || descriptor.capabilities.mcp.remote
	})
}

fn find(id: &str) -> Option<&'static Row> {
	ROWS.iter().find(|row| row.id == id)
}

#[test]
fn every_mcp_agent_answers_all_five_questions() {
	for descriptor in agents_with_mcp() {
		assert!(
			find(descriptor.id).is_some(),
			"'{}' claims MCP support but answers none of the five dialect \
			 questions. A shared helper nobody is forced to call is how the \
			 same mixed-entry defect reached three dialects at once — write \
			 the row.",
			descriptor.id
		);
	}
	for row in ROWS {
		assert!(
			agents_with_mcp().any(|descriptor| descriptor.id == row.id),
			"row '{}' names no MCP-capable agent",
			row.id
		);
	}
	let mut seen = std::collections::BTreeSet::new();
	for row in ROWS {
		assert!(seen.insert(row.id), "duplicate row for '{}'", row.id);
	}
}

#[test]
fn not_applicable_always_means_the_dialect_has_no_such_concept() {
	for row in ROWS {
		assert_eq!(
			row.unknown_tag == NotApplicable,
			row.fixtures.unknown_tag.is_none(),
			"'{}': `NotApplicable` and an absent fixture must agree — \
			 otherwise the cell is a way to skip the question",
			row.id
		);
		assert_eq!(
			row.unfittable == NotApplicable,
			row.fixtures.unfittable.is_none(),
			"'{}': `NotApplicable` and an absent fixture must agree",
			row.id
		);
	}
}

/// `DropsRemote`, `Approximated` and `NotAttachedToServer` are things the
/// probes REPORT when a dialect went soft — never answers a dialect may claim.
/// Without this, a red `mixed entry: table says Rejected, dialect does
/// DropsRemote` could be cleared by editing the CELL instead of the dialect,
/// and the suite would go green over a half-parsing dialect that deletes the
/// other half on the next save. That is the round-six defect, blessed by the
/// file built to prevent it. A comment saying "no row claims this" is not a
/// check; this is.
#[test]
fn no_row_may_declare_a_known_defect_shape() {
	for row in ROWS {
		for (question, answer) in [
			("mixed entry", row.mixed),
			("unknown transport tag", row.unknown_tag),
			("unowned field", row.unmodeled),
			("value that does not fit", row.unfittable),
			("SSE it cannot spell", row.unwritable_sse),
		] {
			assert!(
				!matches!(
					answer,
					DropsRemote | Approximated | NotAttachedToServer
				),
				"'{}' declares {answer:?} for `{question}`. Fix the dialect, \
				 not the table — those three are defect shapes the probes \
				 report, not answers a dialect is allowed to give.",
				row.id
			);
		}
	}
}

fn report(
	agent: &str,
	question: &str,
	expected: &Answer,
	actual: &Answer,
) -> String {
	format!("\n  {agent} :: {question}: table says {expected:?}, dialect does {actual:?}")
}

#[test]
fn each_dialect_answers_the_way_its_row_says() {
	let mut wrong = Vec::new();
	for descriptor in agents_with_mcp() {
		let Some(row) = find(descriptor.id) else {
			continue;
		};
		let parse = descriptor.mcp_parse_config.expect("claimed parser");
		let serialize =
			descriptor.mcp_serialize_config.expect("claimed serializer");

		let mixed = observe_mixed(parse(row.fixtures.mixed));
		if mixed != row.mixed {
			wrong.push(report(row.id, "mixed entry", &row.mixed, &mixed));
		}

		if let Some(fixture) = row.fixtures.unknown_tag {
			let tag = observe_unknown_tag(parse(fixture));
			if tag != row.unknown_tag {
				wrong.push(report(
					row.id,
					"unknown transport tag",
					&row.unknown_tag,
					&tag,
				));
			}
		}

		let (unmodeled, rewritten) =
			observe_unmodeled(descriptor, row.fixtures.unmodeled);
		if unmodeled != row.unmodeled {
			wrong.push(format!(
				"{}\n--- rewritten ---\n{rewritten}",
				report(row.id, "unowned field", &row.unmodeled, &unmodeled)
			));
		}

		if let Some(fixture) = row.fixtures.unfittable {
			let unfittable = observe_unfittable(parse(fixture), row.unfittable);
			if unfittable != row.unfittable {
				wrong.push(report(
					row.id,
					"value that does not fit",
					&row.unfittable,
					&unfittable,
				));
			}
		}

		let sse = match serialize(&sse_only(), None) {
			Ok(_) => Spelled,
			Err(_) => Rejected,
		};
		if sse != row.unwritable_sse {
			wrong.push(report(
				row.id,
				"SSE it cannot spell",
				&row.unwritable_sse,
				&sse,
			));
		}
	}
	assert!(
		wrong.is_empty(),
		"{} dialect decision(s) changed. These are CATEGORIES, not sentences: \
		 a diff here means a dialect started refusing something it used to \
		 accept, or accepting something it used to refuse. Byte-level \
		 expectations live in mcp_dialect_golden.{}",
		wrong.len(),
		wrong.concat()
	);
}
