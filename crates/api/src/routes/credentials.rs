use log::{debug, info};
use rocket::http::Status;
use rocket::serde::json::Json;
use serde::{Deserialize, Serialize};

use crate::credentials::resolve::{
	bind_source_to_credential, list_source_binding_responses,
	prune_bindings_for_credential, source_binding_response, SourceBindingError,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredCredential {
	pub(crate) id: String,
	pub(crate) name: String,
	pub(crate) token: String,
}

pub(crate) fn load_credentials(
) -> Result<Vec<StoredCredential>, CredentialStoreError> {
	Ok(crate::credentials::read_bundle()?.credentials)
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
		// One read, so the bindings and the credentials they reference come
		// from the SAME revision — two reads could straddle a write and render
		// a binding as pointing at a credential that no longer exists.
		let bundle =
			crate::credentials::read_bundle().map_err(ApiError::from)?;
		Ok(list_source_binding_responses(
			&bundle.bindings,
			&bundle.credentials,
		))
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
		let credential_id = body.credential_id.as_deref();
		let outcome = crate::credentials::update_bundle(|bundle| {
			// Split borrow: the validation needs the credential list while the
			// bindings are being mutated, and they are separate fields.
			let crate::credentials::CredentialBundle {
				credentials,
				bindings,
			} = bundle;
			bind_source_to_credential(
				bindings,
				&body.source,
				credential_id,
				credentials,
			)?;
			Ok(source_binding_response(
				&body.source,
				credential_id,
				credentials,
			))
		})
		.map_err(ApiError::from)?;
		outcome.map_err(source_binding_err)
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
		info!("creating credential '{}'", body.name);
		crate::credentials::update_bundle(|bundle| {
			if credential_name_exists(&bundle.credentials, &body.name) {
				return Err(duplicate_credential_err(&body.name));
			}
			let new = StoredCredential {
				id: uuid::Uuid::new_v4().to_string(),
				name: body.name.clone(),
				token: body.token.clone(),
			};
			bundle.credentials.push(new.clone());
			Ok(CredentialResponse {
				id: new.id,
				name: new.name,
			})
		})
		.map_err(ApiError::from)?
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
		// Credential removal and its binding prune are ONE write. Done as two,
		// a reader between them sees a binding referencing a credential that is
		// already gone; and the prune could fail on its own, stranding it.
		crate::credentials::update_bundle(|bundle| {
			let original_len = bundle.credentials.len();
			bundle.credentials.retain(|c| c.id != id);
			info!(
				"deleting credential '{id}', removed={}",
				original_len != bundle.credentials.len()
			);
			prune_bindings_for_credential(&mut bundle.bindings, &id);
			Ok::<(), std::convert::Infallible>(())
		})
		.map_err(ApiError::from)?
		.expect("the delete closure cannot reject");
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
}
