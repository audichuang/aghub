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

#[get("/credentials")]
pub fn list_credentials() -> ApiResult<Vec<CredentialResponse>> {
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
) -> ApiResult<Vec<SourceCredentialBindingResponse>> {
	let _guard = lock_source_bindings();
	let bindings = load_source_bindings().map_err(internal_err)?;
	let creds = load_credentials().map_err(internal_err)?;
	Ok(Json(list_source_binding_responses(&bindings, &creds)))
}

#[put("/credentials/source-bindings", data = "<body>")]
pub fn bind_source_credential(
	body: Json<SourceCredentialBindingRequest>,
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
) -> ApiCreated<CredentialResponse> {
	let mut creds = load_credentials().map_err(internal_err)?;
	info!("creating credential '{}'", body.name);
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

fn prune_deleted_credential_bindings(id: &str) {
	let _guard = lock_source_bindings();
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
}

#[delete("/credentials/<id>")]
pub fn delete_credential(id: &str) -> ApiNoContent {
	let mut creds = load_credentials().map_err(internal_err)?;
	let original_len = creds.len();
	creds.retain(|c| c.id != id);
	info!(
		"deleting credential '{id}', removed={}",
		original_len != creds.len()
	);
	store_credentials(&creds).map_err(internal_err)?;
	prune_deleted_credential_bindings(id);
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
}
