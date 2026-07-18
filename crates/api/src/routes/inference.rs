use aghub_inference::{
	AgentProviderAdapter, AgentProviderBinding, ClaudeProviderAdapter,
	CodexProviderAdapter, CredentialStore, InferenceProvider,
	InferenceProviderRepository, InferenceProviderStore,
	OpenCodeProviderAdapter,
};
use rocket::http::Status;
use rocket::response::status::NoContent;
use rocket::serde::json::Json;
use rocket::State;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::dto::inference::{
	AgentProviderResponse, ClaudeProviderStateResponse,
	CodexProviderStateResponse, CreateAgentProviderRequest,
	CreateInferenceProviderRequest, InferenceProviderFormatDto,
	InferenceProviderPasswordResponse, InferenceProviderPresetResponse,
	InferenceProviderResponse, UpdateAgentProviderRequest,
	UpdateCodexActiveProfileRequest, UpdateCodexProfileProviderRequest,
	UpdateInferenceProviderRequest,
};
use crate::error::{
	run_blocking, ApiCreated, ApiError, ApiNoContent, ApiResult,
};
use crate::extractors::TrustedLocalOrigin;
use crate::state::InferenceProviderState;

/// Store type shared by every route in this file, backed by whatever
/// [`InferenceProviderState::credentials`] resolves to (real keyring in
/// production; an injected in-memory store in tests). Boxed as a trait
/// object so every handler shares one concrete type regardless of backend.
type Store = InferenceProviderStore<Arc<dyn CredentialStore + Send + Sync>>;

fn store(state: &State<InferenceProviderState>) -> Store {
	InferenceProviderStore::with_credentials(
		state.app_data_dir.clone(),
		state.credentials.clone(),
	)
}

fn find_by_latin_name(
	store: &Store,
	latin_name: &str,
) -> Result<InferenceProvider, ApiError> {
	store
		.list()
		.map_err(ApiError::from)?
		.into_iter()
		.find(|provider| provider.latin_name == latin_name)
		.ok_or_else(|| {
			ApiError::new(
				Status::NotFound,
				format!("inference provider '{latin_name}' not found"),
				"RESOURCE_NOT_FOUND",
			)
		})
}

fn opencode_adapter() -> Result<OpenCodeProviderAdapter, ApiError> {
	OpenCodeProviderAdapter::global().map_err(ApiError::from)
}

fn codex_adapter() -> Result<CodexProviderAdapter, ApiError> {
	CodexProviderAdapter::global().map_err(ApiError::from)
}

fn claude_adapter() -> Result<ClaudeProviderAdapter, ApiError> {
	ClaudeProviderAdapter::global().map_err(ApiError::from)
}

fn get_inventory_provider(
	store: &Store,
	id: &str,
) -> Result<(InferenceProvider, String), ApiError> {
	let provider = store.get(id).map_err(ApiError::from)?;
	let api_key = store
		.get_api_key(&provider.id)
		.map_err(ApiError::from)?
		.ok_or_else(|| {
			ApiError::new(
				Status::UnprocessableEntity,
				format!(
					"inference provider '{}' has no stored API key",
					provider.display_name
				),
				"MISSING_CREDENTIAL",
			)
		})?;
	Ok((provider, api_key))
}

fn same_api_base_url(left: &str, right: &str) -> bool {
	left.trim().trim_end_matches('/') == right.trim().trim_end_matches('/')
}

fn inventory_providers_with_api_keys(
	store: &Store,
) -> Result<Vec<(InferenceProvider, String)>, ApiError> {
	let mut providers = Vec::new();
	for provider in store.list().map_err(ApiError::from)? {
		let Some(api_key) =
			store.get_api_key(&provider.id).map_err(ApiError::from)?
		else {
			continue;
		};
		providers.push((provider, api_key));
	}
	Ok(providers)
}

fn opencode_inventory_providers_with_api_keys(
	store: &Store,
) -> Result<Vec<(InferenceProvider, String)>, ApiError> {
	Ok(inventory_providers_with_api_keys(store)?
		.into_iter()
		.map(|(provider, api_key)| {
			(
				OpenCodeProviderAdapter::normalize_inventory_provider(
					&provider,
				),
				api_key,
			)
		})
		.collect())
}

fn find_matching_inventory_provider(
	inventory: &[(InferenceProvider, String)],
	binding: &AgentProviderBinding,
	agent_api_key: Option<String>,
) -> Result<Option<(InferenceProvider, String)>, ApiError> {
	let Some(api_base_url) = binding.api_base_url.as_deref() else {
		return Ok(None);
	};
	let Some(agent_api_key) = agent_api_key else {
		return Ok(None);
	};

	for (provider, api_key) in inventory {
		if !same_api_base_url(&provider.api_base_url, api_base_url) {
			continue;
		}
		if api_key == &agent_api_key {
			return Ok(Some((provider.clone(), api_key.clone())));
		}
	}

	Ok(None)
}

fn opencode_provider_response(
	inventory: &[(InferenceProvider, String)],
	adapter: &OpenCodeProviderAdapter,
	binding: AgentProviderBinding,
) -> Result<AgentProviderResponse, ApiError> {
	let agent_api_key = adapter.api_key(&binding.id).map_err(ApiError::from)?;
	let matched =
		find_matching_inventory_provider(inventory, &binding, agent_api_key)?;
	let response = AgentProviderResponse::from(binding);
	Ok(match matched {
		Some((provider, _)) => {
			response.with_matched_inference_provider(&provider)
		}
		None => response,
	})
}

fn codex_provider_response(
	store: &Store,
	inventory: &[(InferenceProvider, String)],
	adapter: &CodexProviderAdapter,
	binding: AgentProviderBinding,
) -> Result<AgentProviderResponse, ApiError> {
	let agent_api_key = adapter
		.api_key(store, &binding.id)
		.map_err(ApiError::from)?;
	let matched =
		find_matching_inventory_provider(inventory, &binding, agent_api_key)?;
	let response = AgentProviderResponse::from(binding);
	Ok(match matched {
		Some((provider, _)) => {
			response.with_matched_inference_provider(&provider)
		}
		None => response,
	})
}

fn codex_state_response(
	store: &Store,
	adapter: &CodexProviderAdapter,
) -> Result<CodexProviderStateResponse, ApiError> {
	let inventory = inventory_providers_with_api_keys(store)?;
	let state = adapter.load_profile_state(store).map_err(ApiError::from)?;
	let providers = state
		.providers
		.iter()
		.cloned()
		.map(|binding| {
			codex_provider_response(store, &inventory, adapter, binding)
		})
		.collect::<Result<Vec<_>, _>>()?;
	Ok(CodexProviderStateResponse::from_state(state, providers))
}

#[get("/inference/providers")]
pub fn list_inference_providers(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
) -> ApiResult<Vec<InferenceProviderResponse>> {
	let providers = store(state)
		.list()
		.map_err(ApiError::from)?
		.into_iter()
		.map(InferenceProviderResponse::from)
		.collect();
	Ok(Json(providers))
}

const MODELS_DEV_API_JSON: &str =
	include_str!("../dto/data/models_dev_api.json");
const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";

fn models_dev_presets_from_json(
	json: &str,
) -> serde_json::Result<Vec<InferenceProviderPresetResponse>> {
	let providers =
		serde_json::from_str::<BTreeMap<String, ModelsDevProvider>>(json)?;
	let mut presets = providers
		.into_values()
		.filter_map(models_dev_provider_to_preset)
		.collect::<Vec<_>>();
	presets.sort_by_key(|preset| preset.name.to_lowercase());
	Ok(presets)
}

fn vendored_models_dev_presets() -> &'static [InferenceProviderPresetResponse] {
	use std::sync::OnceLock;
	static PRESETS: OnceLock<Vec<InferenceProviderPresetResponse>> =
		OnceLock::new();
	PRESETS.get_or_init(|| {
		models_dev_presets_from_json(MODELS_DEV_API_JSON)
			.expect("models_dev_api.json must be valid")
	})
}

#[derive(Debug, Deserialize)]
struct ModelsDevProvider {
	id: String,
	name: String,
	#[serde(default)]
	npm: Option<String>,
	#[serde(default)]
	api: Option<String>,
	#[serde(default)]
	doc: Option<String>,
	#[serde(default)]
	models: BTreeMap<String, serde_json::Value>,
}

fn default_api_base_url(provider_id: &str) -> Option<&'static str> {
	match provider_id {
		"anthropic" => Some("https://api.anthropic.com"),
		"openai" => Some("https://api.openai.com/v1"),
		_ => None,
	}
}

fn preset_format(npm: Option<&str>) -> Option<InferenceProviderFormatDto> {
	match npm {
		Some("@ai-sdk/anthropic") => {
			Some(InferenceProviderFormatDto::Anthropic)
		}
		Some("@ai-sdk/openai") => {
			Some(InferenceProviderFormatDto::OpenAiResponses)
		}
		Some("@ai-sdk/openai-compatible") => {
			Some(InferenceProviderFormatDto::OpenAiCompletions)
		}
		_ => None,
	}
}

fn models_dev_provider_to_preset(
	provider: ModelsDevProvider,
) -> Option<InferenceProviderPresetResponse> {
	let api_base_url = provider
		.api
		.or_else(|| default_api_base_url(&provider.id).map(str::to_string))?;
	let format = preset_format(provider.npm.as_deref())?;
	let models = provider.models.into_keys().collect::<Vec<_>>();

	Some(InferenceProviderPresetResponse {
		id: provider.id.clone(),
		name: provider.name.clone(),
		api_base_url,
		format,
		models,
		logo: provider.id,
		homepage: provider.doc,
		description: None,
	})
}

async fn fetch_models_dev_presets() -> Result<
	Vec<InferenceProviderPresetResponse>,
	Box<dyn std::error::Error + Send + Sync>,
> {
	let json = reqwest::Client::builder()
		.timeout(Duration::from_secs(8))
		.build()?
		.get(MODELS_DEV_API_URL)
		.send()
		.await?
		.error_for_status()?
		.text()
		.await?;
	Ok(models_dev_presets_from_json(&json)?)
}

#[get("/inference/presets")]
pub async fn list_inference_provider_presets(
	_origin: TrustedLocalOrigin,
) -> Json<Vec<InferenceProviderPresetResponse>> {
	match fetch_models_dev_presets().await {
		Ok(presets) if !presets.is_empty() => Json(presets),
		_ => Json(vendored_models_dev_presets().to_vec()),
	}
}

#[get("/inference/agents/opencode/providers")]
pub async fn list_opencode_providers(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
) -> ApiResult<Vec<AgentProviderResponse>> {
	let store = store(state);
	run_blocking(move || {
		let adapter = opencode_adapter()?;
		let inventory = opencode_inventory_providers_with_api_keys(&store)?;
		let providers = adapter
			.load_providers()
			.map_err(ApiError::from)?
			.providers
			.into_iter()
			.map(|binding| {
				opencode_provider_response(&inventory, &adapter, binding)
			})
			.collect::<Result<Vec<_>, _>>()?;
		Ok(Json(providers))
	})
	.await
}

#[get("/inference/agents/codex/providers")]
pub async fn list_codex_providers(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
) -> ApiResult<Vec<AgentProviderResponse>> {
	let store = store(state);
	run_blocking(move || {
		let adapter = codex_adapter()?;
		let inventory = inventory_providers_with_api_keys(&store)?;
		let providers = adapter
			.load_profile_state(&store)
			.map_err(ApiError::from)?
			.providers
			.into_iter()
			.map(|binding| {
				codex_provider_response(&store, &inventory, &adapter, binding)
			})
			.collect::<Result<Vec<_>, _>>()?;
		Ok(Json(providers))
	})
	.await
}

#[get("/inference/agents/codex/state")]
pub async fn get_codex_state(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
) -> ApiResult<CodexProviderStateResponse> {
	let store = store(state);
	run_blocking(move || {
		let adapter = codex_adapter()?;
		Ok(Json(codex_state_response(&store, &adapter)?))
	})
	.await
}

#[post("/inference/agents/opencode/providers", data = "<body>")]
pub async fn create_opencode_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	body: Json<CreateAgentProviderRequest>,
) -> ApiCreated<AgentProviderResponse> {
	let store = store(state);
	run_blocking(move || {
		let (provider, api_key) =
			get_inventory_provider(&store, &body.inference_provider_id)?;
		let binding = opencode_adapter()?
			.add_inventory_provider(&provider, &api_key)
			.map_err(ApiError::from)?;

		Ok((Status::Created, Json(binding.into())))
	})
	.await
}

#[post("/inference/agents/codex/providers", data = "<body>")]
pub async fn create_codex_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	body: Json<CreateAgentProviderRequest>,
) -> ApiCreated<AgentProviderResponse> {
	let store = store(state);
	run_blocking(move || {
		let (provider, api_key) =
			get_inventory_provider(&store, &body.inference_provider_id)?;
		let adapter = codex_adapter()?;
		let binding = adapter
			.add_inventory_provider(&store, &provider, &api_key)
			.map_err(ApiError::from)?;
		adapter
			.set_active_provider(&store, &binding.id)
			.map_err(ApiError::from)?;

		Ok((
			Status::Created,
			Json(
				AgentProviderResponse::from(binding)
					.with_matched_inference_provider(&provider),
			),
		))
	})
	.await
}

#[put("/inference/agents/opencode/providers/<id>", data = "<body>")]
pub fn update_opencode_provider(
	_origin: TrustedLocalOrigin,
	id: &str,
	body: Json<UpdateAgentProviderRequest>,
) -> ApiResult<AgentProviderResponse> {
	let body = body.into_inner();
	let binding = opencode_adapter()?
		.update_provider(id, body.name.as_deref(), body.api_key.as_deref())
		.map_err(ApiError::from)?;

	Ok(Json(binding.into()))
}

#[put("/inference/agents/codex/providers/<id>", data = "<body>")]
pub async fn update_codex_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	id: &str,
	body: Json<UpdateAgentProviderRequest>,
) -> ApiResult<AgentProviderResponse> {
	let store = store(state);
	let id = id.to_string();
	run_blocking(move || {
		let binding = codex_adapter()?
			.update_provider(
				&store,
				&id,
				body.name.as_deref(),
				body.api_key.as_deref(),
			)
			.map_err(ApiError::from)?;

		Ok(Json(binding.into()))
	})
	.await
}

#[put("/inference/agents/codex/profile", data = "<body>")]
pub async fn update_codex_active_profile(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	body: Json<UpdateCodexActiveProfileRequest>,
) -> ApiResult<CodexProviderStateResponse> {
	let store = store(state);
	run_blocking(move || {
		let adapter = codex_adapter()?;
		adapter
			.set_active_profile(&store, &body.profile_id)
			.map_err(ApiError::from)?;
		Ok(Json(codex_state_response(&store, &adapter)?))
	})
	.await
}

#[put(
	"/inference/agents/codex/profiles/<profile_id>/provider",
	data = "<body>"
)]
pub async fn update_codex_profile_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	profile_id: &str,
	body: Json<UpdateCodexProfileProviderRequest>,
) -> ApiResult<CodexProviderStateResponse> {
	let store = store(state);
	let profile_id = profile_id.to_string();
	run_blocking(move || {
		let adapter = codex_adapter()?;
		adapter
			.set_profile_provider(&store, &profile_id, &body.provider_id)
			.map_err(ApiError::from)?;
		Ok(Json(codex_state_response(&store, &adapter)?))
	})
	.await
}

#[post("/inference/agents/opencode/providers/<id>/sync")]
pub async fn sync_opencode_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	id: &str,
) -> ApiResult<AgentProviderResponse> {
	let store = store(state);
	let id = id.to_string();
	run_blocking(move || {
		let adapter = opencode_adapter()?;
		let inventory = opencode_inventory_providers_with_api_keys(&store)?;
		let binding = adapter
			.load_providers()
			.map_err(ApiError::from)?
			.providers
			.into_iter()
			.find(|provider| provider.id == id)
			.ok_or_else(|| {
				ApiError::new(
					Status::NotFound,
					format!("OpenCode provider '{id}' not found"),
					"RESOURCE_NOT_FOUND",
				)
			})?;
		let agent_api_key =
			adapter.api_key(&binding.id).map_err(ApiError::from)?;
		let Some((provider, api_key)) = find_matching_inventory_provider(
			&inventory,
			&binding,
			agent_api_key,
		)?
		else {
			return Err(ApiError::new(
				Status::UnprocessableEntity,
				format!(
					"OpenCode provider '{id}' is not backed by an aghub \
					 inference provider"
				),
				"UNRECOGNIZED_PROVIDER",
			));
		};

		let updated = adapter
			.add_provider(&id, &provider, &api_key)
			.map_err(ApiError::from)?;

		Ok(Json(
			AgentProviderResponse::from(updated)
				.with_matched_inference_provider(&provider),
		))
	})
	.await
}

#[post("/inference/agents/codex/providers/<id>/sync")]
pub async fn sync_codex_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	id: &str,
) -> ApiResult<AgentProviderResponse> {
	let store = store(state);
	let id = id.to_string();
	run_blocking(move || {
		let adapter = codex_adapter()?;
		let inventory = inventory_providers_with_api_keys(&store)?;
		let binding = adapter
			.load_profile_state(&store)
			.map_err(ApiError::from)?
			.providers
			.into_iter()
			.find(|provider| provider.id == id)
			.ok_or_else(|| {
				ApiError::new(
					Status::NotFound,
					format!("Codex provider '{id}' not found"),
					"RESOURCE_NOT_FOUND",
				)
			})?;
		let agent_api_key = adapter
			.api_key(&store, &binding.id)
			.map_err(ApiError::from)?;
		let Some((provider, api_key)) = find_matching_inventory_provider(
			&inventory,
			&binding,
			agent_api_key,
		)?
		else {
			return Err(ApiError::new(
				Status::UnprocessableEntity,
				format!(
					"Codex provider '{id}' is not backed by an aghub \
					 inference provider"
				),
				"UNRECOGNIZED_PROVIDER",
			));
		};

		let updated = adapter
			.add_provider(&id, &provider, &api_key)
			.map_err(ApiError::from)?;

		Ok(Json(
			AgentProviderResponse::from(updated)
				.with_matched_inference_provider(&provider),
		))
	})
	.await
}

#[delete("/inference/agents/opencode/providers/<id>")]
pub fn delete_opencode_provider(
	_origin: TrustedLocalOrigin,
	id: &str,
) -> ApiNoContent {
	opencode_adapter()?
		.remove_provider(id)
		.map_err(ApiError::from)?;
	Ok(NoContent)
}

#[delete("/inference/agents/codex/providers/<id>")]
pub async fn delete_codex_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	id: &str,
) -> ApiNoContent {
	let store = store(state);
	let id = id.to_string();
	run_blocking(move || {
		codex_adapter()?
			.remove_provider(&store, &id)
			.map_err(ApiError::from)?;
		Ok(NoContent)
	})
	.await
}

#[get("/inference/providers/<latin_name>/password")]
pub async fn get_inference_provider_password(
	state: &State<InferenceProviderState>,
	latin_name: &str,
	_origin: TrustedLocalOrigin,
) -> ApiResult<InferenceProviderPasswordResponse> {
	let store = store(state);
	let latin_name = latin_name.to_string();
	run_blocking(move || {
		let provider = find_by_latin_name(&store, &latin_name)?;
		let api_key = store
			.get_api_key(&provider.id)
			.map_err(ApiError::from)?
			.ok_or_else(|| {
				ApiError::new(
					Status::NotFound,
					format!(
						"inference provider '{}' has no stored API key",
						provider.display_name
					),
					"RESOURCE_NOT_FOUND",
				)
			})?;

		Ok(Json(InferenceProviderPasswordResponse {
			latin_name: provider.latin_name,
			api_key,
		}))
	})
	.await
}

#[post("/inference/providers", data = "<body>")]
pub async fn create_inference_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	body: Json<CreateInferenceProviderRequest>,
) -> ApiCreated<InferenceProviderResponse> {
	let store = store(state);
	run_blocking(move || {
		let provider = store
			.create(body.into_inner().into())
			.map_err(ApiError::from)?;
		Ok((Status::Created, Json(provider.into())))
	})
	.await
}

#[put("/inference/providers/<latin_name>", data = "<body>")]
pub async fn update_inference_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	latin_name: &str,
	body: Json<UpdateInferenceProviderRequest>,
) -> ApiResult<InferenceProviderResponse> {
	let store = store(state);
	let latin_name = latin_name.to_string();
	run_blocking(move || {
		let provider = find_by_latin_name(&store, &latin_name)?;
		let updated = store
			.update(&provider.id, body.into_inner().into())
			.map_err(ApiError::from)?;
		Ok(Json(updated.into()))
	})
	.await
}

#[delete("/inference/providers/<latin_name>")]
pub async fn delete_inference_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	latin_name: &str,
) -> ApiNoContent {
	let store = store(state);
	let provider = find_by_latin_name(&store, latin_name)?;
	// Shared use case: tears down every agent reference, then deletes the
	// provider. The CLI routes its delete through the same fn so neither
	// surface can leave an agent config pointing at a removed provider.
	//
	// Runs on Rocket's blocking pool via the shared `run_blocking` helper:
	// the cascade's precondition check and every step below it are OS
	// keyring reads/writes (D-Bus secret-service on Linux), which must never
	// block the async worker thread directly — Rocket does not
	// spawn_blocking sync handlers on its own (see the `keyring` feature
	// comment in `crates/api/Cargo.toml`).
	run_blocking(move || {
		aghub_inference::delete_provider_cascade(&store, &provider)
			.map_err(ApiError::from)
	})
	.await?;
	Ok(NoContent)
}

// ============================================================================
// Claude Code routes (binding-table backed)
// ============================================================================

fn claude_state_response(
	store: &Store,
	adapter: &ClaudeProviderAdapter,
) -> Result<ClaudeProviderStateResponse, ApiError> {
	let state = adapter.load_bindings_state(store).map_err(ApiError::from)?;
	let inventory = inventory_providers_with_api_keys(store)?;
	let providers = state
		.providers
		.iter()
		.cloned()
		.map(|binding| {
			// Built-in providers have no inventory backing; skip matching.
			let matched = match binding.source_provider_id.as_deref() {
				Some(id) if !id.is_empty() => inventory
					.iter()
					.find(|(provider, _)| provider.id == id)
					.cloned(),
				_ => None,
			};
			let response = AgentProviderResponse::from(binding);
			let result: Result<AgentProviderResponse, ApiError> =
				Ok(match matched {
					Some((provider, _)) => {
						response.with_matched_inference_provider(&provider)
					}
					None => response,
				});
			result
		})
		.collect::<Result<Vec<_>, _>>()?;
	let active_provider_id = adapter
		.derive_active_provider_id(store)
		.map_err(ApiError::from)?;
	Ok(ClaudeProviderStateResponse {
		providers,
		active_provider_id,
	})
}

#[get("/inference/agents/claude/state")]
pub async fn get_claude_state(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
) -> ApiResult<ClaudeProviderStateResponse> {
	let store = store(state);
	run_blocking(move || {
		let adapter = claude_adapter()?;
		Ok(Json(claude_state_response(&store, &adapter)?))
	})
	.await
}

#[post("/inference/agents/claude/providers", data = "<body>")]
pub async fn create_claude_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	body: Json<CreateAgentProviderRequest>,
) -> ApiCreated<AgentProviderResponse> {
	let store = store(state);
	run_blocking(move || {
		let (provider, api_key) =
			get_inventory_provider(&store, &body.inference_provider_id)?;
		let adapter = claude_adapter()?;
		let binding = adapter
			.add_binding(&store, &provider, &api_key, true)
			.map_err(ApiError::from)?;

		Ok((
			Status::Created,
			Json(
				AgentProviderResponse::from(binding)
					.with_matched_inference_provider(&provider),
			),
		))
	})
	.await
}

#[put("/inference/agents/claude/providers/<id>", data = "<body>")]
pub async fn update_claude_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	id: &str,
	body: Json<UpdateAgentProviderRequest>,
) -> ApiResult<ClaudeProviderStateResponse> {
	let store = store(state);
	let id = id.to_string();
	run_blocking(move || {
		let adapter = claude_adapter()?;

		if body.name.as_deref().is_some() {
			return Err(ApiError::new(
				Status::BadRequest,
				"Claude provider name cannot be changed".to_string(),
				"UNSUPPORTED_OPERATION",
			));
		}

		if let Some(api_key) = body.api_key.as_deref() {
			let row = store
				.get_agent_binding("claude", &id)
				.map_err(ApiError::from)?;
			let provider = store
				.get(&row.inference_provider_id)
				.map_err(ApiError::from)?;
			store
				.set_api_key(&provider.id, api_key)
				.map_err(ApiError::from)?;
		}
		adapter
			.set_active_binding(&store, &id)
			.map_err(ApiError::from)?;

		Ok(Json(claude_state_response(&store, &adapter)?))
	})
	.await
}

#[post("/inference/agents/claude/providers/<id>/sync")]
pub async fn sync_claude_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	id: &str,
) -> ApiResult<AgentProviderResponse> {
	let store = store(state);
	let id = id.to_string();
	run_blocking(move || {
		let adapter = claude_adapter()?;
		let row = store
			.get_agent_binding("claude", &id)
			.map_err(ApiError::from)?;
		let provider = store
			.get(&row.inference_provider_id)
			.map_err(ApiError::from)?;
		let was_active = adapter
			.derive_active_provider_id(&store)
			.map_err(ApiError::from)?
			== id;
		let model = provider.models.first().cloned();
		let row = store
			.update_agent_binding("claude", &id, Some(model.clone()))
			.map_err(ApiError::from)?;

		if was_active {
			let api_key = store
				.get_api_key(&provider.id)
				.map_err(ApiError::from)?
				.ok_or_else(|| {
					ApiError::new(
						Status::UnprocessableEntity,
						format!(
							"inference provider '{}' has no stored API key",
							provider.display_name
						),
						"MISSING_CREDENTIAL",
					)
				})?;

			adapter
				.sync_active_binding(&provider, &api_key, model.as_deref())
				.map_err(ApiError::from)?;
		}

		let binding = store.binding_from_row(&row).map_err(ApiError::from)?;
		Ok(Json(
			AgentProviderResponse::from(binding)
				.with_matched_inference_provider(&provider),
		))
	})
	.await
}

#[delete("/inference/agents/claude/providers/<id>")]
pub async fn delete_claude_provider(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
	id: &str,
) -> ApiNoContent {
	let store = store(state);
	let id = id.to_string();
	run_blocking(move || {
		let adapter = claude_adapter()?;
		adapter
			.remove_binding(&store, &id)
			.map_err(ApiError::from)?;
		Ok(NoContent)
	})
	.await
}

#[delete("/inference/agents/claude/state")]
pub fn clear_claude_state(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
) -> ApiNoContent {
	let _store = store(state);
	let adapter = claude_adapter()?;
	adapter.clear_provider_config().map_err(ApiError::from)?;
	Ok(NoContent)
}

#[delete("/inference/agents/codex/state")]
pub async fn clear_codex_state(
	_origin: TrustedLocalOrigin,
	state: &State<InferenceProviderState>,
) -> ApiNoContent {
	let store = store(state);
	run_blocking(move || {
		let adapter = codex_adapter()?;
		adapter
			.clear_active_provider(&store)
			.map_err(ApiError::from)?;
		Ok(NoContent)
	})
	.await
}

// This module drives inference routes through an INJECTED credential store
// (`InferenceProviderState::credentials`, wired via
// `crate::build_rocket_with_inference_credentials`) instead of the real OS
// keyring. That makes these tests deterministic on every OS/CI (no
// gnome-keyring/dbus needed) and lets the fail-closed test simulate a broken
// backend directly, instead of tampering with `DBUS_SESSION_BUS_ADDRESS` /
// racing the process-global mock keyring builder used elsewhere in this
// crate's tests (GitHub #15 P1-1). The platform-agnostic cascade LOGIC is
// covered separately at the `delete_provider_references` seam with mock
// stores (crates/inference/src/cascade.rs).
#[cfg(test)]
mod tests {
	use std::sync::{Arc, Mutex};

	use aghub_inference::{
		CreateInferenceProvider, InferenceProviderFormat,
		InferenceProviderRepository, InferenceProviderStore,
	};

	#[derive(Debug, Clone, Default)]
	struct MemoryCredentialStore {
		values: Arc<Mutex<std::collections::HashMap<String, String>>>,
	}
	impl aghub_inference::CredentialStore for MemoryCredentialStore {
		fn get_api_key(
			&self,
			id: &str,
		) -> aghub_inference::Result<Option<String>> {
			Ok(self.values.lock().unwrap().get(id).cloned())
		}
		fn set_api_key(
			&self,
			id: &str,
			key: &str,
		) -> aghub_inference::Result<()> {
			self.values
				.lock()
				.unwrap()
				.insert(id.to_string(), key.to_string());
			Ok(())
		}
		fn delete_api_key(&self, id: &str) -> aghub_inference::Result<()> {
			self.values.lock().unwrap().remove(id);
			Ok(())
		}
	}

	/// `get_api_key` always reports the backend as unreachable; `delete_api_key`
	/// panics — it must never run once the cascade's reachability precondition
	/// has already failed closed.
	#[derive(Debug, Clone, Default)]
	struct UnavailableCredentialStore;
	impl aghub_inference::CredentialStore for UnavailableCredentialStore {
		fn get_api_key(
			&self,
			_id: &str,
		) -> aghub_inference::Result<Option<String>> {
			Err(aghub_inference::InferenceProviderError::KeyringUnavailable(
				"no secret service provider or dbus session found".to_string(),
			))
		}
		fn set_api_key(
			&self,
			_id: &str,
			_key: &str,
		) -> aghub_inference::Result<()> {
			Ok(())
		}
		fn delete_api_key(&self, id: &str) -> aghub_inference::Result<()> {
			panic!(
				"delete_api_key({id}) must not run once the \
				 backend-reachability precondition has already failed"
			)
		}
	}

	/// Finding #2: route coverage for `delete_inference_provider`. Proves the
	/// DELETE route is mounted, drives the shared cascade end-to-end (its
	/// `*ProviderAdapter::global()` calls must not error), returns 204, and clears
	/// both the binding and the inventory row. The cascade's per-agent file
	/// cleanup branches are asserted directly at the `delete_provider_references`
	/// seam (see crates/inference/src/cascade.rs) with real temp-rooted file
	/// adapters — that is the discriminating coverage; this test pins the wiring.
	///
	/// HOME is repointed to an empty temp dir (under `test_env_lock`) so the
	/// cascade's `global()` adapters read throwaway config, never the real
	/// home. The route's credential store is INJECTED as the very
	/// `MemoryCredentialStore` used to seed the provider (GitHub #15 P1a: a
	/// hardcoded `NativeCredentialStore` made this test fail wherever no OS
	/// keyring is reachable, e.g. CI without gnome-keyring/dbus).
	#[test]
	fn delete_route_cascades_agent_bindings() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let home = tempfile::tempdir().unwrap();
		let old_home = std::env::var("HOME").ok();
		std::env::set_var("HOME", home.path());

		let data = tempfile::tempdir().unwrap();
		let credentials = MemoryCredentialStore::default();
		let seed = InferenceProviderStore::with_credentials(
			data.path(),
			credentials.clone(),
		);
		let provider = seed
			.create(CreateInferenceProvider {
				latin_name: "acme".to_string(),
				display_name: "Acme".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.example.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: Vec::new(),
			})
			.unwrap();
		seed.create_agent_binding("claude", &provider.id, None)
			.unwrap();
		assert_eq!(
			seed.list_agent_bindings("claude").unwrap().len(),
			1,
			"precondition: one claude binding references the provider"
		);

		let client = rocket::local::blocking::Client::tracked(
			crate::build_rocket_with_inference_credentials(
				rocket::Config::default(),
				data.path().to_path_buf(),
				Arc::new(credentials),
			),
		)
		.expect("rocket builds");
		let resp = client.delete("/api/v1/inference/providers/acme").dispatch();
		assert_eq!(
			resp.status(),
			rocket::http::Status::NoContent,
			"delete route returns 204"
		);

		assert!(
			seed.list_agent_bindings("claude").unwrap().is_empty(),
			"the delete route must cascade away the claude binding"
		);
		assert!(
			seed.get(&provider.id).is_err(),
			"the delete route must remove the inventory row too"
		);

		match old_home {
			Some(v) => std::env::set_var("HOME", v),
			None => std::env::remove_var("HOME"),
		}
	}

	/// Regression (GitHub #15 P1a/P1-1, Codex-found): `expected 204 got 500`
	/// when the credential backend is unreachable. Injects
	/// `UnavailableCredentialStore` (its `get_api_key` always errors) as the
	/// route's store, so `delete_provider_cascade`'s precondition read fails
	/// deterministically before any mutation — no `DBUS_SESSION_BUS_ADDRESS`
	/// tampering (that used to race the process-global keyring-builder mock
	/// installed by other tests in this crate, a real source of suite-order
	/// flakiness). Under the contract this must be a stable 503
	/// `KEYCHAIN_UNAVAILABLE` — not a 500, and NOT a 204 — with the claude
	/// binding and the inventory row both left exactly as they were.
	/// `UnavailableCredentialStore::delete_api_key` panics if ever called, so
	/// an accidental mutation fails this test loudly rather than silently.
	#[test]
	fn delete_route_fails_closed_when_keyring_backend_unreachable() {
		let data = tempfile::tempdir().unwrap();
		let seed = InferenceProviderStore::with_credentials(
			data.path(),
			MemoryCredentialStore::default(),
		);
		let provider = seed
			.create(CreateInferenceProvider {
				latin_name: "acmenobus".to_string(),
				display_name: "Acme".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.example.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: Vec::new(),
			})
			.unwrap();
		seed.create_agent_binding("claude", &provider.id, None)
			.unwrap();

		let client = rocket::local::blocking::Client::tracked(
			crate::build_rocket_with_inference_credentials(
				rocket::Config::default(),
				data.path().to_path_buf(),
				Arc::new(UnavailableCredentialStore),
			),
		)
		.expect("rocket builds");
		let resp = client
			.delete("/api/v1/inference/providers/acmenobus")
			.dispatch();

		assert_eq!(
			resp.status(),
			rocket::http::Status::ServiceUnavailable,
			"an unreachable backend must fail closed with a stable, \
			 retryable status — not 500, and not a fabricated 204"
		);
		let json: serde_json::Value =
			serde_json::from_str(&resp.into_string().unwrap()).unwrap();
		assert_eq!(json["code"], "KEYCHAIN_UNAVAILABLE");

		assert_eq!(
			seed.list_agent_bindings("claude").unwrap().len(),
			1,
			"the claude binding must survive a failed-precondition delete"
		);
		assert!(
			seed.get(&provider.id).is_ok(),
			"the provider row must survive a failed-precondition delete"
		);
	}
}
