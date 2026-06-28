//! Source-credential storage + the keyring/env [`TokenResolver`] impls.
//!
//! Moved down from `crates/api` so BOTH the desktop API and the CLI share one
//! credential store and one set of resolvers (env, keyring, env-then-keyring).
//! Two keyring entries back this store, both under SERVICE `aghub`:
//! `github_credentials` (the [`StoredCredential`] list) and
//! `skill_source_bindings` (the source→credential_id [`SourceBindings`] map).
//!
//! Resolution order for a skill source: (1) keyring source→credential_id
//! binding, (2) keychain by host, (3) `None` → caller yields
//! `Uncheckable{auth}`. Tokens never touch the lock.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::TokenResolver;

const SERVICE: &str = "aghub";
const USER: &str = "github_credentials";
const BINDINGS_USER: &str = "skill_source_bindings"; // SERVICE = "aghub"

// Guards in-process read-modify-write cycles for the keyring JSON entries.
// Cross-process keyring races remain a documented known limitation.
static KEYRING_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_keyring() -> std::sync::MutexGuard<'static, ()> {
	KEYRING_MUTEX
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A real credential-store error. Replaces the old `String`/`None`-swallowing:
/// keyring `NoEntry` still maps to "empty", but any other keyring or serde
/// failure surfaces as a [`CredentialError`] for callers to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
	Keyring(String),
	Serde(String),
}

impl std::fmt::Display for CredentialError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			CredentialError::Keyring(msg) => {
				write!(f, "keychain error: {msg}")
			}
			CredentialError::Serde(msg) => {
				write!(f, "credential serialization error: {msg}")
			}
		}
	}
}

impl std::error::Error for CredentialError {}

/// Binding validation errors (HTTP-agnostic; callers map to 400/404/500).
///
/// `Store` carries a real keychain/serde failure DISTINCTLY from the
/// validation variants: a keyring read/write failure during `bind` must NOT be
/// reported as "credential not found" (which would 404), it must surface as a
/// 500/`KEYCHAIN_ERROR`. See finding #1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
	EmptySource,
	CredentialNotFound(String),
	Store(CredentialError),
}

impl std::fmt::Display for BindError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			BindError::EmptySource => write!(f, "source must not be empty"),
			BindError::CredentialNotFound(id) => {
				write!(f, "credential not found: {id}")
			}
			BindError::Store(err) => write!(f, "{err}"),
		}
	}
}

impl std::error::Error for BindError {}

impl From<CredentialError> for BindError {
	fn from(err: CredentialError) -> Self {
		BindError::Store(err)
	}
}

/// Outcome of [`SourceCredentialStore::create_unique`]: either the dup-name
/// check failed (`Duplicate`) or the keychain itself errored (`Store`). The
/// dup-name check + insert run under ONE keyring lock, so a concurrent create
/// cannot slip a duplicate past the check (finding #2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
	Duplicate(String),
	Store(CredentialError),
}

impl std::fmt::Display for CreateError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			CreateError::Duplicate(name) => {
				write!(f, "a credential named '{name}' already exists")
			}
			CreateError::Store(err) => write!(f, "{err}"),
		}
	}
}

impl std::error::Error for CreateError {}

impl From<CredentialError> for CreateError {
	fn from(err: CredentialError) -> Self {
		CreateError::Store(err)
	}
}

/// One stored source credential (a named token), serialized as a JSON array in
/// the `aghub`/`github_credentials` keyring entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredential {
	pub id: String,
	pub name: String,
	pub token: String,
}

/// In-memory representation of the source→credential_id bindings; backed by a
/// single keyring JSON entry (`aghub`/`skill_source_bindings`). The map is
/// `source → credential_id`.
#[derive(Default, Serialize, Deserialize)]
pub struct SourceBindings(pub BTreeMap<String, String>);

// --- storage backend (injectable seam) -----------------------------------

/// The persistence seam behind the store: load/save the credential list and the
/// source-binding map. Production uses [`KeyringBackend`]; tests inject a fake
/// (in-memory, or one that fails on demand) so the store's error-mapping and
/// locking contracts are exercised without a real keychain. Findings #1/#2/#6
/// asked for exactly this injectable backend.
pub(crate) trait CredentialBackend {
	fn load_credentials(
		&self,
	) -> Result<Vec<StoredCredential>, CredentialError>;
	fn store_credentials(
		&self,
		creds: &[StoredCredential],
	) -> Result<(), CredentialError>;
	fn load_bindings(&self) -> Result<SourceBindings, CredentialError>;
	fn save_bindings(
		&self,
		bindings: &SourceBindings,
	) -> Result<(), CredentialError>;
}

/// Production backend: the two `aghub` keyring entries.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct KeyringBackend;

impl CredentialBackend for KeyringBackend {
	fn load_credentials(
		&self,
	) -> Result<Vec<StoredCredential>, CredentialError> {
		load_credentials()
	}
	fn store_credentials(
		&self,
		creds: &[StoredCredential],
	) -> Result<(), CredentialError> {
		store_credentials(creds)
	}
	fn load_bindings(&self) -> Result<SourceBindings, CredentialError> {
		load_source_bindings()
	}
	fn save_bindings(
		&self,
		bindings: &SourceBindings,
	) -> Result<(), CredentialError> {
		save_source_bindings(bindings)
	}
}

// --- keyring entry helpers -----------------------------------------------

fn credentials_entry() -> Result<keyring::Entry, CredentialError> {
	keyring::Entry::new(SERVICE, USER)
		.map_err(|e| CredentialError::Keyring(e.to_string()))
}

fn bindings_entry() -> Result<keyring::Entry, CredentialError> {
	keyring::Entry::new(SERVICE, BINDINGS_USER)
		.map_err(|e| CredentialError::Keyring(e.to_string()))
}

fn load_credentials() -> Result<Vec<StoredCredential>, CredentialError> {
	let entry = credentials_entry()?;
	match entry.get_password() {
		Ok(json) => serde_json::from_str(&json)
			.map_err(|e| CredentialError::Serde(e.to_string())),
		Err(keyring::Error::NoEntry) => Ok(vec![]),
		Err(e) => Err(CredentialError::Keyring(e.to_string())),
	}
}

fn store_credentials(
	creds: &[StoredCredential],
) -> Result<(), CredentialError> {
	let entry = credentials_entry()?;
	if creds.is_empty() {
		match entry.delete_credential() {
			Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
			Err(e) => Err(CredentialError::Keyring(e.to_string())),
		}
	} else {
		let json = serde_json::to_string(creds)
			.map_err(|e| CredentialError::Serde(e.to_string()))?;
		entry
			.set_password(&json)
			.map_err(|e| CredentialError::Keyring(e.to_string()))
	}
}

/// Load the source→credential_id bindings from the `skill_source_bindings`
/// keyring entry. Mirrors [`load_credentials`].
fn load_source_bindings() -> Result<SourceBindings, CredentialError> {
	let entry = bindings_entry()?;
	match entry.get_password() {
		Ok(json) => serde_json::from_str(&json)
			.map_err(|e| CredentialError::Serde(e.to_string())),
		Err(keyring::Error::NoEntry) => Ok(SourceBindings::default()),
		Err(e) => Err(CredentialError::Keyring(e.to_string())),
	}
}

/// Persist the source→credential_id bindings to the keyring entry. An empty
/// map deletes the entry. Mirrors [`store_credentials`].
fn save_source_bindings(
	bindings: &SourceBindings,
) -> Result<(), CredentialError> {
	let entry = bindings_entry()?;
	if bindings.0.is_empty() {
		match entry.delete_credential() {
			Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
			Err(e) => Err(CredentialError::Keyring(e.to_string())),
		}
	} else {
		let json = serde_json::to_string(bindings)
			.map_err(|e| CredentialError::Serde(e.to_string()))?;
		entry
			.set_password(&json)
			.map_err(|e| CredentialError::Keyring(e.to_string()))
	}
}

// --- pure binding/resolution logic (moved VERBATIM) ----------------------

fn credential_name_exists(creds: &[StoredCredential], name: &str) -> bool {
	creds.iter().any(|credential| credential.name == name)
}

/// Resolve a token for a skill source, in priority order:
/// 1. An explicit source→credential_id binding (looked up in `bindings`).
/// 2. A credential whose `name` matches the requested `host`.
/// 3. `None` — the caller surfaces `Uncheckable { reason: auth }`.
///
/// Tokens returned here are used only for in-memory fetches; they are never
/// written to any committed lock file.
fn resolve_token_for_source(
	source: &str,
	host: Option<&str>,
	bindings: &SourceBindings,
	creds: &[StoredCredential],
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

fn bind_source_to_credential(
	bindings: &mut SourceBindings,
	source: &str,
	credential_id: Option<&str>,
	creds: &[StoredCredential],
) -> Result<(), BindError> {
	if source.trim().is_empty() {
		return Err(BindError::EmptySource);
	}
	let key = canonical_binding_key(source);

	if let Some(credential_id) = credential_id {
		if !creds.iter().any(|c| c.id == credential_id) {
			return Err(BindError::CredentialNotFound(
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
fn lookup_keys(source: &str) -> BTreeSet<String> {
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
fn binding_keys_match_lookup(
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

fn prune_bindings_for_credential(
	bindings: &mut SourceBindings,
	credential_id: &str,
) -> bool {
	let original_len = bindings.0.len();
	bindings.0.retain(|_, id| id != credential_id);
	bindings.0.len() != original_len
}

// --- the store (one façade over the storage backend) ---------------------

/// One store over the two keyring entries
/// (`aghub`/`github_credentials` + `aghub`/`skill_source_bindings`). Generic
/// over a [`CredentialBackend`] so tests can inject a fake; production callers
/// use the [`SourceCredentialStore`] unit alias backed by the real keyring.
///
/// Every read-modify-write method takes the in-process [`lock_keyring`] guard
/// so the load→validate→save cycle is atomic against other in-process callers
/// (cross-process keyring races remain a documented limitation).
pub(crate) struct Store<B: CredentialBackend> {
	backend: B,
}

impl<B: CredentialBackend> Store<B> {
	pub(crate) fn with_backend(backend: B) -> Self {
		Self { backend }
	}

	/// List all stored credentials.
	pub fn list(&self) -> Result<Vec<StoredCredential>, CredentialError> {
		self.backend.load_credentials()
	}

	/// Create a credential with a random v4 uuid id, enforcing the unique-name
	/// policy under ONE keyring lock so a concurrent create cannot slip a
	/// duplicate past the check. The HTTP/CLI surface maps [`CreateError`] to
	/// its own 409/500 codes (the store stays HTTP-agnostic).
	pub fn create_unique(
		&self,
		name: &str,
		token: &str,
	) -> Result<StoredCredential, CreateError> {
		let _guard = lock_keyring();
		let mut creds = self.backend.load_credentials()?;
		if credential_name_exists(&creds, name) {
			return Err(CreateError::Duplicate(name.to_string()));
		}
		let new = StoredCredential {
			id: uuid::Uuid::new_v4().to_string(),
			name: name.to_string(),
			token: token.to_string(),
		};
		creds.push(new.clone());
		self.backend.store_credentials(&creds)?;
		Ok(new)
	}

	/// Delete a credential by id; also prunes any bindings that pointed at it.
	/// Returns `true` if a credential was removed.
	///
	/// The credential delete and the binding prune run under ONE lock. A
	/// binding-prune persistence failure is logged and treated as **non-fatal**
	/// (the credential delete still succeeds), matching the route contract that
	/// a prune failure must not 500 a successful delete (finding #6).
	pub fn delete(&self, id: &str) -> Result<bool, CredentialError> {
		let _guard = lock_keyring();
		let mut creds = self.backend.load_credentials()?;
		let original_len = creds.len();
		creds.retain(|c| c.id != id);
		let removed = original_len != creds.len();
		self.backend.store_credentials(&creds)?;

		// Best-effort binding prune: the credential is already gone, so a load
		// or save failure here must NOT turn a successful delete into an error.
		match self.backend.load_bindings() {
			Ok(mut bindings) => {
				if prune_bindings_for_credential(&mut bindings, id) {
					if let Err(error) = self.backend.save_bindings(&bindings) {
						log::warn!(
							"credential {id} deleted; pruning its bindings \
							 failed (non-fatal): {error}"
						);
					}
				}
			}
			Err(error) => log::warn!(
				"credential {id} deleted; loading bindings to prune failed \
				 (non-fatal): {error}"
			),
		}
		Ok(removed)
	}

	/// List the source→credential_id bindings.
	pub fn list_bindings(&self) -> Result<SourceBindings, CredentialError> {
		self.backend.load_bindings()
	}

	/// Bind (or, with `credential_id == None`, clear) a source. Validates the
	/// credential exists before binding; persists the result. A real keychain
	/// failure surfaces as [`BindError::Store`] (NOT `CredentialNotFound`), so
	/// the API returns 500/`KEYCHAIN_ERROR` rather than a misleading 404
	/// (finding #1). On any failure the persisted state is left unmutated.
	pub fn bind(
		&self,
		source: &str,
		credential_id: Option<&str>,
	) -> Result<(), BindError> {
		let _guard = lock_keyring();
		let mut bindings = self.backend.load_bindings()?;
		// Clearing a binding needs no credential validation, so don't load the
		// credential list (a creds keychain/serde failure must not block an
		// unbind). Only load when a credential_id must be validated (finding #2).
		let creds = if credential_id.is_some() {
			self.backend.load_credentials()?
		} else {
			Vec::new()
		};
		bind_source_to_credential(
			&mut bindings,
			source,
			credential_id,
			&creds,
		)?;
		self.backend.save_bindings(&bindings)?;
		Ok(())
	}

	/// Resolve a token for `source`, binding-then-host fallback. A keyring
	/// failure surfaces as [`CredentialError`] (no `.unwrap_or_default()`
	/// swallowing); only the [`TokenResolver`] boundary degrades to `None`.
	pub fn resolve_token(
		&self,
		source: &str,
		host: Option<&str>,
	) -> Result<Option<String>, CredentialError> {
		let bindings = self.backend.load_bindings()?;
		let creds = self.backend.load_credentials()?;
		Ok(resolve_token_for_source(source, host, &bindings, &creds))
	}
}

/// The production store: a unit value backed by the real keyring. Kept as a
/// zero-arg constructible unit struct so existing call sites
/// (`SourceCredentialStore.list()`, …) are unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceCredentialStore;

impl SourceCredentialStore {
	fn store(&self) -> Store<KeyringBackend> {
		Store::with_backend(KeyringBackend)
	}

	/// List all stored credentials.
	pub fn list(&self) -> Result<Vec<StoredCredential>, CredentialError> {
		self.store().list()
	}

	/// Create a credential, enforcing the unique-name policy atomically.
	pub fn create_unique(
		&self,
		name: &str,
		token: &str,
	) -> Result<StoredCredential, CreateError> {
		self.store().create_unique(name, token)
	}

	/// Delete a credential by id (best-effort binding prune; see [`Store::delete`]).
	pub fn delete(&self, id: &str) -> Result<bool, CredentialError> {
		self.store().delete(id)
	}

	/// List the source→credential_id bindings.
	pub fn list_bindings(&self) -> Result<SourceBindings, CredentialError> {
		self.store().list_bindings()
	}

	/// Bind (or clear) a source → credential mapping.
	pub fn bind(
		&self,
		source: &str,
		credential_id: Option<&str>,
	) -> Result<(), BindError> {
		self.store().bind(source, credential_id)
	}

	/// Resolve a token for `source` (binding-then-host fallback).
	pub fn resolve_token(
		&self,
		source: &str,
		host: Option<&str>,
	) -> Result<Option<String>, CredentialError> {
		self.store().resolve_token(source, host)
	}
}

// --- TokenResolver impls -------------------------------------------------

/// Map a store resolution to the [`TokenResolver`] return: a keyring error
/// degrades to `None` (the trait can't return `Result`), but is logged so it
/// stays diagnosable.
fn degrade_to_none(
	resolution: Result<Option<String>, CredentialError>,
) -> Option<String> {
	match resolution {
		Ok(token) => token,
		Err(error) => {
			log::warn!("credential resolution failed, ignoring: {error}");
			None
		}
	}
}

/// [`TokenResolver`] backed by the keyring [`SourceCredentialStore`]. A keyring
/// error degrades to `None` at the trait boundary (the trait can't return
/// `Result`); the error is logged.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyringTokenResolver {
	store: SourceCredentialStore,
}

impl TokenResolver for KeyringTokenResolver {
	fn resolve(&self, source: &str, host: Option<&str>) -> Option<String> {
		degrade_to_none(self.store.resolve_token(source, host))
	}
}

/// [`TokenResolver`] reading `GIT_PASSWORD` (or `GITHUB_TOKEN`) from the
/// environment, ignoring `source`/`host`. Moved from the CLI.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvTokenResolver;

impl EnvTokenResolver {
	fn env_token() -> Option<String> {
		std::env::var("GIT_PASSWORD")
			.or_else(|_| std::env::var("GITHUB_TOKEN"))
			.ok()
	}
}

impl TokenResolver for EnvTokenResolver {
	fn resolve(&self, _source: &str, _host: Option<&str>) -> Option<String> {
		Self::env_token()
	}
}

/// [`TokenResolver`] that tries the environment first, then the keyring store.
/// This is what the CLI `source` commands use, so a token in the environment
/// always wins but a keyring binding still applies otherwise.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvThenKeyringResolver {
	keyring: KeyringTokenResolver,
}

impl TokenResolver for EnvThenKeyringResolver {
	fn resolve(&self, source: &str, host: Option<&str>) -> Option<String> {
		env_then(&self.keyring, source, host)
	}
}

/// Env-first precedence, then a fallback [`TokenResolver`]. Factored out so the
/// fallback half is testable with an injected resolver (the production fallback
/// is the keyring, which a CI box can't exercise) — env set must short-circuit
/// (fallback never consulted); env unset must consult the fallback (finding #3).
fn env_then(
	fallback: &dyn TokenResolver,
	source: &str,
	host: Option<&str>,
) -> Option<String> {
	EnvTokenResolver::env_token().or_else(|| fallback.resolve(source, host))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{Mutex, OnceLock};

	/// In-memory [`CredentialBackend`] for store tests. Each load/save op can be
	/// independently forced to fail with a fixed [`CredentialError`] so the
	/// store's error-mapping / non-mutation / non-fatal contracts are testable
	/// without a real keychain (findings #1, #2, #6).
	#[derive(Default)]
	struct FakeBackend {
		creds: Mutex<Vec<StoredCredential>>,
		bindings: Mutex<SourceBindings>,
		fail_load_creds: Option<CredentialError>,
		fail_store_creds: Option<CredentialError>,
		fail_load_bindings: Option<CredentialError>,
		fail_save_bindings: Option<CredentialError>,
	}

	impl FakeBackend {
		fn with_creds(creds: Vec<StoredCredential>) -> Self {
			Self {
				creds: Mutex::new(creds),
				..Self::default()
			}
		}
	}

	impl CredentialBackend for FakeBackend {
		fn load_credentials(
			&self,
		) -> Result<Vec<StoredCredential>, CredentialError> {
			if let Some(e) = &self.fail_load_creds {
				return Err(e.clone());
			}
			Ok(self.creds.lock().unwrap().clone())
		}
		fn store_credentials(
			&self,
			creds: &[StoredCredential],
		) -> Result<(), CredentialError> {
			if let Some(e) = &self.fail_store_creds {
				return Err(e.clone());
			}
			*self.creds.lock().unwrap() = creds.to_vec();
			Ok(())
		}
		fn load_bindings(&self) -> Result<SourceBindings, CredentialError> {
			if let Some(e) = &self.fail_load_bindings {
				return Err(e.clone());
			}
			Ok(SourceBindings(self.bindings.lock().unwrap().0.clone()))
		}
		fn save_bindings(
			&self,
			bindings: &SourceBindings,
		) -> Result<(), CredentialError> {
			if let Some(e) = &self.fail_save_bindings {
				return Err(e.clone());
			}
			*self.bindings.lock().unwrap() = SourceBindings(bindings.0.clone());
			Ok(())
		}
	}

	// --- store error-mapping / atomicity (injectable backend) ------------

	#[test]
	fn bind_keychain_failure_surfaces_as_store_not_not_found() {
		// finding #1: a keyring failure loading bindings during `bind` must
		// surface as BindError::Store (→ 500/KEYCHAIN_ERROR), NOT as
		// CredentialNotFound (which the API maps to a misleading 404).
		let backend = FakeBackend {
			fail_load_bindings: Some(CredentialError::Keyring("boom".into())),
			..FakeBackend::default()
		};
		let store = Store::with_backend(backend);
		let err = store.bind("o/r", None).unwrap_err();
		assert_eq!(
			err,
			BindError::Store(CredentialError::Keyring("boom".into()))
		);
	}

	#[test]
	fn bind_credential_load_failure_surfaces_as_store() {
		// finding #1: failing to load the credential list during `bind` is a
		// store error, not "credential not found".
		let backend = FakeBackend {
			fail_load_creds: Some(CredentialError::Serde("bad".into())),
			..FakeBackend::default()
		};
		let store = Store::with_backend(backend);
		let err = store.bind("o/r", Some("c1")).unwrap_err();
		assert_eq!(err, BindError::Store(CredentialError::Serde("bad".into())));
	}

	#[test]
	fn bind_missing_credential_still_maps_to_not_found() {
		// finding #1 partner: a genuinely missing credential is still 404, not
		// conflated with a store error.
		let store = Store::with_backend(FakeBackend::with_creds(vec![]));
		let err = store.bind("o/r", Some("gone")).unwrap_err();
		assert_eq!(err, BindError::CredentialNotFound("gone".into()));
	}

	#[test]
	fn bind_save_failure_surfaces_as_store_and_does_not_mutate() {
		// finding #1: a save failure surfaces as Store, and the persisted
		// bindings are not mutated (the in-memory map saw no successful write).
		let backend = FakeBackend {
			creds: Mutex::new(vec![cred("c1", "github.com", "T")]),
			fail_save_bindings: Some(CredentialError::Keyring("disk".into())),
			..FakeBackend::default()
		};
		let store = Store::with_backend(backend);
		let err = store.bind("o/r", Some("c1")).unwrap_err();
		assert_eq!(
			err,
			BindError::Store(CredentialError::Keyring("disk".into()))
		);
		assert!(
			store.list_bindings().unwrap().0.is_empty(),
			"failed save must leave persisted bindings unmutated"
		);
	}

	#[test]
	fn create_unique_rejects_duplicate_name_under_one_lock() {
		// finding #2: the dup-name check + insert are one atomic op on the
		// store, so the policy can't be bypassed by re-listing.
		let store = Store::with_backend(FakeBackend::with_creds(vec![cred(
			"c1",
			"github.com",
			"T",
		)]));
		let err = store.create_unique("github.com", "T2").unwrap_err();
		assert_eq!(err, CreateError::Duplicate("github.com".into()));
		// The store is unchanged (still exactly one credential).
		assert_eq!(store.list().unwrap().len(), 1);
	}

	#[test]
	fn create_unique_allows_new_name() {
		let store = Store::with_backend(FakeBackend::with_creds(vec![cred(
			"c1",
			"github.com",
			"T",
		)]));
		let new = store.create_unique("gitlab.com", "T2").unwrap();
		assert_eq!(new.name, "gitlab.com");
		assert_eq!(store.list().unwrap().len(), 2);
	}

	#[test]
	fn create_unique_maps_store_failure() {
		let backend = FakeBackend {
			fail_store_creds: Some(CredentialError::Keyring("nope".into())),
			..FakeBackend::default()
		};
		let store = Store::with_backend(backend);
		let err = store.create_unique("github.com", "T").unwrap_err();
		assert_eq!(
			err,
			CreateError::Store(CredentialError::Keyring("nope".into()))
		);
	}

	#[test]
	fn delete_prune_save_failure_is_non_fatal() {
		// finding #6: the credential is deleted, then a binding-prune save
		// failure must NOT turn the successful delete into an error (the route
		// documents prune failure as non-fatal).
		let mut bindings = SourceBindings::default();
		bindings.0.insert("o/r".into(), "c1".into());
		let backend = FakeBackend {
			creds: Mutex::new(vec![cred("c1", "github.com", "T")]),
			bindings: Mutex::new(bindings),
			fail_save_bindings: Some(CredentialError::Keyring("disk".into())),
			..FakeBackend::default()
		};
		let store = Store::with_backend(backend);
		// Delete succeeds (returns removed=true) despite the prune save failing.
		assert!(store.delete("c1").unwrap());
		// The credential really is gone.
		assert!(store.list().unwrap().is_empty());
	}

	#[test]
	fn delete_prune_load_failure_is_non_fatal() {
		// finding #1: if LOADING the bindings to prune fails after the credential
		// is already deleted, the delete must still report success (non-fatal),
		// not surface the bindings-load error.
		let backend = FakeBackend {
			creds: Mutex::new(vec![cred("c1", "github.com", "T")]),
			fail_load_bindings: Some(CredentialError::Keyring("boom".into())),
			..FakeBackend::default()
		};
		let store = Store::with_backend(backend);
		assert!(store.delete("c1").unwrap());
		assert!(store.list().unwrap().is_empty());
	}

	#[test]
	fn clear_binding_succeeds_when_credential_load_fails() {
		// finding #2: clearing a binding (credential_id == None) needs no
		// credential validation, so a credentials keychain/serde failure must NOT
		// block the unbind.
		let mut bindings = SourceBindings::default();
		bindings.0.insert("owner/repo".into(), "c1".into());
		let backend = FakeBackend {
			bindings: Mutex::new(bindings),
			fail_load_creds: Some(CredentialError::Keyring("boom".into())),
			..FakeBackend::default()
		};
		let store = Store::with_backend(backend);
		store.bind("owner/repo", None).unwrap();
		assert!(
			store.list_bindings().unwrap().0.is_empty(),
			"the binding should have been cleared"
		);
	}

	#[test]
	fn delete_propagates_credential_store_failure() {
		// The credential store-write failure (not the prune) is still fatal:
		// if we can't persist the credential removal, the delete must error.
		let backend = FakeBackend {
			creds: Mutex::new(vec![cred("c1", "github.com", "T")]),
			fail_store_creds: Some(CredentialError::Keyring("ro".into())),
			..FakeBackend::default()
		};
		let store = Store::with_backend(backend);
		let err = store.delete("c1").unwrap_err();
		assert_eq!(err, CredentialError::Keyring("ro".into()));
	}

	fn cred(id: &str, name: &str, token: &str) -> StoredCredential {
		StoredCredential {
			id: id.into(),
			name: name.into(),
			token: token.into(),
		}
	}

	// Serializes tests that mutate process env vars (GIT_PASSWORD, …).
	fn env_lock() -> &'static Mutex<()> {
		static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
		LOCK.get_or_init(|| Mutex::new(()))
	}

	// --- resolution priority (moved from api/credentials/resolve.rs) -----

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

		assert_eq!(err, BindError::CredentialNotFound("gone".into()));
		assert_eq!(b.0, before);
	}

	#[test]
	fn empty_source_is_rejected() {
		let mut b = SourceBindings::default();
		let err = bind_source_to_credential(&mut b, "  ", Some("c1"), &[])
			.unwrap_err();
		assert_eq!(err, BindError::EmptySource);
		assert!(b.0.is_empty());
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

	#[test]
	fn name_exists_helper_detects_duplicate() {
		let creds = vec![cred("c1", "github.com", "tok")];
		assert!(credential_name_exists(&creds, "github.com"));
		assert!(!credential_name_exists(&creds, "gitlab.com"));
	}

	// --- new behavior: resolver degradation + env precedence -------------

	#[test]
	fn keyring_resolver_maps_credential_error_to_none() {
		// TokenResolver::resolve can't return Result, so a CredentialError
		// from the store must degrade to None at the trait boundary.
		assert_eq!(
			degrade_to_none(Err(CredentialError::Keyring("boom".into()))),
			None
		);
		assert_eq!(
			degrade_to_none(Err(CredentialError::Serde("bad json".into()))),
			None
		);
		// An Ok value passes through unchanged.
		assert_eq!(degrade_to_none(Ok(Some("tok".into()))), Some("tok".into()));
		assert_eq!(degrade_to_none(Ok(None)), None);
	}

	#[test]
	fn env_token_resolver_returns_git_password() {
		let _guard = env_lock().lock().unwrap();
		std::env::remove_var("GITHUB_TOKEN");
		std::env::set_var("GIT_PASSWORD", "ENVTOK");

		assert_eq!(
			EnvTokenResolver.resolve("o/r", Some("github.com")),
			Some("ENVTOK".into())
		);

		std::env::remove_var("GIT_PASSWORD");
	}

	#[test]
	fn env_token_resolver_falls_back_to_github_token() {
		let _guard = env_lock().lock().unwrap();
		std::env::remove_var("GIT_PASSWORD");
		std::env::set_var("GITHUB_TOKEN", "GHTOK");

		assert_eq!(EnvTokenResolver.resolve("o/r", None), Some("GHTOK".into()));

		std::env::remove_var("GITHUB_TOKEN");
	}

	#[test]
	fn env_token_resolver_none_when_unset() {
		let _guard = env_lock().lock().unwrap();
		std::env::remove_var("GIT_PASSWORD");
		std::env::remove_var("GITHUB_TOKEN");

		assert_eq!(EnvTokenResolver.resolve("o/r", Some("github.com")), None);
	}

	#[test]
	fn env_then_keyring_prefers_env_value() {
		// env first: when GIT_PASSWORD is set, EnvThenKeyringResolver returns
		// it WITHOUT touching the keyring (so this is safe on a CI box with no
		// keychain — the env short-circuit wins).
		let _guard = env_lock().lock().unwrap();
		std::env::remove_var("GITHUB_TOKEN");
		std::env::set_var("GIT_PASSWORD", "ENVWINS");

		assert_eq!(
			EnvThenKeyringResolver::default()
				.resolve("o/r", Some("github.com")),
			Some("ENVWINS".into())
		);

		std::env::remove_var("GIT_PASSWORD");
	}

	/// A fallback [`TokenResolver`] that records whether it was consulted and
	/// returns a fixed token. Lets us prove `env_then`'s two halves (finding #3).
	struct RecordingResolver {
		token: Option<String>,
		called: std::sync::atomic::AtomicBool,
	}

	impl TokenResolver for RecordingResolver {
		fn resolve(
			&self,
			_source: &str,
			_host: Option<&str>,
		) -> Option<String> {
			self.called.store(true, std::sync::atomic::Ordering::SeqCst);
			self.token.clone()
		}
	}

	#[test]
	fn env_then_consults_fallback_when_env_unset() {
		// finding #3: with env unset, the keyring (here: fake) fallback IS
		// consulted and its token is returned.
		let _guard = env_lock().lock().unwrap();
		std::env::remove_var("GIT_PASSWORD");
		std::env::remove_var("GITHUB_TOKEN");

		let fallback = RecordingResolver {
			token: Some("KEYTOK".into()),
			called: std::sync::atomic::AtomicBool::new(false),
		};
		assert_eq!(
			env_then(&fallback, "o/r", Some("github.com")),
			Some("KEYTOK".into())
		);
		assert!(
			fallback.called.load(std::sync::atomic::Ordering::SeqCst),
			"fallback must be consulted when env is unset"
		);
	}

	#[test]
	fn env_then_skips_fallback_when_env_set() {
		// finding #3: with env set, the env value wins AND the fallback is never
		// consulted (so an env token short-circuits the keyring entirely).
		let _guard = env_lock().lock().unwrap();
		std::env::remove_var("GITHUB_TOKEN");
		std::env::set_var("GIT_PASSWORD", "ENVWINS");

		let fallback = RecordingResolver {
			token: Some("KEYTOK".into()),
			called: std::sync::atomic::AtomicBool::new(false),
		};
		let got = env_then(&fallback, "o/r", Some("github.com"));
		std::env::remove_var("GIT_PASSWORD");

		assert_eq!(got, Some("ENVWINS".into()));
		assert!(
			!fallback.called.load(std::sync::atomic::Ordering::SeqCst),
			"fallback must NOT be consulted when env is set"
		);
	}

	#[test]
	fn env_then_keyring_none_when_env_unset_and_no_keyring_entry() {
		// With env unset and no keyring binding (CI has no keychain entry),
		// the resolver must yield None — it must never fabricate a token.
		let _guard = env_lock().lock().unwrap();
		std::env::remove_var("GIT_PASSWORD");
		std::env::remove_var("GITHUB_TOKEN");

		assert_eq!(
			EnvThenKeyringResolver::default()
				.resolve("o/r", Some("nonexistent.example")),
			None
		);
	}
}
