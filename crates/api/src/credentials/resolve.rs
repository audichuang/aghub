//! Resolution order: (1) keyring source→credential_id binding, (2) keychain by
//! host, (3) None → caller yields Uncheckable{auth}. Tokens never touch the lock.

use crate::dto::credential::SourceCredentialBindingResponse;
use std::collections::BTreeSet;

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
	//
	// Security: keys are host-prefixed for resolvable URLs so that a binding
	// for `owner/repo` on `github.com` cannot match a lookup for the same
	// `owner/repo` shape on `gitlab.com`. For unresolvable sources (e.g. local
	// paths) we use a `local::` sentinel prefix so two such sources still
	// match each other, but cannot collide with host-prefixed keys.
	let source_keys = lookup_keys(source);
	if let Some(cred_id) = bindings
		.0
		.iter()
		.find(|(bound_source, _)| {
			binding_keys_match_lookup(bound_source, &source_keys)
		})
		.map(|(_, cred_id)| cred_id)
	{
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
	let key = canonical_binding_key(source);

	if let Some(credential_id) = credential_id {
		if !creds.iter().any(|c| c.id == credential_id) {
			return Err(SourceBindingError::CredentialNotFound(
				credential_id.to_string(),
			));
		}
		remove_equivalent_bindings(bindings, source);
		bindings.0.insert(key, credential_id.to_string());
	} else {
		remove_equivalent_bindings(bindings, source);
	}

	Ok(())
}

fn canonical_binding_key(source: &str) -> String {
	source.trim().to_string()
}

/// Prefix for keys derived from sources that did not resolve to a host (e.g.
/// a local path). The sentinel is non-empty so it can never collide with a
/// host-prefixed key, but the post-`::` value still lets two unresolvable
/// sources with the same trimmed string match each other.
const LOCAL_KEY_PREFIX: &str = "local::";

/// Build the set of lookup keys for a source. Two sources match iff their
/// key sets intersect.
///
/// - Resolvable URL: keys are `host::<variant>` for every variant of the
///   source the resolver knows about (bare `owner/repo`, `https://…`,
///   `git@…`, etc.). The host is taken from the resolved source (which is
///   normalised to lowercase) so a binding for `owner/repo` on `github.com`
///   never matches a lookup for the same `owner/repo` shape on `gitlab.com`.
/// - Unresolvable source (e.g. local path): the single key is
///   `local::<trimmed>`. Two unresolvable sources with the same trimmed
///   string still match (preserving the legacy behaviour), but they cannot
///   collide with any host-prefixed key.
pub(crate) fn lookup_keys(source: &str) -> BTreeSet<String> {
	let mut keys = BTreeSet::new();
	let trimmed = canonical_binding_key(source);
	if trimmed.is_empty() {
		return keys;
	}

	if let Ok(resolved) = aghub_git::resolve_remote_source(&trimmed) {
		if let Some(host) = resolved.host.as_deref() {
			let prefix = |key: &str| format!("{host}::{key}");
			keys.insert(prefix(&resolved.source));
			keys.insert(prefix(&resolved.source_url));
			keys.insert(prefix(&resolved.clone_url));
			keys.insert(prefix(&resolved.lock_source()));
			return keys;
		}
	}

	keys.insert(format!("{LOCAL_KEY_PREFIX}{trimmed}"));
	keys
}

/// `true` iff a stored binding's keys intersect with the lookup's keys.
pub(crate) fn binding_keys_match_lookup(
	bound_source: &str,
	source_keys: &BTreeSet<String>,
) -> bool {
	if source_keys.is_empty() {
		return false;
	}
	let bound_keys = lookup_keys(bound_source);
	bound_keys.iter().any(|key| source_keys.contains(key))
}

fn remove_equivalent_bindings(bindings: &mut SourceBindings, source: &str) {
	let source_keys = lookup_keys(source);
	bindings.0.retain(|bound_source, _| {
		!binding_keys_match_lookup(bound_source, &source_keys)
	});
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

/// Load the source→credential_id bindings from the `skill_source_bindings`
/// keyring entry. Mirrors `routes::credentials::load_credentials`.
pub(crate) fn load_source_bindings(
) -> Result<SourceBindings, crate::credentials::CredentialStoreError> {
	Ok(crate::credentials::load_bundle()?.bindings)
}

/// Persist the source→credential_id bindings to the keyring entry. An empty
/// map deletes the entry. Mirrors `routes::credentials` storage behavior.
/// Replace the bindings, preserving the credentials stored alongside them.
/// Read-modify-write on the shared bundle — see `store_credentials` for the
/// locking that makes the pair safe.
pub(crate) fn save_source_bindings(
	bindings: &SourceBindings,
) -> Result<(), crate::credentials::CredentialStoreError> {
	let mut bundle = crate::credentials::load_bundle()?;
	bundle.bindings = SourceBindings(bindings.0.clone());
	crate::credentials::store_bundle(&bundle)
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
	fn bind_then_resolve_trims_source() {
		let mut b = SourceBindings::default();
		let creds = vec![cred("c1", "github.com", "TOK1")];
		bind_source_to_credential(&mut b, " o/r ", Some("c1"), &creds).unwrap();

		assert_eq!(
			resolve_token_for_source("o/r", Some("github.com"), &b, &creds),
			Some("TOK1".into())
		);
		assert!(b.0.contains_key("o/r"));
		assert!(!b.0.contains_key(" o/r "));
	}

	#[test]
	fn binding_matches_equivalent_github_url() {
		let mut b = SourceBindings::default();
		b.0.insert("owner/repo".into(), "c1".into());
		let creds = vec![cred("c1", "personal-token", "TOK1")];

		assert_eq!(
			resolve_token_for_source(
				"https://github.com/owner/repo.git",
				Some("github.com"),
				&b,
				&creds,
			),
			Some("TOK1".into())
		);
	}

	#[test]
	fn url_binding_matches_equivalent_github_source() {
		let mut b = SourceBindings::default();
		b.0.insert("https://github.com/owner/repo.git".into(), "c1".into());
		let creds = vec![cred("c1", "personal-token", "TOK1")];

		assert_eq!(
			resolve_token_for_source(
				"owner/repo",
				Some("github.com"),
				&b,
				&creds
			),
			Some("TOK1".into())
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

	// --- Cross-host security regression tests (P0) -----------------------

	#[test]
	fn cross_host_binding_does_not_leak_token() {
		// P0 regression: a binding for `owner/repo` on `github.com` must
		// NOT match a lookup for the same `owner/repo` shape on
		// `gitlab.com` — otherwise we'd send the GitHub token to GitLab.
		// We deliberately omit a `gitlab.com` host credential so the
		// step-2 host fallback cannot rescue the resolution.
		let mut b = SourceBindings::default();
		b.0.insert("owner/repo".into(), "c_github".into());
		let creds = vec![cred("c_github", "github.com", "GHTOK")];

		assert_eq!(
			resolve_token_for_source(
				"https://gitlab.com/owner/repo.git",
				Some("gitlab.com"),
				&b,
				&creds,
			),
			None
		);
	}

	#[test]
	fn same_host_url_forms_still_alias_match() {
		// P0 regression partner: within the same host, different URL forms
		// of the same repo must still match (the alias-matching behavior
		// we are NOT breaking).
		let mut b = SourceBindings::default();
		b.0.insert(
			"https://github.com/owner/repo.git".into(),
			"c_github".into(),
		);
		let creds = vec![cred("c_github", "github.com", "GHTOK")];

		assert_eq!(
			resolve_token_for_source(
				"https://github.com/owner/repo",
				Some("github.com"),
				&b,
				&creds,
			),
			Some("GHTOK".into())
		);
	}

	#[test]
	fn unbind_clears_all_equivalent_entries() {
		// P0 regression: when a binding is cleared, every stored entry
		// that resolves to the same repo (different URL forms) must be
		// removed. This pins the "host-prefixed alias set" semantic.
		let mut b = SourceBindings::default();
		b.0.insert("owner/repo".into(), "c1".into());
		b.0.insert("https://github.com/owner/repo.git".into(), "c1".into());
		b.0.insert("git@github.com:owner/repo.git".into(), "c1".into());
		let creds = vec![cred("c1", "github.com", "TOK1")];

		bind_source_to_credential(&mut b, "owner/repo", None, &creds).unwrap();

		assert!(!b.0.contains_key("owner/repo"));
		assert!(!b.0.contains_key("https://github.com/owner/repo.git"));
		assert!(!b.0.contains_key("git@github.com:owner/repo.git"));
	}

	#[test]
	fn unresolvable_local_path_still_matches() {
		// P0 regression: local paths (which don't resolve to a host) must
		// still match themselves so the legacy behaviour for local skills
		// keeps working.
		let creds = vec![cred("c_local", "anything", "LOK")];
		let mut b = SourceBindings::default();
		bind_source_to_credential(
			&mut b,
			"/Users/audi/projects/foo",
			Some("c_local"),
			&creds,
		)
		.unwrap();

		assert_eq!(
			resolve_token_for_source(
				"/Users/audi/projects/foo",
				None,
				&b,
				&creds,
			),
			Some("LOK".into())
		);
	}

	#[test]
	fn unresolvable_does_not_match_host_prefixed_binding() {
		// P0 regression: a local path must NOT accidentally match a
		// host-prefixed binding for an unrelated repo.
		let mut b = SourceBindings::default();
		b.0.insert("https://github.com/owner/repo".into(), "c_github".into());
		let creds = vec![cred("c_github", "github.com", "GHTOK")];

		assert_eq!(
			resolve_token_for_source("/some/local/path", None, &b, &creds,),
			None
		);
	}

	#[test]
	fn host_is_case_insensitive() {
		// P0 regression: GitHub hostnames are case-insensitive (URL spec).
		// A binding for `https://github.com/…` must match a lookup for
		// `https://GitHub.com/…`. We normalise the host to lowercase before
		// prefixing, so this is a regression guard for that normalisation.
		let creds = vec![cred("c1", "GitHub.com", "TOK1")];
		let mut b = SourceBindings::default();
		bind_source_to_credential(
			&mut b,
			"https://github.com/owner/repo",
			Some("c1"),
			&creds,
		)
		.unwrap();

		assert_eq!(
			resolve_token_for_source(
				"https://GitHub.com/owner/repo",
				Some("GitHub.com"),
				&b,
				&creds,
			),
			Some("TOK1".into())
		);
	}
}
