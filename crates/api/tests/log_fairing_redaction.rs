//! SECURITY INVARIANT: `ApiLogFairing` must never write a request header into a
//! log sink. `X-Aghub-Git-Tokens` carries raw git tokens for remote credential
//! forwarding, so a single `request.headers()` in that fairing leaks every
//! user's tokens to the log.
//!
//! This file holds EXACTLY ONE `#[test]`, and that is load-bearing.
//! `log::set_logger` succeeds once per PROCESS. The previous guard lived in the
//! crate's lib test binary alongside ~425 other tests; whichever of them built a
//! Rocket first installed Rocket's own logger, so the capturing logger lost the
//! race, `install_capturing_logger` swallowed the `SetLoggerError` with
//! `let _ =`, and the buffer stayed EMPTY. Every `!logs.iter().any(...)`
//! assertion was then vacuously true: a fairing dumping the entire header map
//! AND naming the secret header still passed, while the same test run alone
//! failed. Adding a second test here that builds a Rocket before this one
//! re-creates that bug exactly.
//!
//! The positive canary below is the other half of the fix — it is what turns
//! "the secret is absent" from an empty-buffer tautology into a real assertion.

use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rocket::http::Header;
use rocket::local::blocking::Client;

static LOG_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct CapturingLogger;

impl log::Log for CapturingLogger {
	fn enabled(&self, _: &log::Metadata) -> bool {
		true
	}
	fn log(&self, record: &log::Record) {
		LOG_BUFFER
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push(format!("{}", record.args()));
	}
	fn flush(&self) {}
}

const SECRET: &str = "SUPER-SECRET-GIT-TOKEN-9f3a2b";

#[test]
fn api_log_fairing_never_logs_the_forwarded_token_header() {
	// A hard `expect`, never `let _ =`. If anything else in this process won the
	// logger, the capture is dead and every assertion below would be vacuous —
	// so fail loudly on that exact regression instead of passing silently.
	log::set_logger(&CapturingLogger).expect(
		"the capturing logger must be installed before any Rocket exists",
	);
	log::set_max_level(log::LevelFilter::Trace);

	// An isolated root: the old guard passed `default_app_data_dir()`, i.e. the
	// developer's REAL data directory.
	let data_dir = tempfile::tempdir().expect("temp data dir");
	let client = Client::tracked(aghub_api::build_rocket_for_tests(
		rocket::Config::default(),
		data_dir.path().to_path_buf(),
	))
	.expect("client");

	let header_value =
		BASE64.encode(format!("{{\"owner/repo\":\"{SECRET}\"}}"));

	// Any mounted route triggers the request/response logging fairing.
	let _ = client
		.get("/api/v1/agents")
		.header(Header::new("X-Aghub-Git-Tokens", header_value.clone()))
		.dispatch();

	let logs = LOG_BUFFER.lock().unwrap_or_else(|e| e.into_inner());

	// THE CANARY, asserted first. Without it the three negatives below pass on an
	// empty buffer, which is precisely how this invariant went uncovered.
	assert!(
		logs.iter().any(
			|line| line.contains("api request started: GET /api/v1/agents")
		),
		"the fairing's own line is missing, so nothing was captured and the \
		 assertions below would prove nothing (buffer len = {})",
		logs.len()
	);

	assert!(
		!logs.iter().any(|line| line.contains(SECRET)),
		"the raw forwarded token must never be logged"
	);
	assert!(
		!logs.iter().any(|line| line.contains(&header_value)),
		"the encoded forward header value must never be logged"
	);
	assert!(
		!logs.iter().any(|line| line.contains("X-Aghub-Git-Tokens")),
		"the forward header name must not appear in logs either"
	);
}
