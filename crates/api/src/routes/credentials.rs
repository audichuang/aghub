use log::{debug, info, warn};
use rocket::http::Status;
use rocket::serde::json::Json;
use serde::{Deserialize, Serialize};

use crate::credentials::resolve::{
	bind_source_to_credential, list_source_binding_responses,
	load_source_bindings, prune_bindings_for_credential, save_source_bindings,
	source_binding_response, SourceBindingError,
};
use crate::dto::credential::{
	CreateCredentialRequest, CredentialResponse,
	SourceCredentialBindingRequest, SourceCredentialBindingResponse,
};
use crate::error::{ApiCreated, ApiNoContent, ApiResult};
use crate::extractors::TrustedLocalOrigin;

const SERVICE: &str = "aghub";
const USER: &str = "github_credentials";

// Guards in-process read-modify-write cycles for the single keyring JSON entry.
// Cross-process keyring races remain a documented known limitation.
static SOURCE_BINDINGS_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredCredential {
	pub(crate) id: String,
	pub(crate) name: String,
	pub(crate) token: String,
}

fn get_entry() -> Result<keyring::Entry, String> {
	keyring::Entry::new(SERVICE, USER).map_err(|e| e.to_string())
}

pub(crate) fn load_credentials() -> Result<Vec<StoredCredential>, String> {
	let entry = get_entry()?;
	match entry.get_password() {
		Ok(json) => serde_json::from_str(&json).map_err(|e| e.to_string()),
		Err(keyring::Error::NoEntry) => Ok(vec![]),
		Err(e) => Err(e.to_string()),
	}
}

fn store_credentials(creds: &[StoredCredential]) -> Result<(), String> {
	let entry = get_entry()?;
	if creds.is_empty() {
		match entry.delete_credential() {
			Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
			Err(e) => Err(e.to_string()),
		}
	} else {
		let json = serde_json::to_string(creds).map_err(|e| e.to_string())?;
		entry.set_password(&json).map_err(|e| e.to_string())
	}
}

fn internal_err(msg: impl Into<String>) -> crate::error::ApiError {
	crate::error::ApiError::new(
		Status::InternalServerError,
		msg,
		"KEYCHAIN_ERROR",
	)
}

fn source_binding_err(err: SourceBindingError) -> crate::error::ApiError {
	match err {
		SourceBindingError::EmptySource => crate::error::ApiError::new(
			Status::BadRequest,
			"source must not be empty",
			"VALIDATION_FAILED",
		),
		SourceBindingError::CredentialNotFound(_) => {
			crate::error::ApiError::new(
				Status::NotFound,
				"Credential not found",
				"CREDENTIAL_NOT_FOUND",
			)
		}
	}
}

fn lock_source_bindings() -> std::sync::MutexGuard<'static, ()> {
	SOURCE_BINDINGS_MUTEX
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_credentials() -> std::sync::MutexGuard<'static, ()> {
	SOURCE_BINDINGS_MUTEX
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn credential_name_exists(creds: &[StoredCredential], name: &str) -> bool {
	creds.iter().any(|credential| credential.name == name)
}

fn duplicate_credential_err(name: &str) -> crate::error::ApiError {
	crate::error::ApiError::new(
		Status::Conflict,
		format!("A credential named '{name}' already exists"),
		"CREDENTIAL_NAME_EXISTS",
	)
}

#[get("/credentials")]
pub fn list_credentials(
	_origin: TrustedLocalOrigin,
) -> ApiResult<Vec<CredentialResponse>> {
	let creds = load_credentials().map_err(internal_err)?;
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
	_origin: TrustedLocalOrigin,
) -> ApiResult<Vec<SourceCredentialBindingResponse>> {
	let _guard = lock_source_bindings();
	let bindings = load_source_bindings().map_err(internal_err)?;
	let creds = load_credentials().map_err(internal_err)?;
	Ok(Json(list_source_binding_responses(&bindings, &creds)))
}

#[put("/credentials/source-bindings", data = "<body>")]
pub fn bind_source_credential(
	body: Json<SourceCredentialBindingRequest>,
	_origin: TrustedLocalOrigin,
) -> ApiResult<SourceCredentialBindingResponse> {
	let _guard = lock_source_bindings();
	let mut bindings = load_source_bindings().map_err(internal_err)?;
	let creds = load_credentials().map_err(internal_err)?;
	let credential_id = body.credential_id.as_deref();

	bind_source_to_credential(
		&mut bindings,
		&body.source,
		credential_id,
		&creds,
	)
	.map_err(source_binding_err)?;
	save_source_bindings(&bindings).map_err(internal_err)?;

	Ok(Json(source_binding_response(
		&body.source,
		credential_id,
		&creds,
	)))
}

#[post("/credentials", data = "<body>")]
pub fn create_credential(
	body: Json<CreateCredentialRequest>,
	_origin: TrustedLocalOrigin,
) -> ApiCreated<CredentialResponse> {
	let _guard = lock_credentials();
	let mut creds = load_credentials().map_err(internal_err)?;
	info!("creating credential '{}'", body.name);
	if credential_name_exists(&creds, &body.name) {
		return Err(duplicate_credential_err(&body.name));
	}
	let new = StoredCredential {
		id: uuid::Uuid::new_v4().to_string(),
		name: body.name.clone(),
		token: body.token.clone(),
	};
	creds.push(new.clone());
	store_credentials(&creds).map_err(internal_err)?;
	Ok((
		Status::Created,
		Json(CredentialResponse {
			id: new.id,
			name: new.name,
		}),
	))
}

#[delete("/credentials/<id>")]
pub fn delete_credential(
	id: &str,
	_origin: TrustedLocalOrigin,
) -> ApiNoContent {
	let _guard = lock_credentials();
	let mut creds = load_credentials().map_err(internal_err)?;
	let original_len = creds.len();
	creds.retain(|c| c.id != id);
	info!(
		"deleting credential '{id}', removed={}",
		original_len != creds.len()
	);
	store_credentials(&creds).map_err(internal_err)?;
	let result = (|| {
		let mut bindings = load_source_bindings()?;
		if prune_bindings_for_credential(&mut bindings, id) {
			save_source_bindings(&bindings)?;
		}
		Ok::<(), String>(())
	})();

	if let Err(error) = result {
		warn!("failed to prune source credential bindings for {id}: {error}");
	}
	Ok(rocket::response::status::NoContent)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn missing_binding_credential_maps_to_specific_not_found_code() {
		let err = source_binding_err(SourceBindingError::CredentialNotFound(
			"missing".into(),
		));

		assert_eq!(err.status, Status::NotFound);
		assert_eq!(err.body.code, "CREDENTIAL_NOT_FOUND");
	}

	#[test]
	fn duplicate_credential_name_rejected() {
		let creds = vec![StoredCredential {
			id: "c1".to_string(),
			name: "github.com".to_string(),
			token: "tok".to_string(),
		}];

		assert!(credential_name_exists(&creds, "github.com"));
		let err = duplicate_credential_err("github.com");
		assert_eq!(err.status, Status::Conflict);
		assert_eq!(err.body.code, "CREDENTIAL_NAME_EXISTS");
	}

	#[test]
	fn credential_store_lock_survives_poison_shape() {
		let _guard = lock_credentials();
	}
}
