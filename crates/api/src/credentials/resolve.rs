//! Resolution order: (1) keyring source→credential_id binding, (2) keychain by
//! host, (3) None → caller yields Uncheckable{auth}. Tokens never touch the lock.

use crate::dto::credential::SourceCredentialBindingResponse;

const SERVICE: &str = "aghub";
const BINDINGS_USER: &str = "skill_source_bindings"; // SERVICE = "aghub"

/// In-memory representation for tests; backed by a single keyring JSON entry.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SourceBindings(
	pub std::collections::BTreeMap<String, String>,
); // source → credential_id

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceBindingError {
	EmptySource,
	CredentialNotFound(String),
}

/// Resolve a token for a skill source, in priority order:
/// 1. An explicit source→credential_id binding (looked up in `bindings`).
/// 2. A credential whose `name` matches the requested `host`.
/// 3. `None` — the caller surfaces `Uncheckable { reason: auth }`.
///
/// Tokens returned here are used only for in-memory fetches; they are never
/// written to any committed lock file.
pub(crate) fn resolve_token_for_source(
	source: &str,
	host: Option<&str>,
	bindings: &SourceBindings,
	creds: &[crate::routes::credentials::StoredCredential],
) -> Option<String> {
	// (1) Explicit binding: source → credential_id.
	if let Some(cred_id) = bindings.0.get(source) {
		if let Some(c) = creds.iter().find(|c| &c.id == cred_id) {
			return Some(c.token.clone());
		}
	}

	// (2) Host fallback: a credential whose name matches the host.
	if let Some(host) = host {
		if let Some(c) = creds.iter().find(|c| c.name == host) {
			return Some(c.token.clone());
		}
	}

	// (3) No match.
	None
}

pub(crate) fn bind_source_to_credential(
	bindings: &mut SourceBindings,
	source: &str,
	credential_id: Option<&str>,
	creds: &[crate::routes::credentials::StoredCredential],
) -> Result<(), SourceBindingError> {
	if source.trim().is_empty() {
		return Err(SourceBindingError::EmptySource);
	}

	if let Some(credential_id) = credential_id {
		if !creds.iter().any(|c| c.id == credential_id) {
			return Err(SourceBindingError::CredentialNotFound(
				credential_id.to_string(),
			));
		}
		bindings
			.0
			.insert(source.to_string(), credential_id.to_string());
	} else {
		bindings.0.remove(source);
	}

	Ok(())
}

pub(crate) fn list_source_binding_responses(
	bindings: &SourceBindings,
	creds: &[crate::routes::credentials::StoredCredential],
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
	creds: &[crate::routes::credentials::StoredCredential],
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

pub(crate) fn prune_bindings_for_credential(
	bindings: &mut SourceBindings,
	credential_id: &str,
) -> bool {
	let original_len = bindings.0.len();
	bindings.0.retain(|_, id| id != credential_id);
	bindings.0.len() != original_len
}

fn bindings_entry() -> Result<keyring::Entry, String> {
	keyring::Entry::new(SERVICE, BINDINGS_USER).map_err(|e| e.to_string())
}

/// Load the source→credential_id bindings from the `skill_source_bindings`
/// keyring entry. Mirrors `routes::credentials::load_credentials`.
pub(crate) fn load_source_bindings() -> Result<SourceBindings, String> {
	let entry = bindings_entry()?;
	match entry.get_password() {
		Ok(json) => serde_json::from_str(&json).map_err(|e| e.to_string()),
		Err(keyring::Error::NoEntry) => Ok(SourceBindings::default()),
		Err(e) => Err(e.to_string()),
	}
}

/// Persist the source→credential_id bindings to the keyring entry. An empty
/// map deletes the entry. Mirrors `routes::credentials` storage behavior.
pub(crate) fn save_source_bindings(
	bindings: &SourceBindings,
) -> Result<(), String> {
	let entry = bindings_entry()?;
	if bindings.0.is_empty() {
		match entry.delete_credential() {
			Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
			Err(e) => Err(e.to_string()),
		}
	} else {
		let json =
			serde_json::to_string(bindings).map_err(|e| e.to_string())?;
		entry.set_password(&json).map_err(|e| e.to_string())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::routes::credentials::StoredCredential;

	fn cred(id: &str, name: &str, token: &str) -> StoredCredential {
		StoredCredential {
			id: id.into(),
			name: name.into(),
			token: token.into(),
		}
	}

	#[test]
	fn binding_wins_first() {
		let mut b = SourceBindings::default();
		b.0.insert("o/r".into(), "c1".into());
		let creds = vec![
			cred("c1", "github.com", "TOK1"),
			cred("c2", "github.com", "TOK2"),
		];
		assert_eq!(
			resolve_token_for_source("o/r", Some("github.com"), &b, &creds),
			Some("TOK1".into())
		);
	}

	#[test]
	fn falls_back_to_host_match() {
		let b = SourceBindings::default();
		let creds = vec![cred("c2", "github.com", "TOK2")];
		assert_eq!(
			resolve_token_for_source("o/r", Some("github.com"), &b, &creds),
			Some("TOK2".into())
		);
	}

	#[test]
	fn none_when_no_match() {
		let b = SourceBindings::default();
		let creds = vec![cred("c2", "gitlab.com", "X")];
		assert_eq!(
			resolve_token_for_source("o/r", Some("github.com"), &b, &creds),
			None
		);
	}

	#[test]
	fn binding_to_missing_cred_falls_through_to_host() {
		// Binding points at a credential id that no longer exists; resolution
		// should fall through to the host match rather than return None.
		let mut b = SourceBindings::default();
		b.0.insert("o/r".into(), "gone".into());
		let creds = vec![cred("c2", "github.com", "TOK2")];
		assert_eq!(
			resolve_token_for_source("o/r", Some("github.com"), &b, &creds),
			Some("TOK2".into())
		);
	}

	#[test]
	fn none_when_no_host_and_no_binding() {
		let b = SourceBindings::default();
		let creds = vec![cred("c2", "github.com", "TOK2")];
		assert_eq!(resolve_token_for_source("o/r", None, &b, &creds), None);
	}

	#[test]
	fn set_binding_in_memory() {
		let mut b = SourceBindings::default();
		let creds = vec![cred("c1", "github.com", "TOK1")];

		bind_source_to_credential(
			&mut b,
			"https://github.com/owner/repo",
			Some("c1"),
			&creds,
		)
		.unwrap();

		assert_eq!(
			b.0.get("https://github.com/owner/repo").map(String::as_str),
			Some("c1")
		);
	}

	#[test]
	fn clear_binding_in_memory() {
		let mut b = SourceBindings::default();
		b.0.insert("owner/repo".into(), "c1".into());

		bind_source_to_credential(&mut b, "owner/repo", None, &[]).unwrap();

		assert!(!b.0.contains_key("owner/repo"));
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
	fn unknown_credential_id_does_not_mutate_bindings() {
		let mut b = SourceBindings::default();
		b.0.insert("owner/repo".into(), "c1".into());
		let before = b.0.clone();
		let creds = vec![cred("c1", "github.com", "TOK1")];

		let err = bind_source_to_credential(
			&mut b,
			"owner/repo",
			Some("gone"),
			&creds,
		)
		.unwrap_err();

		assert_eq!(err, SourceBindingError::CredentialNotFound("gone".into()));
		assert_eq!(b.0, before);
	}

	#[test]
	fn deleting_credential_prunes_matching_bindings_in_memory() {
		let mut b = SourceBindings::default();
		b.0.insert("first".into(), "c1".into());
		b.0.insert("second".into(), "c2".into());

		assert!(prune_bindings_for_credential(&mut b, "c1"));

		assert!(!b.0.contains_key("first"));
		assert_eq!(b.0.get("second").map(String::as_str), Some("c2"));
	}
}
