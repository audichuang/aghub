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
}

/// Fixed, safe-to-expose message for "the OS credential backend itself is
/// unreachable" (Linux secret-service with no D-Bus session, a locked
/// keychain with no prompt path, ...). The underlying cause is logged
/// server-side only — never put backend/platform detail (which can include
/// internal paths) in a response body. Shared by every keyring-touching
/// surface (github credentials, source bindings, inference provider keys)
/// so they never diverge on status/code/message for the same failure class.
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
		// Boundary: the HTTP RESPONSE (fixed safe `message`) and OUR structured
		// log records carry NO panic payload. `JoinError::Display` embeds the
		// panic payload (which can contain paths/internal detail), so never log
		// it raw — record only the redacted classification below.
		// NOT in scope: the process-global default Rust panic hook still prints
		// the panic message to stderr when the blocking task panics. That is
		// standard runtime behavior, server-side, and needed for debugging; we
		// deliberately do NOT install a panic-suppressing global hook (it would
		// harm observability for a non-client-facing stderr line).
		log::error!(
			"{code}: blocking task failed (is_panic={}, is_cancelled={})",
			error.is_panic(),
			error.is_cancelled()
		);
		Self::new(Status::InternalServerError, message, code)
	}
}

impl From<ConfigError> for ApiError {
	fn from(e: ConfigError) -> Self {
		match e {
			ConfigError::ResourceNotFound {
				resource_type,
				name,
			} => ApiError::new(
				Status::NotFound,
				format!("{resource_type} '{name}' not found"),
				"RESOURCE_NOT_FOUND",
			),
			ConfigError::ResourceExists {
				resource_type,
				name,
			} => ApiError::new(
				Status::Conflict,
				format!("{resource_type} '{name}' already exists"),
				"RESOURCE_EXISTS",
			),
			ConfigError::NotFound { path } => ApiError::new(
				Status::NotFound,
				format!("Config file not found: {}", path.display()),
				"CONFIG_NOT_FOUND",
			),
			ConfigError::UnsupportedOperation(msg) => ApiError::new(
				Status::UnprocessableEntity,
				msg,
				"UNSUPPORTED_OPERATION",
			),
			ConfigError::ValidationFailed(msg) => ApiError::new(
				Status::UnprocessableEntity,
				msg,
				"VALIDATION_FAILED",
			),
			ConfigError::InvalidConfig(msg) => {
				ApiError::new(Status::BadRequest, msg, "INVALID_CONFIG")
			}
			ConfigError::Json(e) => ApiError::new(
				Status::BadRequest,
				e.to_string(),
				"JSON_PARSE_ERROR",
			),
			ConfigError::Io(e) => ApiError::new(
				Status::InternalServerError,
				e.to_string(),
				"IO_ERROR",
			),
		}
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
			| InferenceProviderError::Database(_)
			| InferenceProviderError::AppDataDir(_) => ApiError::new(
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
/// Every route whose body performs OS keyring I/O (secret-service on Linux,
/// or any other slow synchronous credential-store call) MUST go through this
/// instead of running that I/O inline on the route's async worker thread —
/// Rocket 0.5 does not `spawn_blocking` a sync handler fn on its own (see the
/// `keyring` feature comment in `crates/api/Cargo.toml`). Shared by every
/// keyring-touching route module (`routes::credentials`, `routes::inference`)
/// so they don't each hand-roll the same `spawn_blocking` + error-mapping
/// boilerplate.
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
