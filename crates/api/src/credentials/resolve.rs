//! DTO mappers for source→credential bindings.
//!
//! The storage, resolution, and host-prefixed binding logic moved down to
//! [`skill_update::credentials`] so the CLI shares it. What stays here is the
//! API-only projection of a binding into its ts-rs DTO
//! (`SourceCredentialBindingResponse`), since `skill-update` has no ts-rs
//! wiring.

use skill_update::{SourceBindings, StoredCredential};

use crate::dto::credential::SourceCredentialBindingResponse;

pub(crate) fn list_source_binding_responses(
	bindings: &SourceBindings,
	creds: &[StoredCredential],
) -> Vec<SourceCredentialBindingResponse> {
	bindings
		.0
		.iter()
		.map(|(source, credential_id)| {
			source_binding_response(source, Some(credential_id), creds)
		})
		.collect()
}

pub(crate) fn source_binding_response(
	source: &str,
	credential_id: Option<&str>,
	creds: &[StoredCredential],
) -> SourceCredentialBindingResponse {
	let credential_name = credential_id.and_then(|credential_id| {
		creds
			.iter()
			.find(|c| c.id == credential_id)
			.map(|c| c.name.clone())
	});

	SourceCredentialBindingResponse {
		source: source.to_string(),
		credential_id: credential_id.map(str::to_string),
		credential_name,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn cred(id: &str, name: &str, token: &str) -> StoredCredential {
		StoredCredential {
			id: id.into(),
			name: name.into(),
			token: token.into(),
		}
	}

	#[test]
	fn list_binding_responses_with_existing_credential_name() {
		let mut b = SourceBindings::default();
		b.0.insert("b/source".into(), "c2".into());
		b.0.insert("a/source".into(), "c1".into());
		let creds = vec![
			cred("c1", "github.com", "TOK1"),
			cred("c2", "gitlab.com", "TOK2"),
		];

		let responses = list_source_binding_responses(&b, &creds);

		assert_eq!(responses[0].source, "a/source");
		assert_eq!(responses[0].credential_id.as_deref(), Some("c1"));
		assert_eq!(responses[0].credential_name.as_deref(), Some("github.com"));
		assert_eq!(responses[1].source, "b/source");
		assert_eq!(responses[1].credential_name.as_deref(), Some("gitlab.com"));
	}

	#[test]
	fn list_binding_responses_with_missing_credential_name() {
		let mut b = SourceBindings::default();
		b.0.insert("owner/repo".into(), "gone".into());

		let responses = list_source_binding_responses(&b, &[]);

		assert_eq!(responses.len(), 1);
		assert_eq!(responses[0].credential_id.as_deref(), Some("gone"));
		assert_eq!(responses[0].credential_name, None);
	}

	#[test]
	fn source_binding_response_without_credential_id_is_unbound() {
		let resp = source_binding_response("owner/repo", None, &[]);
		assert_eq!(resp.source, "owner/repo");
		assert_eq!(resp.credential_id, None);
		assert_eq!(resp.credential_name, None);
	}
}
