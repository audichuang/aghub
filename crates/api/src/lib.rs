#[macro_use]
extern crate rocket;

use std::path::PathBuf;

use log::{debug, error, info, warn};
use rocket::{
	fairing::{Fairing, Info, Kind},
	Data, Request, Response,
};

pub mod cli;
pub(crate) mod credentials;
pub mod dto;
pub mod editor_detection;
pub mod error;
pub mod extractors;
pub mod routes;
pub(crate) mod skills;
pub mod state;

// Controller-side credential resolution for the desktop `src-tauri` layer
// (remote git-credential forwarding). The `credentials` module itself stays
// crate-private; only these wrappers + their origin types are public.
pub use crate::credentials::origin::ResolvedOrigin;
pub use crate::credentials::public::{
	list_bound_sources, resolve_git_token_for_source, ResolvedToken,
};

/// Version of the `aghub-api` binary/library, shared with the desktop remote
/// compatibility check.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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

pub(crate) fn default_app_data_dir() -> PathBuf {
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
	let cors = rocket_cors::CorsOptions {
		allowed_origins: rocket_cors::AllOrSome::All,
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
		.manage(crate::state::GitCloneSessions {
			sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
		})
		.manage(crate::state::InferenceProviderState { app_data_dir })
		.mount(
			"/api/v1",
			routes![
				routes::preflight,
				routes::agents::list_agents,
				routes::agents::check_availability,
				routes::market::search_skill_market,
				routes::skills::list_all_agents_skills,
				routes::skills::list_skills,
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
				routes::skills::git_scan_skills,
				routes::skills::git_install_skills,
				routes::skills::git_sync_skill,
				routes::skills_update::check_skill_updates,
				routes::skills_update::apply_skill_update,
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

pub async fn start(options: ApiOptions) -> Result<(), rocket::Error> {
	start_with_port_reporter(options, None).await
}

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
}
