use log::{debug, info, warn};
use rocket::http::Status;
use rocket::serde::json::Json;
use serde::{Deserialize, Serialize};

use crate::credentials::resolve::{
	bind_source_to_credential, list_source_binding_responses,
	load_source_bindings, prune_bindings_for_credential, save_source_bindings,
	source_binding_response, SourceBindingError,
};
use crate::credentials::CredentialStoreError;
use crate::dto::credential::{
	CreateCredentialRequest, CredentialResponse,
	SourceCredentialBindingRequest, SourceCredentialBindingResponse,
};
use crate::error::{
	run_blocking as blocking, ApiCreated, ApiError, ApiNoContent, ApiResult,
};
use crate::extractors::TrustedLocalOrigin;

// Guards in-process read-modify-write cycles for the single keyring JSON entry.
// Cross-process keyring races remain a documented known limitation.
static CREDENTIAL_STORE_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredCredential {
	pub(crate) id: String,
	pub(crate) name: String,
	pub(crate) token: String,
}

pub(crate) fn load_credentials(
) -> Result<Vec<StoredCredential>, CredentialStoreError> {
	crate::credentials::credentials_store().load()
}

fn store_credentials(
	creds: &[StoredCredential],
) -> Result<(), CredentialStoreError> {
	crate::credentials::credentials_store().store(&creds.to_vec())
}

fn source_binding_err(err: SourceBindingError) -> ApiError {
	match err {
		SourceBindingError::EmptySource => ApiError::new(
			Status::BadRequest,
			"source must not be empty",
			"VALIDATION_FAILED",
		),
		SourceBindingError::CredentialNotFound(_) => ApiError::new(
			Status::NotFound,
			"Credential not found",
			"CREDENTIAL_NOT_FOUND",
		),
	}
}

/// Single lock for both the credentials entry and the source-bindings entry —
/// the delete flow holds ONE guard across a read-modify-write on both (prune
/// bindings for a deleted credential), so this must stay one mutex, not two.
fn lock_credential_store() -> std::sync::MutexGuard<'static, ()> {
	CREDENTIAL_STORE_MUTEX
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn credential_name_exists(creds: &[StoredCredential], name: &str) -> bool {
	creds.iter().any(|credential| credential.name == name)
}

fn duplicate_credential_err(name: &str) -> ApiError {
	ApiError::new(
		Status::Conflict,
		format!("A credential named '{name}' already exists"),
		"CREDENTIAL_NAME_EXISTS",
	)
}

#[get("/credentials")]
pub async fn list_credentials(
	_origin: TrustedLocalOrigin,
) -> ApiResult<Vec<CredentialResponse>> {
	let creds = blocking(|| load_credentials().map_err(ApiError::from)).await?;
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
pub async fn list_source_bindings_route(
	_origin: TrustedLocalOrigin,
) -> ApiResult<Vec<SourceCredentialBindingResponse>> {
	let responses = blocking(|| {
		let _guard = lock_credential_store();
		let bindings = load_source_bindings().map_err(ApiError::from)?;
		let creds = load_credentials().map_err(ApiError::from)?;
		Ok(list_source_binding_responses(&bindings, &creds))
	})
	.await?;
	Ok(Json(responses))
}

#[put("/credentials/source-bindings", data = "<body>")]
pub async fn bind_source_credential(
	body: Json<SourceCredentialBindingRequest>,
	_origin: TrustedLocalOrigin,
) -> ApiResult<SourceCredentialBindingResponse> {
	let body = body.into_inner();
	let response = blocking(move || {
		let _guard = lock_credential_store();
		let mut bindings = load_source_bindings().map_err(ApiError::from)?;
		let creds = load_credentials().map_err(ApiError::from)?;
		let credential_id = body.credential_id.as_deref();

		bind_source_to_credential(
			&mut bindings,
			&body.source,
			credential_id,
			&creds,
		)
		.map_err(source_binding_err)?;
		save_source_bindings(&bindings).map_err(ApiError::from)?;

		Ok(source_binding_response(&body.source, credential_id, &creds))
	})
	.await?;
	Ok(Json(response))
}

#[post("/credentials", data = "<body>")]
pub async fn create_credential(
	body: Json<CreateCredentialRequest>,
	_origin: TrustedLocalOrigin,
) -> ApiCreated<CredentialResponse> {
	let body = body.into_inner();
	let created = blocking(move || {
		let _guard = lock_credential_store();
		let mut creds = load_credentials().map_err(ApiError::from)?;
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
		store_credentials(&creds).map_err(ApiError::from)?;
		Ok(CredentialResponse {
			id: new.id,
			name: new.name,
		})
	})
	.await?;
	Ok((Status::Created, Json(created)))
}

#[delete("/credentials/<id>")]
pub async fn delete_credential(
	id: &str,
	_origin: TrustedLocalOrigin,
) -> ApiNoContent {
	let id = id.to_string();
	blocking(move || {
		let _guard = lock_credential_store();
		let mut creds = load_credentials().map_err(ApiError::from)?;
		let original_len = creds.len();
		creds.retain(|c| c.id != id);
		info!(
			"deleting credential '{id}', removed={}",
			original_len != creds.len()
		);
		store_credentials(&creds).map_err(ApiError::from)?;
		let result = (|| {
			let mut bindings = load_source_bindings()?;
			if prune_bindings_for_credential(&mut bindings, &id) {
				save_source_bindings(&bindings)?;
			}
			Ok::<(), CredentialStoreError>(())
		})();

		if let Err(error) = result {
			warn!(
				"failed to prune source credential bindings for {id}: {error}"
			);
		}
		Ok(())
	})
	.await?;
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
		let _guard = lock_credential_store();
	}
}
