#[macro_use]
extern crate rocket;

use std::path::PathBuf;
use std::sync::Arc;

use log::{debug, error, info, warn};
use rocket::{
	fairing::{Fairing, Info, Kind},
	Data, Request, Response,
};

pub mod blocking;
pub mod cli;
pub(crate) mod credentials;
pub mod dto;
pub mod editor_detection;
pub mod error;
pub mod extractors;
pub mod routes;
pub(crate) mod skills;
pub(crate) mod source_sessions;
pub mod state;

// Controller-side credential resolution for the desktop `src-tauri` layer
// (remote git-credential forwarding). The `credentials` module itself stays
// crate-private; only these wrappers + their origin types are public.
pub use crate::credentials::origin::ResolvedOrigin;
pub use crate::credentials::public::{
	list_bound_sources, resolve_git_token_for_source, ResolvedToken,
};

/// Version of the `aghub-api` binary/library, shared with the desktop remote
/// compatibility check. Git-derived by `build.rs` (see there for the
/// resolution order) so a from-source build self-reports a real version
/// instead of the workspace manifest's placeholder.
pub const VERSION: &str = env!("AGHUB_API_VERSION");

#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct ApiOptions {
	pub port: u16,
	pub app_data_dir: Option<PathBuf>,
}

impl ApiOptions {
	pub fn new(port: u16) -> Self {
		Self {
			port,
			app_data_dir: None,
		}
	}
}

/// `dirs::data_dir()/aghub` — the root the CLI (`commands::app_data_dir`) and
/// the desktop's skill-check sidecar also resolve to. Public because the
/// desktop needs the SAME root the scheduled CLI writes into; Tauri's own
/// `app_data_dir()` is identifier-scoped (`<data>/com.akrc.aghub`) and would
/// point somewhere else.
pub fn default_app_data_dir() -> PathBuf {
	dirs::data_dir()
		.unwrap_or_else(std::env::temp_dir)
		.join("aghub")
}

struct ApiLogFairing;

#[rocket::async_trait]
impl Fairing for ApiLogFairing {
	fn info(&self) -> Info {
		Info {
			name: "aghub-api request logger",
			kind: Kind::Request | Kind::Response,
		}
	}

	// SECURITY INVARIANT (remote git-credential forwarding): this fairing logs
	// ONLY the method, URI, and response status — never request/response
	// headers and never the body. The `X-Aghub-Git-Tokens` forward header
	// carries raw git tokens, so it must never reach a log sink. As long as
	// this fairing does not log `request.headers()`, that header (and any
	// future secret header) is safe by construction. The
	// `api_log_fairing_never_logs_forwarded_token_header` test asserts this
	// invariant.
	async fn on_request(&self, request: &mut Request<'_>, _: &mut Data<'_>) {
		info!(
			"api request started: {} {}",
			request.method(),
			request.uri()
		);
	}

	async fn on_response<'r>(
		&self,
		request: &'r Request<'_>,
		response: &mut Response<'r>,
	) {
		let status = response.status();
		if status.class().is_server_error() {
			error!(
				"api request failed: {} {} -> {}",
				request.method(),
				request.uri(),
				status
			);
		} else if status.class().is_client_error() {
			warn!(
				"api request returned client error: {} {} -> {}",
				request.method(),
				request.uri(),
				status
			);
		} else {
			debug!(
				"api request completed: {} {} -> {}",
				request.method(),
				request.uri(),
				status
			);
		}
	}
}

pub(crate) fn build_rocket(
	config: rocket::Config,
	app_data_dir: PathBuf,
) -> rocket::Rocket<rocket::Build> {
	build_rocket_with_skill_repository_factory(
		config,
		app_data_dir,
		crate::state::SkillRepositoryFactory::default(),
	)
}

pub(crate) fn build_rocket_with_skill_repository_factory(
	config: rocket::Config,
	app_data_dir: PathBuf,
	skill_repositories: crate::state::SkillRepositoryFactory,
) -> rocket::Rocket<rocket::Build> {
	build_rocket_with_state_factories(
		config,
		app_data_dir,
		skill_repositories,
		Arc::new(aghub_inference::NativeCredentialStore),
	)
}

/// Test-only entry point: same as [`build_rocket`], but lets a test inject a
/// deterministic credential store (e.g. an in-memory store) for
/// `routes::inference`, instead of the real OS keyring. Route tests that
/// exercise inference provider delete/create/etc. should build their client
/// through this, not `build_rocket` — a hardcoded `NativeCredentialStore`
/// coupled those tests to a real, reachable keyring backend, which CI (no
/// gnome-keyring/dbus) does not have (GitHub #15 P1a).
#[cfg(test)]
pub(crate) fn build_rocket_with_inference_credentials(
	config: rocket::Config,
	app_data_dir: PathBuf,
	credentials: Arc<dyn aghub_inference::CredentialStore + Send + Sync>,
) -> rocket::Rocket<rocket::Build> {
	build_rocket_with_state_factories(
		config,
		app_data_dir,
		crate::state::SkillRepositoryFactory::default(),
		credentials,
	)
}

fn build_rocket_with_state_factories(
	config: rocket::Config,
	app_data_dir: PathBuf,
	skill_repositories: crate::state::SkillRepositoryFactory,
	credentials: Arc<dyn aghub_inference::CredentialStore + Send + Sync>,
) -> rocket::Rocket<rocket::Build> {
	// Only the desktop webview is a legitimate browser origin. Allow-listing it
	// (instead of `AllOrSome::All`) makes a cross-origin JSON POST — e.g. a
	// malicious page driving `git/scan` against the localhost API — fail its
	// CORS preflight, so the request never reaches the handler. Covers the
	// webview on every platform: `tauri://localhost` (macOS/Linux prod),
	// `http(s)://tauri.localhost` (Windows prod), `http://localhost:1420`
	// (vite dev). Non-browser clients (CLI, curl, the SSH-tunnel proxy) don't
	// do CORS, so they are unaffected. build_rocket is the single construction
	// point for both the standalone bin and the desktop-embedded server, so
	// this one change covers every launch path.
	let cors = rocket_cors::CorsOptions {
		allowed_origins: rocket_cors::AllowedOrigins::some(
			&[
				"http://localhost:1420",
				"http://tauri.localhost",
				"https://tauri.localhost",
			],
			&[r"^tauri://localhost$"],
		),
		allowed_methods: vec![
			rocket::http::Method::Get,
			rocket::http::Method::Post,
			rocket::http::Method::Put,
			rocket::http::Method::Delete,
			rocket::http::Method::Options,
		]
		.into_iter()
		.map(From::from)
		.collect(),
		allowed_headers: rocket_cors::AllowedHeaders::some(&[
			"Authorization",
			"Accept",
			"Content-Type",
			// Remote git-credential forwarding: the desktop attaches a
			// per-request `source → token` map to a remote api over the SSH
			// tunnel. The webview call is cross-origin, so the custom header
			// must be allow-listed or the preflight blocks it.
			"X-Aghub-Git-Tokens",
		]),
		allow_credentials: true,
		..Default::default()
	}
	.to_cors()
	.unwrap();
	rocket::custom(config)
		.attach(ApiLogFairing)
		.attach(cors)
		.manage(crate::source_sessions::PinnedSourceSessions::default())
		.manage(crate::state::InferenceProviderState {
			app_data_dir,
			credentials,
		})
		.manage(skill_repositories)
		.mount(
			"/api/v1",
			routes![
				routes::preflight,
				routes::agents::list_agents,
				routes::agents::check_availability,
				routes::market::search_skill_market,
				routes::skills::list_all_agents_skills,
				routes::skills::list_skills,
				routes::skills::list_skill_usage,
				routes::skills::create_skill,
				routes::skills::import_skill,
				routes::skills::get_skill,
				routes::skills::update_skill,
				routes::skills::delete_skill,
				routes::skills::enable_skill,
				routes::skills::disable_skill,
				routes::skills::install_skill,
				routes::skills::transfer_skill_route,
				routes::skills::reconcile_skill_route,
				routes::mcps::list_all_agents_mcps,
				routes::mcps::list_mcps,
				routes::mcps::create_mcp,
				routes::mcps::batch_create_mcp,
				routes::mcps::get_mcp,
				routes::mcps::update_mcp,
				routes::mcps::delete_mcp,
				routes::mcps::enable_mcp,
				routes::mcps::disable_mcp,
				routes::mcps::transfer_mcp_route,
				routes::mcps::reconcile_mcp_route,
				routes::sub_agents::list_all_agents_sub_agents,
				routes::sub_agents::list_sub_agents,
				routes::sub_agents::get_sub_agent,
				routes::sub_agents::create_sub_agent,
				routes::sub_agents::update_sub_agent,
				routes::sub_agents::delete_sub_agent,
				routes::sub_agents::transfer_sub_agent_route,
				routes::sub_agents::reconcile_sub_agent_route,
				routes::integrations::list_code_editors,
				routes::integrations::open_with_editor,
				routes::integrations::get_preferences,
				routes::credentials::list_credentials,
				routes::credentials::list_source_bindings_route,
				routes::credentials::bind_source_credential,
				routes::credentials::create_credential,
				routes::credentials::delete_credential,
				routes::inference::list_inference_providers,
				routes::inference::list_inference_provider_presets,
				routes::inference::list_opencode_providers,
				routes::inference::list_codex_providers,
				routes::inference::get_codex_state,
				routes::inference::create_opencode_provider,
				routes::inference::create_codex_provider,
				routes::inference::update_opencode_provider,
				routes::inference::update_codex_provider,
				routes::inference::update_codex_active_profile,
				routes::inference::update_codex_profile_provider,
				routes::inference::sync_opencode_provider,
				routes::inference::sync_codex_provider,
				routes::inference::delete_opencode_provider,
				routes::inference::delete_codex_provider,
				routes::inference::get_inference_provider_password,
				routes::inference::create_inference_provider,
				routes::inference::update_inference_provider,
				routes::inference::get_claude_state,
				routes::inference::create_claude_provider,
				routes::inference::update_claude_provider,
				routes::inference::sync_claude_provider,
				routes::inference::delete_claude_provider,
				routes::inference::clear_claude_state,
				routes::inference::clear_codex_state,
				routes::inference::delete_inference_provider,
				routes::skills::open_skill_folder,
				routes::skills::edit_skill_folder,
				routes::skills::get_skill_content,
				routes::skills::get_skill_tree,
				routes::skills::get_global_skill_lock,
				routes::skills::get_project_skill_lock,
				routes::skills::delete_skill_by_path,
				routes::skills::prune_lock_route,
				routes::skills::git_credential_status,
				routes::skills::git_scan_skills,
				routes::skills::git_install_skills,
				routes::skills::git_sync_skill,
				routes::skills_update::check_skill_updates,
				routes::skills_update::apply_skill_update,
				routes::skills_update::apply_skill_updates,
				routes::skills_update::accept_skill_rename,
				routes::sources::list_sources,
				routes::sources::diff_source,
				routes::coverage::skills_coverage,
				routes::plugins::list_plugins,
				routes::plugins::get_plugin_detail,
				routes::plugins::enable_plugin,
				routes::plugins::disable_plugin,
				routes::plugins::install_plugin,
				routes::plugins::uninstall_plugin,
				routes::plugins::update_plugin,
				routes::plugins::open_plugin_folder,
				routes::plugins::open_plugin_skill_in_editor,
				routes::plugins::get_plugin_config,
				routes::plugins::update_plugin_config,
				routes::plugins::delete_plugin_config,
				routes::plugins::list_plugin_market,
				routes::plugins::update_marketplace,
				routes::plugins::list_marketplaces,
				routes::plugins::add_marketplace,
				routes::plugins::remove_marketplace,
				routes::plugins::update_marketplace_one,
				routes::plugins::cli_status,
				routes::plugins::prune_plugins,
				routes::plugins::validate_plugin,
			],
		)
		.register(
			"/",
			catchers![
				routes::catchers::not_found,
				routes::catchers::unprocessable_entity,
				routes::catchers::internal_error,
				routes::catchers::default_catcher,
			],
		)
}

/// Callback invoked exactly once, AFTER Rocket has bound its listener, with the
/// real bound port. With an ephemeral `port: 0` the bound port is only known
/// post-bind, so a caller that must report the port (e.g. the SSH bring-up
/// parser) has to wait for liftoff rather than guess beforehand.
pub type PortReporter = std::sync::Arc<dyn Fn(u16) + Send + Sync + 'static>;

// `rocket::Error` is 224 bytes and belongs to rocket, so we cannot shrink it.
// Boxing it would change the signature every caller (desktop bring-up, the CLI
// `serve` path) matches on, to buy nothing: these two functions run once per
// process and never on a hot path.
#[allow(clippy::result_large_err)]
pub async fn start(options: ApiOptions) -> Result<(), rocket::Error> {
	start_with_port_reporter(options, None).await
}

#[allow(clippy::result_large_err)]
pub async fn start_with_port_reporter(
	options: ApiOptions,
	reporter: Option<PortReporter>,
) -> Result<(), rocket::Error> {
	info!("starting aghub API server on 127.0.0.1:{}", options.port);
	let app_data_dir =
		options.app_data_dir.unwrap_or_else(default_app_data_dir);
	let config = rocket::Config {
		port: options.port,
		address: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
		log_level: rocket::config::LogLevel::Normal,
		..rocket::Config::default()
	};
	let mut rocket = build_rocket(config, app_data_dir);
	if let Some(reporter) = reporter {
		// `on_liftoff` runs after the listener is bound, so `config().port` is
		// the real port even when we requested an ephemeral `0`.
		rocket = rocket.attach(rocket::fairing::AdHoc::on_liftoff(
			"aghub-api port reporter",
			move |rocket| {
				Box::pin(async move {
					reporter(rocket.config().port);
				})
			},
		));
	}
	rocket
		.launch()
		.await
		.inspect(|_rocket| {
			info!("aghub API server stopped cleanly");
		})
		.map(|_| ())
		.map_err(|error| {
			error!("aghub API server exited with error: {error}");
			error
		})
}

#[cfg(test)]
mod tests {
	use super::{build_rocket, default_app_data_dir};
	use rocket::http::{Header, Status};
	use rocket::local::blocking::Client;

	/// Process-global capturing logger. Records every formatted log message so a
	/// test can assert a secret (the `X-Aghub-Git-Tokens` value) never appears in
	/// any log line. The buffer only grows; tests search for their own unique
	/// token, so parallel test noise cannot cause a false pass/fail.
	static LOG_BUFFER: std::sync::Mutex<Vec<String>> =
		std::sync::Mutex::new(Vec::new());

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

	/// Install the capturing logger once for the whole test binary. Ignoring the
	/// `SetLoggerError` keeps this safe if another test/crate already installed a
	/// logger — the assertion below searches for a unique token regardless.
	fn install_capturing_logger() {
		static INSTALL: std::sync::Once = std::sync::Once::new();
		INSTALL.call_once(|| {
			let _ = log::set_logger(&CapturingLogger);
			log::set_max_level(log::LevelFilter::Trace);
		});
	}

	/// Regression test for the log-redaction invariant on `ApiLogFairing`: the
	/// `X-Aghub-Git-Tokens` header value must NEVER be logged. The fairing logs
	/// only method/URI/status, so the secret can never reach a log sink; this
	/// test sends a request carrying a unique token and asserts it appears in no
	/// captured log line (so a future change that starts logging headers fails).
	#[test]
	fn api_log_fairing_never_logs_forwarded_token_header() {
		install_capturing_logger();
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		const SECRET: &str = "SUPER-SECRET-GIT-TOKEN-9f3a2b";
		let header_value = {
			use base64::engine::general_purpose::STANDARD as BASE64;
			use base64::Engine as _;
			BASE64.encode(format!("{{\"owner/repo\":\"{SECRET}\"}}"))
		};

		// Any mounted route triggers the request/response logging fairing.
		let _ = client
			.get("/api/v1/agents")
			.header(Header::new("X-Aghub-Git-Tokens", header_value.clone()))
			.dispatch();

		let logs = LOG_BUFFER.lock().unwrap_or_else(|e| e.into_inner());
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

	#[test]
	fn plugin_preflight_routes_return_cors_response() {
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let path = "/api/v1/plugins/uninstall";
		let response = client
			.req(rocket::http::Method::Options, path)
			.header(Header::new("Origin", "http://localhost:1420"))
			.header(Header::new("Access-Control-Request-Method", "POST"))
			.header(Header::new(
				"Access-Control-Request-Headers",
				"content-type",
			))
			.dispatch();

		assert_eq!(response.status(), Status::NoContent);
		assert_eq!(
			response.headers().get_one("Access-Control-Allow-Origin"),
			Some("http://localhost:1420"),
		);
		assert_eq!(
			response.headers().get_one("Access-Control-Allow-Headers"),
			Some("content-type"),
		);
	}

	#[test]
	fn cors_rejects_untrusted_origin_preflight() {
		// Layer-1 drive-by guard: an untrusted browser origin must NOT be
		// granted CORS access to the localhost API. Before origins were
		// allow-listed (`AllOrSome::All`), this echoed any Origin back, letting
		// a malicious page's cross-origin JSON POST (e.g. `git/scan`) preflight
		// succeed and the real request go through.
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let response = client
			.req(rocket::http::Method::Options, "/api/v1/plugins/uninstall")
			.header(Header::new("Origin", "http://evil.example"))
			.header(Header::new("Access-Control-Request-Method", "POST"))
			.dispatch();

		assert_ne!(
			response.headers().get_one("Access-Control-Allow-Origin"),
			Some("http://evil.example"),
			"an untrusted origin must never be granted CORS access",
		);
	}

	#[test]
	fn trusted_local_origin_guard_blocks_foreign_origin_and_host() {
		// Layer-2 guard on a credential route: a browser cross-origin request
		// (foreign Origin) and a DNS-rebinding request (foreign Host, no Origin)
		// must both get 403 before the handler runs; a trusted origin must not
		// be blocked by the guard.
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let foreign_origin = client
			.get("/api/v1/credentials")
			.header(Header::new("Origin", "http://evil.example"))
			.dispatch();
		assert_eq!(
			foreign_origin.status(),
			Status::Forbidden,
			"a foreign Origin must be rejected by the guard",
		);

		let foreign_host = client
			.get("/api/v1/credentials")
			.header(Header::new("Host", "evil.example"))
			.dispatch();
		assert_eq!(
			foreign_host.status(),
			Status::Forbidden,
			"a foreign Host (DNS-rebinding) must be rejected by the guard",
		);

		// A trusted origin passes the guard (handler may then 200/500 depending
		// on the keyring, but the guard itself must not forbid it).
		let trusted = client
			.get("/api/v1/credentials")
			.header(Header::new("Origin", "tauri://localhost"))
			.dispatch();
		assert_ne!(
			trusted.status(),
			Status::Forbidden,
			"a trusted local origin must pass the guard",
		);
	}

	#[test]
	fn skill_content_rejects_path_outside_skills_roots() {
		let project = tempfile::tempdir().expect("project dir");
		let skill_dir = project.path().join(".claude/skills/legit");
		std::fs::create_dir_all(&skill_dir).expect("skill dir");
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: legit\ndescription: d\n---\n\n# Body\n",
		)
		.expect("write skill");

		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let mut q = url::form_urlencoded::Serializer::new(String::new());
		q.append_pair("path", "/etc/passwd");
		q.append_pair("scope", "project");
		q.append_pair("project_root", &project.path().to_string_lossy());
		let uri = format!("/api/v1/skills/content?{}", q.finish());

		let response = client.get(&uri).dispatch();
		// Refused, never served: an existing out-of-root path canonicalizes
		// outside the roots -> Forbidden; a path that does not exist on this
		// platform (e.g. `/etc/passwd` on Windows) -> NotFound. Both are a
		// refusal; only a 200 would be a security failure.
		let status = response.status();
		assert!(
			status == Status::Forbidden || status == Status::NotFound,
			"reading outside skills roots must be refused, not served; \
			 got {status:?}"
		);
	}

	#[test]
	fn skill_tree_rejects_parent_dir_traversal() {
		let project = tempfile::tempdir().expect("project dir");
		let skill_dir = project.path().join(".claude/skills/legit");
		std::fs::create_dir_all(&skill_dir).expect("skill dir");
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: legit\ndescription: d\n---\n\n# Body\n",
		)
		.expect("write skill");

		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let escape = skill_dir
			.join("../../../../../../etc")
			.to_string_lossy()
			.to_string();
		let mut q = url::form_urlencoded::Serializer::new(String::new());
		q.append_pair("path", &escape);
		q.append_pair("scope", "project");
		q.append_pair("project_root", &project.path().to_string_lossy());
		let uri = format!("/api/v1/skills/tree?{}", q.finish());

		let response = client.get(&uri).dispatch();
		// Refused regardless of platform: the `..` target either resolves
		// outside the roots (Forbidden) or to a non-existent path (NotFound);
		// the escape depth lands differently on deeper macOS/Windows temp
		// dirs, but it never resolves back inside the roots, so never 200.
		let status = response.status();
		assert!(
			status == Status::Forbidden || status == Status::NotFound,
			"traversal must be refused; got {status:?}"
		);
	}

	#[cfg(unix)]
	#[test]
	fn skill_tree_rejects_symlink_escaping_root() {
		use std::os::unix::fs::symlink;
		let project = tempfile::tempdir().expect("project dir");
		let skills = project.path().join(".claude/skills");
		std::fs::create_dir_all(&skills).expect("skills dir");
		let outside = tempfile::tempdir().expect("outside");
		std::fs::create_dir_all(outside.path().join("secret"))
			.expect("secret dir");
		let evil = skills.join("evil");
		symlink(outside.path().join("secret"), &evil).expect("symlink");

		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let mut q = url::form_urlencoded::Serializer::new(String::new());
		q.append_pair("path", &evil.to_string_lossy());
		q.append_pair("scope", "project");
		q.append_pair("project_root", &project.path().to_string_lossy());
		let uri = format!("/api/v1/skills/tree?{}", q.finish());

		let response = client.get(&uri).dispatch();
		let status = response.status();
		assert!(
			status == Status::Forbidden || status == Status::NotFound,
			"a skills-root entry that is a symlink out of tree must be \
			 refused; got {status:?}"
		);
	}

	// --- DELETE MCP / sub-agent over the real HTTP wire (#5 audit) ----------
	//
	// The route-module tests call the handlers directly, which proves the seam
	// but NOT the wire change: the routes returned 204 NoContent before #5 and
	// now return 200 + the removal JSON. These dispatch through a mounted Rocket
	// client so the status code, `confirm` query parsing, route mounting and the
	// on-disk effect are all exercised end-to-end.

	/// Seed one Claude MCP over HTTP into a project-scoped temp root.
	fn seed_mcp_http(client: &Client, root: &std::path::Path) {
		let body = serde_json::json!({
			"name": "wire",
			"transport": { "type": "stdio", "command": "echo", "args": ["hi"] },
		})
		.to_string();
		let uri = format!(
			"/api/v1/agents/claude/mcps?scope=project&project_root={}",
			urlencoding(&root.to_string_lossy()),
		);
		let resp = client
			.post(&uri)
			.header(rocket::http::ContentType::JSON)
			.body(body)
			.dispatch();
		assert_eq!(resp.status(), Status::Created, "seed mcp must succeed");
	}

	/// Minimal query-component percent-encoder (avoids a serializer import).
	fn urlencoding(s: &str) -> String {
		let mut q = url::form_urlencoded::Serializer::new(String::new());
		q.append_pair("v", s);
		// "v=" prefix stripped -> just the encoded value.
		q.finish().trim_start_matches("v=").to_string()
	}

	#[test]
	fn delete_mcp_wire_dry_run_returns_200_json_and_keeps_entry() {
		let project = tempfile::tempdir().expect("project dir");
		let root = project.path();
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");
		seed_mcp_http(&client, root);

		let uri = format!(
			"/api/v1/agents/claude/mcps/wire?scope=project&project_root={}",
			urlencoding(&root.to_string_lossy()),
		);
		// No `confirm` => default dry-run.
		let resp = client.delete(&uri).dispatch();
		// Pre-#5 this was 204 NoContent; the wire change is 200 + JSON.
		assert_eq!(resp.status(), Status::Ok, "delete must be 200, not 204");
		let json: serde_json::Value =
			serde_json::from_str(&resp.into_string().unwrap()).unwrap();
		assert_eq!(json["success"], true);
		assert_eq!(json["dry_run"], true);
		assert_eq!(json["executed"], false);
		assert!(json["deleted_path"].is_null());
		// The MCP config file still holds the entry after a dry-run.
		let cfg =
			std::fs::read_to_string(root.join(".mcp.json")).unwrap_or_default();
		assert!(cfg.contains("wire"), "dry-run must leave the mcp on disk");
	}

	#[test]
	fn delete_mcp_wire_confirm_returns_200_and_removes_entry() {
		let project = tempfile::tempdir().expect("project dir");
		let root = project.path();
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");
		seed_mcp_http(&client, root);

		let uri = format!(
			"/api/v1/agents/claude/mcps/wire?scope=project&project_root={}&confirm=true",
			urlencoding(&root.to_string_lossy()),
		);
		let resp = client.delete(&uri).dispatch();
		assert_eq!(resp.status(), Status::Ok);
		let json: serde_json::Value =
			serde_json::from_str(&resp.into_string().unwrap()).unwrap();
		assert_eq!(json["success"], true);
		assert_eq!(json["executed"], true, "confirm=true must execute");
		assert_eq!(json["dry_run"], false);
		// The entry is gone from disk.
		let cfg =
			std::fs::read_to_string(root.join(".mcp.json")).unwrap_or_default();
		assert!(!cfg.contains("wire"), "confirm=true must remove the mcp");
	}

	/// Seed one Claude sub-agent over HTTP, returning its backing file path.
	fn seed_sub_agent_http(
		client: &Client,
		root: &std::path::Path,
	) -> std::path::PathBuf {
		let body = serde_json::json!({
			"name": "wire",
			"description": "d",
			"instruction": "do things",
		})
		.to_string();
		let create_uri = format!(
			"/api/v1/agents/claude/sub-agents?scope=project&project_root={}",
			urlencoding(&root.to_string_lossy()),
		);
		let created = client
			.post(&create_uri)
			.header(rocket::http::ContentType::JSON)
			.body(body)
			.dispatch();
		assert_eq!(created.status(), Status::Created, "seed sub-agent");
		let file = root.join(".claude/agents/wire.md");
		assert!(file.exists(), "precondition: backing file written");
		file
	}

	#[test]
	fn delete_sub_agent_wire_dry_run_returns_200_json_and_keeps_file() {
		// Proves the default (no `confirm`) over the wire is a dry-run: 200 + JSON
		// with executed:false and the backing file untouched — the counterpart to
		// the confirm test below, matching the MCP route's dry-run vs confirm pair.
		let project = tempfile::tempdir().expect("project dir");
		let root = project.path();
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");
		let file = seed_sub_agent_http(&client, root);

		let del_uri = format!(
			"/api/v1/agents/claude/sub-agents/wire?scope=project&project_root={}",
			urlencoding(&root.to_string_lossy()),
		);
		// No `confirm` => default dry-run.
		let resp = client.delete(&del_uri).dispatch();
		assert_eq!(resp.status(), Status::Ok, "delete must be 200, not 204");
		let json: serde_json::Value =
			serde_json::from_str(&resp.into_string().unwrap()).unwrap();
		assert_eq!(json["success"], true);
		assert_eq!(json["dry_run"], true);
		assert_eq!(json["executed"], false);
		assert!(json["deleted_path"].is_null());
		assert!(file.exists(), "dry-run must leave the backing file on disk");
	}

	#[test]
	fn delete_sub_agent_wire_confirm_returns_200_and_removes_file() {
		let project = tempfile::tempdir().expect("project dir");
		let root = project.path();
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let file = seed_sub_agent_http(&client, root);

		let del_uri = format!(
			"/api/v1/agents/claude/sub-agents/wire?scope=project&project_root={}&confirm=true",
			urlencoding(&root.to_string_lossy()),
		);
		let resp = client.delete(&del_uri).dispatch();
		// Pre-#5 this was 204 NoContent; the wire change is 200 + JSON.
		assert_eq!(resp.status(), Status::Ok, "delete must be 200, not 204");
		let json: serde_json::Value =
			serde_json::from_str(&resp.into_string().unwrap()).unwrap();
		assert_eq!(json["success"], true);
		assert_eq!(json["executed"], true);
		assert!(!file.exists(), "confirm=true must remove the backing file");
	}

	#[tokio::test]
	async fn port_reporter_fires_after_bind_with_real_ephemeral_port() {
		use super::{start_with_port_reporter, ApiOptions};
		use std::sync::{Arc, Mutex};

		// The reporter forwards the bound port once, post-liftoff.
		let (tx, rx) = tokio::sync::oneshot::channel::<u16>();
		let tx = Arc::new(Mutex::new(Some(tx)));
		let reporter: super::PortReporter = Arc::new(move |port: u16| {
			if let Some(tx) = tx.lock().unwrap().take() {
				let _ = tx.send(port);
			}
		});

		let server = tokio::spawn(async move {
			// Port 0 -> the OS assigns an ephemeral port at bind time.
			let _ = start_with_port_reporter(
				ApiOptions {
					port: 0,
					app_data_dir: Some(default_app_data_dir()),
				},
				Some(reporter),
			)
			.await;
		});

		// Liftoff (and thus the report) must arrive promptly.
		let port = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
			.await
			.expect("port reporter should fire before the timeout")
			.expect("reporter should send a port");
		// An ephemeral bind never yields port 0; we got the real bound port.
		assert!(port > 0, "expected a real bound port, got {port}");

		// Tear the server down.
		server.abort();
		let _ = server.await;
	}

	/// Replace Rocket dynamic path segments (`<id>`, `<name..>`, …) with a
	/// non-empty placeholder so the router matches. Query is omitted when fully
	/// dynamic/wild (current routes); static `key=value` query fields are kept.
	fn fill_uri(uri: &str) -> String {
		let (path, query) = match uri.split_once('?') {
			Some((p, q)) => (p, Some(q)),
			None => (uri, None),
		};
		let mut filled = String::with_capacity(path.len());
		let mut chars = path.chars().peekable();
		while let Some(c) = chars.next() {
			if c == '<' {
				for inner in chars.by_ref() {
					if inner == '>' {
						break;
					}
				}
				filled.push('x');
			} else {
				filled.push(c);
			}
		}
		if let Some(q) = query {
			// Keep only static `key=value` fields; drop dynamic/wild (`<…>`).
			let static_parts: Vec<&str> = q
				.split('&')
				.filter(|part| !part.contains('<') && part.contains('='))
				.collect();
			if !static_parts.is_empty() {
				filled.push('?');
				filled.push_str(&static_parts.join("&"));
			}
		}
		filled
	}

	/// Triple isolation for handler-side side effects if a route ever omits the
	/// Layer-2 guard: (a) env → temp HOME/XDG/PATH under `test_env_lock`,
	/// (b) temp app_data for inference SQLite, (c) mock keyring builder so the
	/// native OS keychain is never touched.
	struct IsolatedApiTest {
		_lock: std::sync::MutexGuard<'static, ()>,
		_home: tempfile::TempDir,
		_xdg_config: tempfile::TempDir,
		_xdg_state: tempfile::TempDir,
		_path_bin: tempfile::TempDir,
		app_data: tempfile::TempDir,
		old_home: Option<String>,
		old_xdg_config: Option<String>,
		old_xdg_state: Option<String>,
		old_path: Option<String>,
	}

	impl IsolatedApiTest {
		fn new() -> Self {
			let lock = crate::routes::test_env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			// Process-global; safe under the same lock that serializes env
			// tests -- but `keyring::set_default_credential_builder` has no
			// "unset" API, only "set to something else". Left unrestored
			// (as before this fix), the mock stayed the process-global
			// default for the rest of the test binary, racing any other
			// test that expects the platform's real keyring backend
			// depending on run order (GitHub #15 P1-1: `cargo test` order
			// determined whether the DBUS-tampering fail-closed test in
			// `routes::inference` saw a real secret-service failure or a
			// trivially-succeeding mock). `Drop` below puts the platform's
			// real default builder back so this guard's effect is scoped to
			// its own lifetime, not the rest of the process.
			keyring::set_default_credential_builder(
				keyring::mock::default_credential_builder(),
			);

			let home = tempfile::tempdir().expect("home tempdir");
			let xdg_config = tempfile::tempdir().expect("xdg config tempdir");
			let xdg_state = tempfile::tempdir().expect("xdg state tempdir");
			let path_bin = tempfile::tempdir().expect("PATH tempdir");
			let app_data = tempfile::tempdir().expect("app_data tempdir");

			let old_home = std::env::var("HOME").ok();
			let old_xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
			let old_xdg_state = std::env::var("XDG_STATE_HOME").ok();
			let old_path = std::env::var("PATH").ok();

			std::env::set_var("HOME", home.path());
			std::env::set_var("XDG_CONFIG_HOME", xdg_config.path());
			std::env::set_var("XDG_STATE_HOME", xdg_state.path());
			// Minimal PATH so handlers that shell out to CLIs cannot find tools.
			std::env::set_var("PATH", path_bin.path());

			Self {
				_lock: lock,
				_home: home,
				_xdg_config: xdg_config,
				_xdg_state: xdg_state,
				_path_bin: path_bin,
				app_data,
				old_home,
				old_xdg_config,
				old_xdg_state,
				old_path,
			}
		}

		fn restore_var(key: &str, old: &Option<String>) {
			match old {
				Some(v) => std::env::set_var(key, v),
				None => std::env::remove_var(key),
			}
		}
	}

	impl Drop for IsolatedApiTest {
		fn drop(&mut self) {
			// Restore the platform's real default builder -- see the
			// comment in `new()`. Still under `_lock` (dropped after this
			// fn returns), so this can't race another `IsolatedApiTest`.
			keyring::set_default_credential_builder(
				keyring::default::default_credential_builder(),
			);
			Self::restore_var("HOME", &self.old_home);
			Self::restore_var("XDG_CONFIG_HOME", &self.old_xdg_config);
			Self::restore_var("XDG_STATE_HOME", &self.old_xdg_state);
			Self::restore_var("PATH", &self.old_path);
		}
	}

	/// RAII guard restoring an env var's previous value on drop — including
	/// during a panicking unwind, unlike a plain "restore after the assert"
	/// statement. Fixes a real bug in the ORIGINAL version of
	/// `isolated_api_test_restores_real_builder_after_drop` (GitHub #15
	/// round-2 Codex finding): it restored `DBUS_SESSION_BUS_ADDRESS` in a
	/// plain statement placed AFTER the `assert!`, so a failing assertion
	/// panicked straight past the restore and left the bogus D-Bus address
	/// set for every later test in the same `cargo test` process.
	// Only constructed by `isolated_api_test_restores_real_builder_after_drop`
	// below, which is `#[cfg(target_os = "linux")]` -- match that exactly so
	// non-Linux targets don't see this as dead code under clippy -D warnings.
	#[cfg(target_os = "linux")]
	struct EnvVarRestoreGuard {
		key: &'static str,
		old_value: Option<String>,
	}

	#[cfg(target_os = "linux")]
	impl Drop for EnvVarRestoreGuard {
		fn drop(&mut self) {
			IsolatedApiTest::restore_var(self.key, &self.old_value);
		}
	}

	/// Regression (GitHub #15 P1-1, Codex-found): `IsolatedApiTest::new()`
	/// used to leave keyring's process-global default credential builder
	/// permanently set to the mock, with no restore -- so whichever test ran
	/// afterward in the same `cargo test` process would silently see the
	/// in-memory mock instead of a real (or really-erroring) backend,
	/// regardless of what it expected. Drive a full `IsolatedApiTest`
	/// lifecycle, then -- strictly after it drops -- point
	/// `DBUS_SESSION_BUS_ADDRESS` at a socket that does not exist and
	/// attempt a real keyring round trip. Before the `Drop` fix this
	/// spuriously SUCCEEDS (the leaked mock ignores D-Bus entirely and just
	/// round-trips in memory); after the fix the real platform builder is
	/// back in place, so a broken D-Bus session must produce a real
	/// `PlatformFailure`, proving the guard's mock was actually swapped back
	/// out and not left process-global.
	///
	/// **Linux-only** (GitHub #15 round-2 Codex finding): this test's whole
	/// point is proving the REAL platform keyring builder is active, which
	/// on this crate's feature set (see `Cargo.toml`) is Linux
	/// secret-service/D-Bus. `DBUS_SESSION_BUS_ADDRESS` does nothing on
	/// macOS Keychain / Windows Credential Manager, so on those CI runners
	/// this test would get a non-error result and fail — there is no
	/// equivalent env-var lever to break the real backend on those
	/// platforms, so cross-platform coverage of THIS SPECIFIC property is
	/// intentionally not attempted (unlike the two 503 tests in
	/// `routes::skills_update`/`routes::skills`, which inject a fake
	/// backend-unavailable result instead of touching any real backend at
	/// all, and so stay fully cross-platform).
	#[cfg(target_os = "linux")]
	#[test]
	fn isolated_api_test_restores_real_builder_after_drop() {
		{
			let _iso = IsolatedApiTest::new();
			// `_iso` drops at the end of this block, releasing
			// `test_env_lock` too -- the lock is re-acquired below only
			// after that has happened, so this can't deadlock.
		}

		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _restore_dbus = EnvVarRestoreGuard {
			key: "DBUS_SESSION_BUS_ADDRESS",
			old_value: std::env::var("DBUS_SESSION_BUS_ADDRESS").ok(),
		};
		std::env::set_var(
			"DBUS_SESSION_BUS_ADDRESS",
			"unix:path=/tmp/aghub-test-no-such-bus-lib",
		);

		let entry =
			keyring::Entry::new("aghub-test-restore-probe", "probe").unwrap();
		let result = entry.set_password("probe-value");
		if result.is_ok() {
			// Only reachable if the bug is back, or this environment somehow
			// still resolves a bus despite the bogus address -- clean up
			// either way rather than leaving a stray real keyring entry.
			let _ = entry.delete_credential();
		}

		assert!(
			result.is_err(),
			"after IsolatedApiTest drops, the REAL platform credential \
			 builder must be active again -- an unreachable D-Bus session \
			 must produce a real error, not a silently-successful leaked \
			 mock"
		);
		// `_restore_dbus` drops here (even if the assert above panicked,
		// since it runs during unwind), restoring the env var.
	}

	/// The desktop posts to this exact path (`src/lib/api.ts`,
	/// `skills/apply-updates`) and its own test stubs `fetch`, so the two halves
	/// of that contract are verified in isolation and can drift apart silently:
	/// unmounting the route, or renaming its path, breaks "Update all" with
	/// nothing else going red.
	#[test]
	fn bulk_apply_updates_is_mounted_where_the_desktop_posts() {
		let iso = IsolatedApiTest::new();
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			iso.app_data.path().to_path_buf(),
		))
		.expect("client");

		assert!(
			client.rocket().routes().any(|route| {
				route.method == rocket::http::Method::Post
					&& route.uri.as_str() == "/api/v1/skills/apply-updates"
			}),
			"POST /api/v1/skills/apply-updates must stay mounted: {:?}",
			client
				.rocket()
				.routes()
				.map(|route| route.uri.as_str())
				.collect::<Vec<_>>()
		);
	}

	#[test]
	fn all_routes_reject_foreign_host() {
		// Gatekeeper: every non-OPTIONS mounted route must carry Layer-2
		// TrustedLocalOrigin. Probe with foreign Host and NO Origin — foreign
		// Origin alone is also blocked by CORS Layer 1 and would give a false
		// green. ContentType::JSON avoids format=json routes matching 404.
		let iso = IsolatedApiTest::new();
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			iso.app_data.path().to_path_buf(),
		))
		.expect("client");

		let mut checked = 0usize;
		for route in client.rocket().routes() {
			if route.method == rocket::http::Method::Options {
				continue;
			}
			// Only `/api/v1/*` handlers must carry Layer 2. rocket_cors mounts
			// internal `/cors/<status>` error routes that are not our surface.
			let uri = fill_uri(route.uri.as_str());
			if !uri.starts_with("/api/v1") {
				continue;
			}
			let response = client
				.req(route.method, &uri)
				.header(Header::new("Host", "evil.example"))
				.header(rocket::http::ContentType::JSON)
				.dispatch();
			assert_eq!(
				response.status(),
				Status::Forbidden,
				"route {} {} ({}) must reject foreign Host with 403; got {}",
				route.method,
				uri,
				route.name.as_deref().unwrap_or("?"),
				response.status(),
			);
			checked += 1;
		}
		assert!(
			checked > 0,
			"expected mounted non-OPTIONS /api/v1 routes to enumerate"
		);
	}

	#[test]
	fn trusted_origin_and_host_not_forbidden() {
		// Positive sanity: webview-shaped headers (trusted Origin + trusted Host)
		// must not be blocked by Layer 2. Use a pure read route so the handler
		// cannot fail for unrelated reasons in a way that confuses the assert.
		let iso = IsolatedApiTest::new();
		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			iso.app_data.path().to_path_buf(),
		))
		.expect("client");
		let port = client.rocket().config().port;
		let host = format!("127.0.0.1:{port}");

		for path in [
			"/api/v1/agents",
			"/api/v1/integrations/code-editors",
			"/api/v1/credentials",
		] {
			let response = client
				.get(path)
				.header(Header::new("Origin", "tauri://localhost"))
				.header(Header::new("Host", host.clone()))
				.dispatch();
			assert_ne!(
				response.status(),
				Status::Forbidden,
				"{path} with trusted Origin+Host must not be 403; got {}",
				response.status(),
			);
		}
	}
}
