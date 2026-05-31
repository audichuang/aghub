//! Resolution order: (1) keyring source→credential_id binding, (2) keychain by
//! host, (3) None → caller yields Uncheckable{auth}. Tokens never touch the lock.

const SERVICE: &str = "aghub";
const BINDINGS_USER: &str = "skill_source_bindings"; // SERVICE = "aghub"

/// In-memory representation for tests; backed by a single keyring JSON entry.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SourceBindings(
	pub std::collections::BTreeMap<String, String>,
); // source → credential_id

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
}
