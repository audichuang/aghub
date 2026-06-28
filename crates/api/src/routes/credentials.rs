//! Source-credential CRUD routes. Thin Rocket wrappers over the shared
//! [`skill_update::SourceCredentialStore`]: storage, binding validation, and
//! the host-prefixed resolution logic all live in `skill-update` now (so the
//! CLI shares them). These handlers only resolve scope-free HTTP shapes, map
//! the store's [`CredentialError`]/[`BindError`] to API codes, and enforce the
//! HTTP-only 409 duplicate-name policy (the store stays HTTP-agnostic).

use log::{debug, info, warn};
use rocket::http::Status;
use rocket::serde::json::Json;

use skill_update::{
	BindError, CreateError, CredentialError, SourceCredentialStore,
};

use crate::credentials::resolve::{
	list_source_binding_responses, source_binding_response,
};
use crate::dto::credential::{
	CreateCredentialRequest, CredentialResponse,
	SourceCredentialBindingRequest, SourceCredentialBindingResponse,
};
use crate::error::{ApiCreated, ApiNoContent, ApiResult};

/// Map a real keychain/serde failure to a 500 with the stable `KEYCHAIN_ERROR`
/// code. The store's [`CredentialError`] replaces the old `String`/`None`
/// swallowing, so a genuine keyring failure now surfaces here instead of being
/// silently treated as "no credentials".
fn internal_err(err: CredentialError) -> crate::error::ApiError {
	crate::error::ApiError::new(
		Status::InternalServerError,
		err.to_string(),
		"KEYCHAIN_ERROR",
	)
}

fn source_binding_err(err: BindError) -> crate::error::ApiError {
	match err {
		BindError::EmptySource => crate::error::ApiError::new(
			Status::BadRequest,
			"source must not be empty",
			"VALIDATION_FAILED",
		),
		BindError::CredentialNotFound(_) => crate::error::ApiError::new(
			Status::NotFound,
			"Credential not found",
			"CREDENTIAL_NOT_FOUND",
		),
		// A real keychain/serde failure during bind is a 500, NOT a 404
		// (finding #1): conflating it with not-found hid keychain failures.
		BindError::Store(inner) => internal_err(inner),
	}
}

fn create_credential_err(err: CreateError) -> crate::error::ApiError {
	match err {
		CreateError::Duplicate(name) => crate::error::ApiError::new(
			Status::Conflict,
			format!("A credential named '{name}' already exists"),
			"CREDENTIAL_NAME_EXISTS",
		),
		CreateError::Store(inner) => internal_err(inner),
	}
}

#[get("/credentials")]
pub fn list_credentials() -> ApiResult<Vec<CredentialResponse>> {
	let creds = SourceCredentialStore.list().map_err(internal_err)?;
	debug!("loaded {} stored credentials", creds.len());
	Ok(Json(
		creds
			.into_iter()
			.map(|c| CredentialResponse {
				id: c.id,
				name: c.name,
			})
			.collect(),
	))
}

#[get("/credentials/source-bindings")]
pub fn list_source_bindings_route(
) -> ApiResult<Vec<SourceCredentialBindingResponse>> {
	let store = SourceCredentialStore;
	let bindings = store.list_bindings().map_err(internal_err)?;
	let creds = store.list().map_err(internal_err)?;
	Ok(Json(list_source_binding_responses(&bindings, &creds)))
}

#[put("/credentials/source-bindings", data = "<body>")]
pub fn bind_source_credential(
	body: Json<SourceCredentialBindingRequest>,
) -> ApiResult<SourceCredentialBindingResponse> {
	let store = SourceCredentialStore;
	let credential_id = body.credential_id.as_deref();

	// The store performs the load → validate → save read-modify-write under its
	// own keyring lock; a missing credential / empty source surfaces as a
	// `BindError`, mapped to the same 400/404 the route returned before.
	store
		.bind(&body.source, credential_id)
		.map_err(source_binding_err)?;

	// Re-read the credential list so the response can resolve the bound
	// credential's display name.
	let creds = store.list().map_err(internal_err)?;
	Ok(Json(source_binding_response(
		&body.source,
		credential_id,
		&creds,
	)))
}

#[post("/credentials", data = "<body>")]
pub fn create_credential(
	body: Json<CreateCredentialRequest>,
) -> ApiCreated<CredentialResponse> {
	let store = SourceCredentialStore;
	info!("creating credential '{}'", body.name);
	// The dup-name check + insert run under ONE keyring lock inside the store
	// (`create_unique`), so a concurrent create cannot slip a duplicate past the
	// check (finding #2). The HTTP-only 409 mapping lives here; a keychain
	// failure maps to 500/KEYCHAIN_ERROR.
	let new = store
		.create_unique(&body.name, &body.token)
		.map_err(create_credential_err)?;
	Ok((
		Status::Created,
		Json(CredentialResponse {
			id: new.id,
			name: new.name,
		}),
	))
}

#[delete("/credentials/<id>")]
pub fn delete_credential(id: &str) -> ApiNoContent {
	// `delete` removes the credential AND prunes any bindings that pointed at
	// it in a single locked read-modify-write. A binding-prune failure is
	// logged rather than failing the delete, preserving the old route's
	// behavior.
	match SourceCredentialStore.delete(id) {
		Ok(removed) => info!("deleting credential '{id}', removed={removed}"),
		Err(error) => {
			warn!("failed to fully delete credential {id}: {error}");
			return Err(internal_err(error));
		}
	}
	Ok(rocket::response::status::NoContent)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn missing_binding_credential_maps_to_specific_not_found_code() {
		let err =
			source_binding_err(BindError::CredentialNotFound("missing".into()));

		assert_eq!(err.status, Status::NotFound);
		assert_eq!(err.body.code, "CREDENTIAL_NOT_FOUND");
	}

	#[test]
	fn empty_source_binding_maps_to_validation_failed() {
		let err = source_binding_err(BindError::EmptySource);

		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "VALIDATION_FAILED");
		assert_eq!(err.body.error, "source must not be empty");
	}

	#[test]
	fn duplicate_credential_name_rejected() {
		let err =
			create_credential_err(CreateError::Duplicate("github.com".into()));
		assert_eq!(err.status, Status::Conflict);
		assert_eq!(err.body.code, "CREDENTIAL_NAME_EXISTS");
		assert_eq!(
			err.body.error,
			"A credential named 'github.com' already exists"
		);
	}

	#[test]
	fn create_store_failure_maps_to_keychain_error() {
		// finding #2/#1: a keychain failure during create is a 500, not a 409.
		let err = create_credential_err(CreateError::Store(
			CredentialError::Keyring("boom".into()),
		));
		assert_eq!(err.status, Status::InternalServerError);
		assert_eq!(err.body.code, "KEYCHAIN_ERROR");
	}

	#[test]
	fn bind_store_failure_maps_to_keychain_error_not_not_found() {
		// finding #1: a keychain failure during bind must surface as
		// 500/KEYCHAIN_ERROR, never the misleading 404/CREDENTIAL_NOT_FOUND.
		let err = source_binding_err(BindError::Store(
			CredentialError::Keyring("boom".into()),
		));
		assert_eq!(err.status, Status::InternalServerError);
		assert_eq!(err.body.code, "KEYCHAIN_ERROR");
	}

	#[test]
	fn keyring_error_maps_to_keychain_error_code() {
		let err = internal_err(CredentialError::Keyring("boom".into()));
		assert_eq!(err.status, Status::InternalServerError);
		assert_eq!(err.body.code, "KEYCHAIN_ERROR");
		assert_eq!(err.body.error, "keychain error: boom");
	}

	#[test]
	fn serde_error_maps_to_keychain_error_code() {
		let err = internal_err(CredentialError::Serde("bad json".into()));
		assert_eq!(err.status, Status::InternalServerError);
		assert_eq!(err.body.code, "KEYCHAIN_ERROR");
		assert_eq!(err.body.error, "credential serialization error: bad json");
	}
}
