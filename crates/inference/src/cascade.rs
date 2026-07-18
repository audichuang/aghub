//! Shared provider-delete use case.
//!
//! Deleting an inference provider must first tear down every agent reference to
//! it (Claude/Codex binding rows + config, OpenCode config/auth entries) so no
//! external agent config is left pointing at a removed provider. This logic is
//! the single seam called by BOTH the HTTP API delete route and the CLI
//! `inference delete` command — they must not diverge.

use crate::claude::ClaudeProviderAdapter;
use crate::codex::CodexProviderAdapter;
use crate::credentials::CredentialStore;
use crate::error::Result;
use crate::model::InferenceProvider;
use crate::opencode::OpenCodeProviderAdapter;
use crate::store::{InferenceProviderRepository, InferenceProviderStore};
use crate::{AgentProviderAdapter, AgentProviderBinding};

/// Two API base URLs are "the same" ignoring trailing slashes / surrounding
/// whitespace — the matching key for OpenCode's config-backed providers.
fn same_api_base_url(left: &str, right: &str) -> bool {
	left.trim().trim_end_matches('/') == right.trim().trim_end_matches('/')
}

/// Find the inventory provider an agent binding maps to, by (base URL, key).
fn matches_inventory(
	provider: &InferenceProvider,
	api_key: &str,
	binding: &AgentProviderBinding,
	agent_api_key: Option<&str>,
) -> bool {
	let Some(api_base_url) = binding.api_base_url.as_deref() else {
		return false;
	};
	let Some(agent_api_key) = agent_api_key else {
		return false;
	};
	same_api_base_url(&provider.api_base_url, api_base_url)
		&& api_key == agent_api_key
}

fn remove_claude_references<C: CredentialStore>(
	store: &InferenceProviderStore<C>,
	provider: &InferenceProvider,
	adapter: &ClaudeProviderAdapter,
) -> Result<()> {
	let binding_ids = store
		.list_agent_bindings("claude")?
		.into_iter()
		.filter(|row| row.inference_provider_id == provider.id)
		.map(|row| row.id)
		.collect::<Vec<_>>();

	for binding_id in binding_ids {
		adapter.remove_binding(store, &binding_id)?;
	}

	Ok(())
}

fn remove_codex_references<C: CredentialStore>(
	store: &InferenceProviderStore<C>,
	provider: &InferenceProvider,
	adapter: &CodexProviderAdapter,
) -> Result<()> {
	let binding_ids = store
		.list_agent_bindings("codex")?
		.into_iter()
		.filter(|row| row.inference_provider_id == provider.id)
		.map(|row| row.id)
		.collect::<Vec<_>>();

	for binding_id in binding_ids {
		adapter.remove_provider(store, &binding_id)?;
	}

	Ok(())
}

fn remove_opencode_references<C: CredentialStore>(
	store: &InferenceProviderStore<C>,
	provider: &InferenceProvider,
	adapter: &OpenCodeProviderAdapter,
) -> Result<()> {
	let Some(api_key) = store.get_api_key(&provider.id)? else {
		return Ok(());
	};
	let normalized =
		OpenCodeProviderAdapter::normalize_inventory_provider(provider);

	let mut provider_ids: Vec<String> = Vec::new();
	for binding in adapter.load_providers()?.providers {
		let agent_api_key = adapter.api_key(&binding.id)?;
		if matches_inventory(
			&normalized,
			&api_key,
			&binding,
			agent_api_key.as_deref(),
		) && !provider_ids.iter().any(|id| id == &binding.id)
		{
			provider_ids.push(binding.id);
		}
	}

	for provider_id in provider_ids {
		adapter.remove_provider(&provider_id)?;
	}

	Ok(())
}

/// Remove every agent reference to `provider` using the supplied adapters.
///
/// Split out from [`delete_provider_cascade`] so it can be tested against
/// adapters rooted at temp config paths without touching the real home dir.
///
/// **Partial-failure semantics (no rollback by design).** References are removed
/// in a fixed order — Claude, then Codex, then OpenCode — and the first error
/// short-circuits with `?`. There is intentionally NO rollback: each step that
/// already ran has detached a real agent config from a provider that is about to
/// be deleted, so undoing it would re-point a live agent at a doomed provider.
/// Each step is also idempotent — re-running the cascade after a transient
/// failure skips the references it already removed (binding rows are gone;
/// OpenCode no longer matches) and continues from where it stopped. Callers
/// surface the error so the operator can retry; a retry converges.
pub fn delete_provider_references<C: CredentialStore>(
	store: &InferenceProviderStore<C>,
	provider: &InferenceProvider,
	claude: &ClaudeProviderAdapter,
	codex: &CodexProviderAdapter,
	opencode: &OpenCodeProviderAdapter,
) -> Result<()> {
	remove_claude_references(store, provider, claude)?;
	remove_codex_references(store, provider, codex)?;
	remove_opencode_references(store, provider, opencode)?;
	Ok(())
}

/// Delete `provider` and ALL agent references to it.
///
/// Builds the global agent adapters, tears down every Claude/Codex/OpenCode
/// reference, then removes the provider (metadata + keyring key) from the
/// inventory. Both the API and CLI surfaces route their delete through here so
/// neither can leave an agent config pointing at a removed provider.
///
/// **Precondition: the credential backend must be reachable.** The very
/// first thing this does is read the provider's API key — the same read
/// `remove_opencode_references` needs later for its (base_url, api_key)
/// match, but done here FIRST, before any adapter is built or any binding
/// row is touched. If the backend itself is unreachable (Linux
/// secret-service with no D-Bus session, a locked keychain, ...), this
/// returns `KeyringUnavailable` with the whole delete a no-op — no Claude,
/// Codex, or OpenCode reference and no inventory row is touched, so a client
/// gets a stable, retryable error instead of a half-finished delete (GitHub
/// #15 P1a). Once the backend has answered — even `Ok(None)`, "no entry",
/// still counts as reachable — the rest of the cascade keeps its existing
/// partial-failure/idempotent-retry semantics (see
/// [`delete_provider_references`]).
///
/// The value read here is intentionally DISCARDED past the reachability
/// check — it is only a precondition probe. `store.delete` re-reads the
/// current key itself, under its own lock, immediately before it needs it
/// for rollback. Threading this read's value into `delete` instead (the
/// previous design) was a stale-read race (GitHub #15 P2-5): this read runs
/// UNLOCKED, so a concurrent `set_api_key` committing a newer key between
/// this read and `store.delete`'s lock acquisition could have its result
/// clobbered by a rollback restoring this now-stale value. `delete`'s own
/// lock-scoped read closes that window.
pub fn delete_provider_cascade<C: CredentialStore>(
	store: &InferenceProviderStore<C>,
	provider: &InferenceProvider,
) -> Result<InferenceProvider> {
	store.get_api_key(&provider.id)?;

	let claude = ClaudeProviderAdapter::global()?;
	let codex = CodexProviderAdapter::global()?;
	let opencode = OpenCodeProviderAdapter::global()?;
	delete_provider_references(store, provider, &claude, &codex, &opencode)?;
	store.delete(&provider.id)
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;
	use std::sync::{Arc, Mutex};

	use super::*;
	use crate::model::{CreateInferenceProvider, InferenceProviderFormat};

	#[derive(Debug, Clone, Default)]
	struct MemoryCredentialStore {
		values: Arc<Mutex<HashMap<String, String>>>,
	}

	impl CredentialStore for MemoryCredentialStore {
		fn get_api_key(&self, provider_id: &str) -> Result<Option<String>> {
			Ok(self.values.lock().unwrap().get(provider_id).cloned())
		}

		fn set_api_key(&self, provider_id: &str, api_key: &str) -> Result<()> {
			self.values
				.lock()
				.unwrap()
				.insert(provider_id.to_string(), api_key.to_string());
			Ok(())
		}

		fn delete_api_key(&self, provider_id: &str) -> Result<()> {
			self.values.lock().unwrap().remove(provider_id);
			Ok(())
		}
	}

	/// `get_api_key` always reports the backend as unreachable;
	/// `set_api_key` still works (so a provider can be seeded), and
	/// `delete_api_key` panics — it must never be reached once the
	/// precondition in [`delete_provider_cascade`] fails closed.
	#[derive(Debug, Clone, Default)]
	struct BackendDownCredentialStore {
		values: Arc<Mutex<HashMap<String, String>>>,
	}

	impl CredentialStore for BackendDownCredentialStore {
		fn get_api_key(&self, _provider_id: &str) -> Result<Option<String>> {
			Err(crate::error::InferenceProviderError::KeyringUnavailable(
				"no secret service provider or dbus session found".to_string(),
			))
		}

		fn set_api_key(&self, provider_id: &str, api_key: &str) -> Result<()> {
			self.values
				.lock()
				.unwrap()
				.insert(provider_id.to_string(), api_key.to_string());
			Ok(())
		}

		fn delete_api_key(&self, provider_id: &str) -> Result<()> {
			panic!(
				"delete_api_key({provider_id}) must not run once the \
				 backend-reachability precondition has already failed"
			)
		}
	}

	type TempStore = InferenceProviderStore<MemoryCredentialStore>;

	/// Build a temp-rooted store + a provider whose key lives in the in-memory
	/// credential store. Returns `(temp, store, provider)`; keep `temp` alive.
	fn seed_provider(
		api_base_url: &str,
	) -> (tempfile::TempDir, TempStore, InferenceProvider) {
		let temp = tempfile::tempdir().unwrap();
		let store = InferenceProviderStore::with_credentials(
			temp.path(),
			MemoryCredentialStore::default(),
		);
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "acme".to_string(),
				display_name: "acme".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: api_base_url.to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: Vec::new(),
			})
			.unwrap();
		(temp, store, provider)
	}

	/// The three agent adapters rooted under `dir`, touching no real home dir.
	fn temp_adapters(
		dir: &std::path::Path,
	) -> (
		ClaudeProviderAdapter,
		CodexProviderAdapter,
		OpenCodeProviderAdapter,
	) {
		(
			ClaudeProviderAdapter::new(dir.join("claude.json")),
			CodexProviderAdapter::new(dir.join("config.toml")),
			OpenCodeProviderAdapter::new(
				dir.join("opencode.json"),
				dir.join("oc-auth.json"),
			),
		)
	}

	#[test]
	fn references_removes_dangling_claude_binding() {
		// Regression (finding #1): a Claude binding row pointing at a provider
		// must be torn down by the shared cascade, not left dangling. Adapters
		// are rooted at temp config paths so no real home dir is touched and the
		// keyring is never used (MemoryCredentialStore).
		let (temp, store, provider) =
			seed_provider("https://api.openai.com/v1");
		store
			.create_agent_binding("claude", &provider.id, None)
			.unwrap();
		assert_eq!(store.list_agent_bindings("claude").unwrap().len(), 1);

		let (claude, codex, opencode) = temp_adapters(temp.path());
		delete_provider_references(
			&store, &provider, &claude, &codex, &opencode,
		)
		.unwrap();

		assert!(
			store.list_agent_bindings("claude").unwrap().is_empty(),
			"the shared cascade must remove the dangling claude binding"
		);
	}

	#[test]
	fn references_removes_dangling_codex_binding() {
		// Finding #2: the Codex removal branch was untested. A codex binding row
		// (binding-table backed, like Claude) pointing at the provider must be
		// torn down too.
		let (temp, store, provider) =
			seed_provider("https://api.openai.com/v1");
		store
			.create_agent_binding("codex", &provider.id, None)
			.unwrap();
		assert_eq!(store.list_agent_bindings("codex").unwrap().len(), 1);

		let (claude, codex, opencode) = temp_adapters(temp.path());
		delete_provider_references(
			&store, &provider, &claude, &codex, &opencode,
		)
		.unwrap();

		assert!(
			store.list_agent_bindings("codex").unwrap().is_empty(),
			"the shared cascade must remove the dangling codex binding"
		);
	}

	#[test]
	fn references_removes_matching_opencode_provider() {
		// Finding #2: the OpenCode match/removal branch was untested. OpenCode is
		// config-file backed: the cascade matches by (normalized base URL, api
		// key) and must remove the matching provider + its auth entry. Seed the
		// OpenCode config with the SAME provider+key so the (url, key) match hits.
		let (temp, store, provider) =
			seed_provider("https://api.openai.com/v1");
		let (claude, codex, opencode) = temp_adapters(temp.path());

		// Write OpenCode through the SAME normalized inventory provider the
		// cascade matches against, so both sides agree on the base URL.
		let normalized =
			OpenCodeProviderAdapter::normalize_inventory_provider(&provider);
		opencode
			.add_inventory_provider(&normalized, "secret")
			.expect("seed opencode provider");
		assert!(
			!opencode.load_providers().unwrap().providers.is_empty(),
			"precondition: opencode has the provider"
		);

		delete_provider_references(
			&store, &provider, &claude, &codex, &opencode,
		)
		.unwrap();

		assert!(
			opencode.load_providers().unwrap().providers.is_empty(),
			"the cascade must remove the matching opencode provider"
		);
	}

	#[test]
	fn references_skips_opencode_when_no_api_key() {
		// Finding #2: the missing-API-key branch (`get_api_key` => None) must be a
		// silent no-op, not an error — with no key there is nothing to match
		// OpenCode against. Persist the provider (so the store row exists) then
		// drop its key so `get_api_key` returns Ok(None), not an error.
		let (temp, store, provider) =
			seed_provider("https://api.openai.com/v1");
		store.delete_api_key(&provider.id).unwrap();
		assert_eq!(
			store.get_api_key(&provider.id).unwrap(),
			None,
			"precondition: provider exists but has no stored key"
		);

		let (claude, codex, opencode) = temp_adapters(temp.path());
		delete_provider_references(
			&store, &provider, &claude, &codex, &opencode,
		)
		.expect("missing api key is a no-op, not an error");
	}

	#[test]
	fn references_is_idempotent_after_partial_progress() {
		// Finding #2: no-rollback semantics. After Claude+Codex bindings are
		// removed, re-running the cascade must converge (idempotent), not error on
		// the already-removed references — this is what makes an operator retry
		// safe after a transient mid-cascade failure.
		let (temp, store, provider) =
			seed_provider("https://api.openai.com/v1");
		store
			.create_agent_binding("claude", &provider.id, None)
			.unwrap();
		store
			.create_agent_binding("codex", &provider.id, None)
			.unwrap();
		let (claude, codex, opencode) = temp_adapters(temp.path());

		delete_provider_references(
			&store, &provider, &claude, &codex, &opencode,
		)
		.unwrap();
		// Re-run: every reference is already gone; the cascade is a clean no-op.
		delete_provider_references(
			&store, &provider, &claude, &codex, &opencode,
		)
		.expect("cascade must be idempotent so a retry converges");
		assert!(store.list_agent_bindings("claude").unwrap().is_empty());
		assert!(store.list_agent_bindings("codex").unwrap().is_empty());
	}

	#[test]
	fn cascade_fails_closed_before_any_mutation_when_backend_unreachable() {
		// Regression (GitHub #15 P1a, Codex-found): `delete_provider_cascade`
		// used to read the API key for OpenCode matching only AFTER Claude
		// and Codex bindings were already torn down, so an unreachable
		// backend produced a HALF-finished delete. The precondition check
		// must run first and leave every binding + the inventory row
		// untouched. `BackendDownCredentialStore::delete_api_key` panics if
		// called, so an accidental mutation attempt fails this test loudly.
		let temp = tempfile::tempdir().unwrap();
		let store = InferenceProviderStore::with_credentials(
			temp.path(),
			BackendDownCredentialStore::default(),
		);
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "acme".to_string(),
				display_name: "acme".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: Vec::new(),
			})
			.unwrap();
		store
			.create_agent_binding("claude", &provider.id, None)
			.unwrap();

		let err = delete_provider_cascade(&store, &provider).unwrap_err();
		assert!(
			matches!(
				err,
				crate::error::InferenceProviderError::KeyringUnavailable(_)
			),
			"expected KeyringUnavailable, got {err:?}"
		);

		assert_eq!(
			store.list_agent_bindings("claude").unwrap().len(),
			1,
			"the claude binding must survive a failed-precondition delete"
		);
		assert!(
			store.list().unwrap().iter().any(|p| p.id == provider.id),
			"the provider row must survive a failed-precondition delete"
		);
	}

	/// Deterministically reproduces the P2-5 race window without real
	/// threads: the FIRST `get_api_key` call — `delete_provider_cascade`'s
	/// own precondition read, which runs UNLOCKED before any teardown or
	/// `store.delete` — returns the value as of that moment and, as a side
	/// effect, commits a DIFFERENT value into the backing map. That's
	/// exactly as if a concurrent `set_api_key` call had raced in and
	/// committed right after the precondition read returned. Every
	/// subsequent `get_api_key` call (`remove_opencode_references`'s own
	/// read, and — the one this test cares about — `store.delete`'s
	/// lock-scoped internal read) sees the already-updated value.
	#[derive(Debug, Clone)]
	struct RacingCredentialStore {
		values: Arc<Mutex<HashMap<String, String>>>,
		precondition_read_done: Arc<std::sync::atomic::AtomicBool>,
		key_after_concurrent_write: String,
	}

	impl CredentialStore for RacingCredentialStore {
		fn get_api_key(&self, provider_id: &str) -> Result<Option<String>> {
			let current = self.values.lock().unwrap().get(provider_id).cloned();
			if !self
				.precondition_read_done
				.swap(true, std::sync::atomic::Ordering::SeqCst)
			{
				// Simulate a concurrent `set_api_key` committing a NEW key
				// right after this (the precondition) read returns.
				self.values.lock().unwrap().insert(
					provider_id.to_string(),
					self.key_after_concurrent_write.clone(),
				);
			}
			Ok(current)
		}

		fn set_api_key(&self, provider_id: &str, api_key: &str) -> Result<()> {
			self.values
				.lock()
				.unwrap()
				.insert(provider_id.to_string(), api_key.to_string());
			Ok(())
		}

		fn delete_api_key(&self, provider_id: &str) -> Result<()> {
			self.values.lock().unwrap().remove(provider_id);
			Ok(())
		}
	}

	/// Regression (GitHub #15 P2-5, Codex-found): `delete_provider_cascade`
	/// used to read the API key ONCE — via its own precondition check, which
	/// runs UNLOCKED before any teardown — and thread THAT value into
	/// `store.delete` for rollback. If a concurrent `set_api_key` committed
	/// a NEWER key in the window between that read and `store.delete`
	/// acquiring its lock, an SQL-delete failure would roll back to the
	/// STALE precondition value, permanently clobbering the
	/// concurrently-committed key. Fixed by making `store.delete` read the
	/// current key itself, under its own lock, instead of trusting a value
	/// read outside it.
	///
	/// This test inlines `delete_provider_cascade`'s own steps (precondition
	/// read -> `delete_provider_references` -> `store.delete`) with
	/// TEMP-ROOTED adapters instead of calling `delete_provider_cascade`
	/// directly, because that fn hardcodes `ClaudeProviderAdapter::global()`
	/// etc., which touch the real home directory — exactly what every other
	/// `delete_provider_references`-driving test in this file also avoids.
	/// The SQL delete is forced to fail via a `BEFORE DELETE` trigger (same
	/// technique as `store.rs`'s migration tests) so `store.delete`'s
	/// rollback path actually runs.
	#[test]
	fn cascade_delete_rollback_never_restores_stale_precondition_value() {
		let temp = tempfile::tempdir().unwrap();
		let credentials = RacingCredentialStore {
			values: Arc::new(Mutex::new(HashMap::new())),
			precondition_read_done: Arc::new(
				std::sync::atomic::AtomicBool::new(false),
			),
			key_after_concurrent_write: "concurrently-committed-key"
				.to_string(),
		};
		let store = InferenceProviderStore::with_credentials(
			temp.path(),
			credentials.clone(),
		);
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "acme".to_string(),
				display_name: "acme".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "original-key".to_string(),
				models: Vec::new(),
			})
			.unwrap();
		assert_eq!(
			credentials
				.values
				.lock()
				.unwrap()
				.get(&provider.id)
				.cloned(),
			Some("original-key".to_string()),
			"precondition: the original key is really in the backend"
		);

		// Force the SQL delete to fail so `store.delete`'s rollback path
		// runs.
		let db_path = store.file_path();
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(async {
				use sqlx::ConnectOptions;
				let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
					.filename(&db_path)
					.connect()
					.await
					.unwrap();
				sqlx::query(
					"CREATE TRIGGER abort_provider_delete \
					 BEFORE DELETE ON inference_providers \
					 BEGIN SELECT RAISE(ABORT, 'simulated sql failure'); END;",
				)
				.execute(&mut conn)
				.await
				.unwrap();
			});

		// Inline `delete_provider_cascade`'s steps with temp-rooted adapters
		// (see doc comment above for why we don't call it directly).
		store.get_api_key(&provider.id).unwrap(); // precondition read (discarded)
		let (claude, codex, opencode) = temp_adapters(temp.path());
		delete_provider_references(
			&store, &provider, &claude, &codex, &opencode,
		)
		.unwrap();
		let err = store.delete(&provider.id).unwrap_err();

		assert!(
			matches!(err, crate::error::InferenceProviderError::Database(_)),
			"expected the aborted SQL delete to surface as a database \
			 error, got {err:?}"
		);
		assert_eq!(
			credentials
				.values
				.lock()
				.unwrap()
				.get(&provider.id)
				.cloned(),
			Some("concurrently-committed-key".to_string()),
			"rollback must restore the key that was CURRENT when `delete` \
			 read it under its own lock — never the stale value the \
			 earlier, unlocked precondition read captured"
		);
		assert!(
			store.get(&provider.id).is_ok(),
			"the provider row must survive the aborted SQL delete"
		);
	}
}
