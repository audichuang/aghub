use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateCredentialRequest {
	pub name: String,
	pub token: String,
}

/// Token is intentionally omitted from responses — write-only secret.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct CredentialResponse {
	pub id: String,
	pub name: String,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceCredentialBindingRequest {
	pub source: String,
	pub credential_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceCredentialBindingResponse {
	pub source: String,
	pub credential_id: Option<String>,
	pub credential_name: Option<String>,
}
