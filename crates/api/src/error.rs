use aghub_core::errors::ConfigError;
use aghub_inference::InferenceProviderError;
use rocket::http::{ContentType, Status};
use rocket::response::{self, Responder};
use rocket::serde::json::serde_json;
use serde::Serialize;

#[derive(Serialize)]
pub struct ErrorBody {
	pub error: String,
	pub code: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub rejected_targets: Option<Vec<aghub_core::errors::RejectedTarget>>,
}

/// Fixed, safe message for "the OS credential backend is unreachable". The
/// cause is logged server-side only — backend detail can carry internal paths.
/// Shared by every keyring-touching surface so they answer identically.
const KEYCHAIN_UNAVAILABLE_MSG: &str =
	"Credential storage is temporarily unavailable. Please try again.";

pub struct ApiError {
	pub status: Status,
	pub body: ErrorBody,
}

impl ApiError {
	pub fn new(
		status: Status,
		error: impl Into<String>,
		code: &'static str,
	) -> Self {
		Self {
			status,
			body: ErrorBody {
				error: error.into(),
				code,
				rejected_targets: None,
			},
		}
	}

	pub fn with_rejected_targets(
		status: Status,
		error: impl Into<String>,
		code: &'static str,
		rejected_targets: Option<Vec<aghub_core::errors::RejectedTarget>>,
	) -> Self {
		Self {
			status,
			body: ErrorBody {
				error: error.into(),
				code,
				rejected_targets,
			},
		}
	}

	pub fn internal(error: impl Into<String>) -> Self {
		Self::new(Status::InternalServerError, error, "INTERNAL_ERROR")
	}

	pub fn bad_request(error: impl Into<String>) -> Self {
		Self::new(Status::BadRequest, error, "BAD_REQUEST")
	}

	pub fn not_found(error: impl Into<String>) -> Self {
		Self::new(Status::NotFound, error, "NOT_FOUND")
	}

	pub fn from_join_error(
		error: tokio::task::JoinError,
		message: &'static str,
		code: &'static str,
	) -> Self {
		// Neither the response nor our log carries the panic payload
		// (`JoinError::Display` embeds it and it can hold paths). The default
		// panic hook still prints it to stderr — deliberately left alone.
		log::error!(
			"{code}: blocking task failed (is_panic={}, is_cancelled={})",
			error.is_panic(),
			error.is_cancelled()
		);
		Self::new(Status::InternalServerError, message, code)
	}
}

impl ApiError {
	pub fn from_config_ref(e: &ConfigError) -> Self {
		// The machine code comes from `aghub_core::error_codes`, the ONE place
		// it is defined, so the CLI's `--json` errors and this response speak
		// the same vocabulary. Only the HTTP status and the message wording are
		// decided here — they are the genuinely transport-specific half.
		let code = aghub_core::error_codes::wire_code(e);
		match e {
			ConfigError::ResourceNotFound {
				resource_type,
				name,
			} => ApiError::new(
				Status::NotFound,
				format!("{resource_type} '{name}' not found"),
				code,
			),
			ConfigError::ResourceExists {
				resource_type,
				name,
			} => ApiError::new(
				Status::Conflict,
				format!("{resource_type} '{name}' already exists"),
				code,
			),
			ConfigError::NotFound { path } => ApiError::new(
				Status::NotFound,
				format!("Config file not found: {}", path.display()),
				code,
			),
			ConfigError::UnsupportedOperation {
				message,
				rejected_targets,
			} => ApiError::with_rejected_targets(
				Status::UnprocessableEntity,
				message.clone(),
				code,
				rejected_targets.clone(),
			),
			ConfigError::ValidationFailed(msg) => {
				ApiError::new(Status::UnprocessableEntity, msg.clone(), code)
			}
			ConfigError::InvalidConfig(msg) => {
				ApiError::new(Status::BadRequest, msg.clone(), code)
			}
			ConfigError::InvalidConfigWithTargets {
				message,
				rejected_targets,
			} => ApiError::with_rejected_targets(
				Status::BadRequest,
				message.clone(),
				code,
				rejected_targets.clone(),
			),
			ConfigError::ManagedResource(msg) => {
				ApiError::new(Status::BadRequest, msg.clone(), code)
			}
			ConfigError::Json(e) => {
				ApiError::new(Status::BadRequest, e.to_string(), code)
			}
			// Mutation-lock contention (`Io(WouldBlock)`, produced only by
			// `skill::lock::guard`) is a RETRYABLE 409, not a 500.
			// `lock_unavailable` (no lock possible at all) stays 500 on purpose.
			ConfigError::Io(e)
				if e.kind() == std::io::ErrorKind::WouldBlock =>
			{
				ApiError::new(Status::Conflict, e.to_string(), code)
			}
			ConfigError::Io(e) => {
				ApiError::new(Status::InternalServerError, e.to_string(), code)
			}
		}
	}
}

impl From<&ConfigError> for ApiError {
	fn from(e: &ConfigError) -> Self {
		Self::from_config_ref(e)
	}
}

impl From<ConfigError> for ApiError {
	fn from(e: ConfigError) -> Self {
		Self::from_config_ref(&e)
	}
}

impl From<InferenceProviderError> for ApiError {
	fn from(e: InferenceProviderError) -> Self {
		match e {
			InferenceProviderError::EmptyName
			| InferenceProviderError::EmptyAgentProviderId
			| InferenceProviderError::EmptyModelName
			| InferenceProviderError::EmptyApiBaseUrl
			| InferenceProviderError::EmptyApiKey
			| InferenceProviderError::InvalidFormat(_)
			| InferenceProviderError::InvalidLatinName(_)
			| InferenceProviderError::UnsupportedAgentProviderCapability {
				..
			} => ApiError::new(
				Status::BadRequest,
				e.to_string(),
				"INVALID_PARAM",
			),
			InferenceProviderError::InvalidAgentProviderConfig {
				agent_id,
				message,
				..
			} => ApiError::new(
				Status::BadRequest,
				format!("invalid {agent_id} provider config: {message}"),
				"INVALID_PARAM",
			),
			InferenceProviderError::InvalidAgentCredentialStore {
				agent_id,
				message,
				..
			} => ApiError::new(
				Status::BadRequest,
				format!("invalid {agent_id} credential store: {message}"),
				"INVALID_PARAM",
			),
			InferenceProviderError::AlreadyExists(_)
			| InferenceProviderError::ModelAlreadyExists(_) => ApiError::new(
				Status::Conflict,
				e.to_string(),
				"RESOURCE_EXISTS",
			),
			InferenceProviderError::NotFound(_) => ApiError::new(
				Status::NotFound,
				e.to_string(),
				"RESOURCE_NOT_FOUND",
			),
			InferenceProviderError::Keyring(_) => ApiError::new(
				Status::InternalServerError,
				e.to_string(),
				"KEYCHAIN_ERROR",
			),
			InferenceProviderError::KeyringUnavailable(_) => {
				log::warn!("credential backend unavailable: {e}");
				ApiError::new(
					Status::ServiceUnavailable,
					KEYCHAIN_UNAVAILABLE_MSG,
					"KEYCHAIN_UNAVAILABLE",
				)
			}
			InferenceProviderError::Io(_)
			| InferenceProviderError::Database(_) => ApiError::new(
				Status::InternalServerError,
				e.to_string(),
				"INFERENCE_PROVIDER_STORE_ERROR",
			),
		}
	}
}

impl From<crate::credentials::CredentialStoreError> for ApiError {
	fn from(e: crate::credentials::CredentialStoreError) -> Self {
		match e {
			crate::credentials::CredentialStoreError::Unavailable(detail) => {
				log::warn!("credential backend unavailable: {detail}");
				ApiError::new(
					Status::ServiceUnavailable,
					KEYCHAIN_UNAVAILABLE_MSG,
					"KEYCHAIN_UNAVAILABLE",
				)
			}
			crate::credentials::CredentialStoreError::Other(message) => {
				ApiError::new(
					Status::InternalServerError,
					message,
					"KEYCHAIN_ERROR",
				)
			}
		}
	}
}

/// Run `f` on Rocket's blocking-task pool and map a panicked/cancelled task
/// to a safe, generic error.
///
/// Every route whose body does OS keyring I/O MUST go through this: Rocket 0.5
/// does not offload a sync handler itself (see the `keyring` feature comment in
/// `crates/api/Cargo.toml`).
pub(crate) async fn run_blocking<F, T>(f: F) -> Result<T, ApiError>
where
	F: FnOnce() -> Result<T, ApiError> + Send + 'static,
	T: Send + 'static,
{
	tokio::task::spawn_blocking(f).await.map_err(|e| {
		ApiError::from_join_error(
			e,
			"Credential operation failed",
			"CREDENTIAL_TASK_ERROR",
		)
	})?
}

impl<'r> Responder<'r, 'static> for ApiError {
	fn respond_to(
		self,
		_: &'r rocket::Request<'_>,
	) -> response::Result<'static> {
		let body = serde_json::to_string(&self.body).unwrap_or_else(|_| {
			r#"{"error":"Internal error","code":"INTERNAL_ERROR"}"#.to_string()
		});
		rocket::Response::build()
			.status(self.status)
			.header(ContentType::JSON)
			.sized_body(body.len(), std::io::Cursor::new(body))
			.ok()
	}
}

pub type ApiResult<T> = Result<rocket::serde::json::Json<T>, ApiError>;
pub type ApiCreated<T> =
	Result<(Status, rocket::serde::json::Json<T>), ApiError>;
pub type ApiNoContent = Result<rocket::response::status::NoContent, ApiError>;

#[cfg(test)]
mod tests {
	use super::ApiError;

	#[tokio::test]
	async fn join_error_response_omits_panic_payload() {
		const PANIC_PAYLOAD: &str =
			"secret panic detail at /home/private/repository";
		let join_error = tokio::task::spawn_blocking(|| {
			panic!("{PANIC_PAYLOAD}");
		})
		.await
		.unwrap_err();

		let error = ApiError::from_join_error(
			join_error,
			"Clone task failed",
			"CLONE_ERROR",
		);

		assert_eq!(error.body.error, "Clone task failed");
		assert_eq!(error.body.code, "CLONE_ERROR");
		assert!(!error.body.error.contains(PANIC_PAYLOAD));
	}
}
