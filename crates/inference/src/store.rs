//! SQLite-backed CRUD storage for inference providers.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{ConnectOptions, Row};

use crate::agent::{
	AgentProviderBinding, AgentProviderCredential, AgentProviderSource,
};
use crate::credentials::{CredentialStore, NativeCredentialStore};
use crate::error::{InferenceProviderError, Result};
use crate::model::{
	CreateInferenceProvider, InferenceProvider, InferenceProviderFormat,
	UpdateInferenceProvider,
};

/// SQLite database file name under the app data directory.
pub const INFERENCE_PROVIDERS_FILE: &str = "inference_providers.db";

/// Per-`app_data_dir` mutex serializing the credential read-modify-write
/// sequences (`create`/`update`/`delete`/`set_api_key`/`delete_api_key`)
/// that touch BOTH the keyring and the matching SQLite row for a single
/// provider's API key. `CredentialStore`'s own contract (see
/// `credentials.rs`) already says backends may not handle concurrent writes
/// reliably — but even a backend that did would still race: two concurrent
/// "read the old key, write a new key, roll back to the old key on SQL
/// failure" sequences targeting the SAME provider could have one sequence's
/// rollback stomp the OTHER sequence's already-committed new key, leaving
/// the DB (which only ever sees one UPDATE actually win) out of sync with
/// the keyring (GitHub #15 P2-5).
///
/// Keyed by `app_data_dir` rather than held as a field on
/// `InferenceProviderStore` itself: the API constructs a fresh
/// `InferenceProviderStore` per request (see `routes::inference::store`), so
/// a lock living on the struct would never actually be shared across two
/// concurrent requests. Every store pointed at the same app data dir shares
/// the same lock instead.
///
/// In-process only — cross-process keyring races remain a documented known
/// limitation, the same caveat `routes::credentials`'s
/// `SOURCE_BINDINGS_MUTEX` already carries for the sibling credential store.
// ponytail: the registry never evicts entries, so it grows by one per
// distinct `app_data_dir` ever seen by this process. Fine for the API (one
// long-lived app data dir) and for tests (short-lived processes); revisit
// with an eviction/LRU policy only if something starts opening many distinct
// app data dirs over one long-lived process's lifetime.
fn credential_lock_for(app_data_dir: &Path) -> Arc<Mutex<()>> {
	static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
		OnceLock::new();
	let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
	let mut map = locks.lock().unwrap_or_else(|e| e.into_inner());
	map.entry(app_data_dir.to_path_buf())
		.or_insert_with(|| Arc::new(Mutex::new(())))
		.clone()
}

/// CRUD interface for inference provider metadata and API keys.
pub trait InferenceProviderRepository {
	/// List all providers.
	fn list(&self) -> Result<Vec<InferenceProvider>>;

	/// Get one provider by ID.
	fn get(&self, id: &str) -> Result<InferenceProvider>;

	/// Create a provider and store its API key in the native credential store.
	fn create(
		&self,
		input: CreateInferenceProvider,
	) -> Result<InferenceProvider>;

	/// Update provider metadata and optionally replace its API key.
	fn update(
		&self,
		id: &str,
		input: UpdateInferenceProvider,
	) -> Result<InferenceProvider>;

	/// Delete provider metadata and its API key.
	///
	/// Reads the CURRENT API key itself, under the same lock that serializes
	/// this call against `set_api_key` (see `credential_lock_for`), and uses
	/// that value for rollback if the SQL delete fails after the keyring
	/// delete has already succeeded. The read is fail-hard: if it errors,
	/// `delete` returns that error immediately without touching the keyring
	/// or the database.
	///
	/// This method used to accept the api key as a caller-supplied
	/// `known_api_key` parameter (reused from `delete_provider_cascade`'s own
	/// precondition read) instead of reading it itself. That was a stale-read
	/// race (GitHub #15 P2-5): the caller's read happened OUTSIDE this
	/// method's lock, so a concurrent `set_api_key` committing a newer key in
	/// between could have its result clobbered by this method's rollback
	/// restoring the caller's now-stale value. Reading under the lock here
	/// closes that window — this is the ONLY read whose result rollback can
	/// ever use, and it can't be stale relative to lock-ordering. A
	/// best-effort/degrade-to-`None` read was considered and rejected: that
	/// would silently disable the rollback on a transient read failure,
	/// which is a worse outcome than fail-hard for a security-sensitive
	/// value (see GitHub #15 P1c).
	fn delete(&self, id: &str) -> Result<InferenceProvider>;

	/// Read the provider API key from the native credential store.
	fn get_api_key(&self, id: &str) -> Result<Option<String>>;

	/// Replace the provider API key in the native credential store.
	fn set_api_key(&self, id: &str, api_key: &str) -> Result<()>;

	/// Delete the provider API key from the native credential store.
	fn delete_api_key(&self, id: &str) -> Result<()>;
}

/// SQLite-backed inference provider store.
#[derive(Debug, Clone)]
pub struct InferenceProviderStore<C = NativeCredentialStore> {
	app_data_dir: PathBuf,
	credentials: C,
}

impl InferenceProviderStore<NativeCredentialStore> {
	/// Create a store rooted at a Tauri app data directory path.
	pub fn new(app_data_dir: impl Into<PathBuf>) -> Self {
		Self::with_credentials(app_data_dir, NativeCredentialStore)
	}

	/// Create a store from the current Tauri app handle.
	#[cfg(feature = "tauri")]
	pub fn from_tauri<R: tauri::Runtime>(
		app: &tauri::AppHandle<R>,
	) -> Result<Self> {
		use tauri::Manager;

		let app_data_dir = app.path().app_data_dir().map_err(|error| {
			InferenceProviderError::AppDataDir(error.to_string())
		})?;
		Ok(Self::new(app_data_dir))
	}
}

impl<C> InferenceProviderStore<C> {
	/// Create a store with an explicit credential store implementation.
	pub fn with_credentials(
		app_data_dir: impl Into<PathBuf>,
		credentials: C,
	) -> Self {
		Self {
			app_data_dir: app_data_dir.into(),
			credentials,
		}
	}

	/// App data directory used by this store.
	pub fn app_data_dir(&self) -> &Path {
		&self.app_data_dir
	}

	/// Full path to `inference_providers.db`.
	pub fn file_path(&self) -> PathBuf {
		self.app_data_dir.join(INFERENCE_PROVIDERS_FILE)
	}

	/// Bridge a future to synchronous callers.
	///
	/// Safe to call from `spawn_blocking` threads (Rocket handlers) or from
	/// plain synchronous code that has no active runtime. Must NOT be called
	/// from within an async task — use `block_in_place` for that.
	fn block_on<F>(&self, fut: F) -> F::Output
	where
		F: Future,
	{
		match tokio::runtime::Handle::try_current() {
			Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
			Err(_) => tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.expect("failed to build tokio runtime")
				.block_on(fut),
		}
	}
}

impl<C: CredentialStore> InferenceProviderStore<C> {
	async fn open_db(&self) -> Result<SqliteConnection> {
		let db_path = self.file_path();
		if let Some(parent) = db_path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		let mut conn = SqliteConnectOptions::new()
			.filename(&db_path)
			.create_if_missing(true)
			.connect()
			.await?;
		sqlx::query("PRAGMA foreign_keys = ON")
			.execute(&mut conn)
			.await?;
		sqlx::migrate!().run(&mut conn).await?;
		Ok(conn)
	}

	async fn fetch_by_id(
		conn: &mut SqliteConnection,
		id: &str,
	) -> Result<InferenceProvider> {
		let row = sqlx::query(
			"SELECT id, latin_name, display_name, format, api_base_url, \
                 preset, masked_api_key \
             FROM inference_providers WHERE id = ?",
		)
		.bind(id)
		.fetch_optional(&mut *conn)
		.await?;

		let mut provider = row
			.map(map_row)
			.transpose()?
			.ok_or_else(|| InferenceProviderError::NotFound(id.to_string()))?;
		provider.models = Self::fetch_model_names(conn, &provider.id).await?;
		Ok(provider)
	}

	async fn check_latin_name_unique(
		conn: &mut SqliteConnection,
		latin_name: &str,
		ignore_id: Option<&str>,
	) -> Result<()> {
		let count: i64 = if let Some(id) = ignore_id {
			sqlx::query_scalar(
				"SELECT COUNT(*) FROM inference_providers \
                 WHERE latin_name = ? AND id != ?",
			)
			.bind(latin_name)
			.bind(id)
			.fetch_one(conn)
			.await?
		} else {
			sqlx::query_scalar(
				"SELECT COUNT(*) FROM inference_providers \
                 WHERE latin_name = ?",
			)
			.bind(latin_name)
			.fetch_one(conn)
			.await?
		};

		if count > 0 {
			Err(InferenceProviderError::AlreadyExists(
				latin_name.to_string(),
			))
		} else {
			Ok(())
		}
	}

	async fn fetch_model_names(
		conn: &mut SqliteConnection,
		provider_id: &str,
	) -> Result<Vec<String>> {
		let rows = sqlx::query(
			"SELECT name FROM inference_models \
             WHERE provider_id = ? ORDER BY rowid",
		)
		.bind(provider_id)
		.fetch_all(conn)
		.await?;

		rows.into_iter()
			.map(|row| row.try_get("name").map_err(Into::into))
			.collect()
	}

	async fn replace_models(
		conn: &mut SqliteConnection,
		provider_id: &str,
		models: &[String],
	) -> Result<()> {
		sqlx::query("DELETE FROM inference_models WHERE provider_id = ?")
			.bind(provider_id)
			.execute(&mut *conn)
			.await?;

		for model in models {
			sqlx::query(
				"INSERT INTO inference_models (provider_id, name) \
                 VALUES (?, ?)",
			)
			.bind(provider_id)
			.bind(model)
			.execute(&mut *conn)
			.await?;
		}

		Ok(())
	}
}

/// Build the WARN message for a failed rollback step, or `None` when it
/// succeeded. Pure so the message contract can be unit-tested without a logger.
fn rollback_failure_message(
	op: &str,
	id: &str,
	result: &Result<()>,
) -> Option<String> {
	result.as_ref().err().map(|error| {
		format!(
			"inference provider {id}: rollback step '{op}' failed after a \
			 primary error; stored state may be inconsistent: {error}"
		)
	})
}

/// Log a best-effort rollback/cleanup failure instead of silently dropping it.
///
/// These run only after a PRIMARY operation already failed: the primary error is
/// what the caller gets back (a rollback can't change that), but a rollback that
/// ALSO fails leaves stored state inconsistent (e.g. a keyring key that couldn't
/// be restored after a DB error). Swallowing it with `let _` hid that second
/// failure behind the first; logging at WARN makes it observable. `op` names the
/// rollback action; `id` is the provider it targeted.
fn log_rollback_failure(op: &str, id: &str, result: Result<()>) {
	if let Some(message) = rollback_failure_message(op, id, &result) {
		log::warn!("{message}");
	}
}

fn map_row(row: sqlx::sqlite::SqliteRow) -> Result<InferenceProvider> {
	let format_str: String = row.try_get("format")?;
	let format = format_str.parse::<InferenceProviderFormat>()?;
	Ok(InferenceProvider {
		id: row.try_get("id")?,
		latin_name: row.try_get("latin_name")?,
		display_name: row.try_get("display_name")?,
		format,
		api_base_url: row.try_get("api_base_url")?,
		preset: row.try_get("preset")?,
		masked_api_key: row.try_get("masked_api_key")?,
		models: Vec::new(),
	})
}

impl<C: CredentialStore> InferenceProviderRepository
	for InferenceProviderStore<C>
{
	fn list(&self) -> Result<Vec<InferenceProvider>> {
		self.block_on(async {
			let mut conn = self.open_db().await?;
			let rows = sqlx::query(
				"SELECT id, latin_name, display_name, format, api_base_url, \
                     preset, masked_api_key \
                 FROM inference_providers ORDER BY rowid",
			)
			.fetch_all(&mut conn)
			.await?;
			let mut providers = Vec::with_capacity(rows.len());
			for row in rows {
				let mut provider = map_row(row)?;
				provider.models =
					Self::fetch_model_names(&mut conn, &provider.id).await?;
				providers.push(provider);
			}
			Ok(providers)
		})
	}

	fn get(&self, id: &str) -> Result<InferenceProvider> {
		self.block_on(async {
			let mut conn = self.open_db().await?;
			Self::fetch_by_id(&mut conn, id).await
		})
	}

	fn create(
		&self,
		input: CreateInferenceProvider,
	) -> Result<InferenceProvider> {
		let latin_name = clean_latin_name(&input.latin_name)?;
		let display_name = clean_display_name(&input.display_name)?;
		let api_base_url = clean_api_base_url(&input.api_base_url)?;
		let preset = clean_optional_preset(input.preset.as_deref());
		let models = clean_model_names(&input.models)?;
		ensure_api_key(&input.api_key)?;

		// See `credential_lock_for` (GitHub #15 P2-5): serializes this
		// keyring+SQL sequence against every other provider mutation
		// sharing this app data dir.
		let lock = credential_lock_for(&self.app_data_dir);
		let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

		self.block_on(async {
			let mut conn = self.open_db().await?;
			Self::check_latin_name_unique(&mut conn, &latin_name, None).await?;

			let provider = InferenceProvider {
				id: uuid::Uuid::new_v4().to_string(),
				latin_name,
				display_name,
				format: input.format,
				api_base_url,
				preset,
				masked_api_key: mask_api_key(&input.api_key),
				models,
			};

			self.credentials.set_api_key(&provider.id, &input.api_key)?;

			let result: Result<()> = async {
				sqlx::query(
					"INSERT INTO inference_providers \
                     (id, latin_name, display_name, format, api_base_url, \
                      preset, masked_api_key) \
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
				)
				.bind(&provider.id)
				.bind(&provider.latin_name)
				.bind(&provider.display_name)
				.bind(provider.format.to_string())
				.bind(&provider.api_base_url)
				.bind(&provider.preset)
				.bind(&provider.masked_api_key)
				.execute(&mut conn)
				.await?;
				Self::replace_models(&mut conn, &provider.id, &provider.models)
					.await?;
				Ok(())
			}
			.await;

			if let Err(error) = result {
				log_rollback_failure(
					"delete_api_key",
					&provider.id,
					self.credentials.delete_api_key(&provider.id),
				);
				let row_rollback =
					sqlx::query("DELETE FROM inference_providers WHERE id = ?")
						.bind(&provider.id)
						.execute(&mut conn)
						.await;
				if let Err(rollback) = row_rollback {
					log::warn!(
						"inference provider {}: rollback step \
						 'delete_row' failed after a primary error; an orphan \
						 row may remain: {rollback}",
						provider.id
					);
				}
				return Err(error);
			}

			Ok(provider)
		})
	}

	fn update(
		&self,
		id: &str,
		input: UpdateInferenceProvider,
	) -> Result<InferenceProvider> {
		let models = input
			.models
			.as_ref()
			.map(|models| clean_model_names(models))
			.transpose()?;

		// See `credential_lock_for` (GitHub #15 P2-5).
		let lock = credential_lock_for(&self.app_data_dir);
		let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

		self.block_on(async {
			let mut conn = self.open_db().await?;
			let mut provider = Self::fetch_by_id(&mut conn, id).await?;

			if let Some(ref latin_name) = input.latin_name {
				let latin_name = clean_latin_name(latin_name)?;
				Self::check_latin_name_unique(&mut conn, &latin_name, Some(id))
					.await?;
				provider.latin_name = latin_name;
			}

			if let Some(ref display_name) = input.display_name {
				provider.display_name = clean_display_name(display_name)?;
			}

			if let Some(format) = input.format {
				provider.format = format;
			}

			if let Some(ref api_base_url) = input.api_base_url {
				provider.api_base_url = clean_api_base_url(api_base_url)?;
			}

			if let Some(ref preset) = input.preset {
				provider.preset = clean_optional_preset(preset.as_deref());
			}

			if let Some(models) = models {
				provider.models = models;
			}

			let previous_api_key = match input.api_key.as_ref() {
				Some(api_key) => {
					ensure_api_key(api_key)?;
					let previous = self.credentials.get_api_key(id)?;
					self.credentials.set_api_key(id, api_key)?;
					provider.masked_api_key = mask_api_key(api_key);
					Some(previous)
				}
				None => None,
			};

			let result: Result<()> = async {
				sqlx::query(
					"UPDATE inference_providers \
                     SET latin_name = ?, display_name = ?, format = ?, \
                         api_base_url = ?, preset = ?, masked_api_key = ? \
                     WHERE id = ?",
				)
				.bind(&provider.latin_name)
				.bind(&provider.display_name)
				.bind(provider.format.to_string())
				.bind(&provider.api_base_url)
				.bind(&provider.preset)
				.bind(&provider.masked_api_key)
				.bind(id)
				.execute(&mut conn)
				.await?;
				if input.models.is_some() {
					Self::replace_models(&mut conn, id, &provider.models)
						.await?;
				}
				Ok(())
			}
			.await;

			if let Err(error) = result {
				if let Some(previous) = previous_api_key {
					match previous {
						Some(key) => log_rollback_failure(
							"restore_api_key",
							id,
							self.credentials.set_api_key(id, &key),
						),
						None => log_rollback_failure(
							"delete_api_key",
							id,
							self.credentials.delete_api_key(id),
						),
					}
				}
				return Err(error);
			}

			Ok(provider)
		})
	}

	fn delete(&self, id: &str) -> Result<InferenceProvider> {
		// See `credential_lock_for` (GitHub #15 P2-5).
		let lock = credential_lock_for(&self.app_data_dir);
		let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

		self.block_on(async {
			let mut conn = self.open_db().await?;
			let provider = Self::fetch_by_id(&mut conn, id).await?;

			// Read the CURRENT key under this same lock (see the trait doc
			// comment on `delete`, GitHub #15 P2-5): this is the only read
			// the rollback below can use, so it can never be stale relative
			// to a concurrent `set_api_key`. Fail-hard on a read error
			// (propagates via `?`) instead of degrading to `None` -- a
			// best-effort read would silently disable the rollback (GitHub
			// #15 P1c). `self.credentials` is the `CredentialStore` directly
			// (never a store-level locking method), so this cannot
			// self-deadlock against the `lock` already held above.
			let known_api_key = self.credentials.get_api_key(id)?;

			self.credentials.delete_api_key(id)?;

			let result =
				sqlx::query("DELETE FROM inference_providers WHERE id = ?")
					.bind(id)
					.execute(&mut conn)
					.await;

			if let Err(error) = result {
				if let Some(key) = known_api_key {
					log_rollback_failure(
						"restore_api_key",
						id,
						self.credentials.set_api_key(id, &key),
					);
				}
				return Err(error.into());
			}

			Ok(provider)
		})
	}

	fn get_api_key(&self, id: &str) -> Result<Option<String>> {
		self.get(id)?;
		self.credentials.get_api_key(id)
	}

	fn set_api_key(&self, id: &str, api_key: &str) -> Result<()> {
		ensure_api_key(api_key)?;
		// See `credential_lock_for` (GitHub #15 P2-5): without this, two
		// concurrent `set_api_key` calls for the same provider can both read
		// the SAME previous key, then one's rollback (on a losing/failing
		// SQL write) stomps the other's already-committed new key.
		let lock = credential_lock_for(&self.app_data_dir);
		let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
		self.block_on(async {
			let mut conn = self.open_db().await?;
			Self::fetch_by_id(&mut conn, id).await?;
			let previous = self.credentials.get_api_key(id)?;
			self.credentials.set_api_key(id, api_key)?;

			let result = sqlx::query(
				"UPDATE inference_providers SET masked_api_key = ? \
                 WHERE id = ?",
			)
			.bind(mask_api_key(api_key))
			.bind(id)
			.execute(&mut conn)
			.await;

			if let Err(error) = result {
				match previous {
					Some(key) => log_rollback_failure(
						"restore_api_key",
						id,
						self.credentials.set_api_key(id, &key),
					),
					None => log_rollback_failure(
						"delete_api_key",
						id,
						self.credentials.delete_api_key(id),
					),
				}
				return Err(error.into());
			}

			Ok(())
		})
	}

	fn delete_api_key(&self, id: &str) -> Result<()> {
		// See `credential_lock_for` (GitHub #15 P2-5).
		let lock = credential_lock_for(&self.app_data_dir);
		let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
		self.block_on(async {
			let mut conn = self.open_db().await?;
			Self::fetch_by_id(&mut conn, id).await?;
			let previous = self.credentials.get_api_key(id)?;
			self.credentials.delete_api_key(id)?;

			let result = sqlx::query(
				"UPDATE inference_providers SET masked_api_key = '' \
                 WHERE id = ?",
			)
			.bind(id)
			.execute(&mut conn)
			.await;

			if let Err(error) = result {
				if let Some(key) = previous {
					log_rollback_failure(
						"restore_api_key",
						id,
						self.credentials.set_api_key(id, &key),
					);
				}
				return Err(error.into());
			}

			Ok(())
		})
	}
}

fn clean_model_name(name: &str) -> Result<String> {
	let name = name.trim();
	if name.is_empty() {
		Err(InferenceProviderError::EmptyModelName)
	} else {
		Ok(name.to_string())
	}
}

fn clean_model_names(models: &[String]) -> Result<Vec<String>> {
	let mut seen = HashSet::new();
	let mut clean = Vec::with_capacity(models.len());

	for model in models {
		let model = clean_model_name(model)?;
		if !seen.insert(model.to_ascii_lowercase()) {
			return Err(InferenceProviderError::ModelAlreadyExists(model));
		}
		clean.push(model);
	}

	Ok(clean)
}

fn clean_optional_preset(preset: Option<&str>) -> Option<String> {
	preset.and_then(|value| {
		let value = value.trim();
		if value.is_empty() {
			None
		} else {
			Some(value.to_string())
		}
	})
}

fn clean_latin_name(latin_name: &str) -> Result<String> {
	let latin_name = latin_name.trim();
	if latin_name.is_empty() {
		Err(InferenceProviderError::EmptyName)
	} else if !latin_name.bytes().all(|byte| byte.is_ascii_lowercase()) {
		Err(InferenceProviderError::InvalidLatinName(
			latin_name.to_string(),
		))
	} else {
		Ok(latin_name.to_string())
	}
}

fn clean_display_name(display_name: &str) -> Result<String> {
	let display_name = display_name.trim();
	if display_name.is_empty() {
		Err(InferenceProviderError::EmptyName)
	} else {
		Ok(display_name.to_string())
	}
}

fn ensure_api_key(api_key: &str) -> Result<()> {
	if api_key.trim().is_empty() {
		Err(InferenceProviderError::EmptyApiKey)
	} else {
		Ok(())
	}
}

fn mask_api_key(api_key: &str) -> String {
	let value = api_key.trim();
	let chars = value.chars().collect::<Vec<_>>();
	let len = chars.len();

	if len <= 4 {
		return "*".repeat(len);
	}

	let visible_each = (len / 6).clamp(1, 6);
	let mask_len = len.saturating_sub(visible_each * 2);
	let prefix = chars.iter().take(visible_each).collect::<String>();
	let suffix = chars.iter().skip(len - visible_each).collect::<String>();

	format!("{prefix}{}{suffix}", "*".repeat(mask_len))
}

fn clean_api_base_url(api_base_url: &str) -> Result<String> {
	let api_base_url = api_base_url.trim();
	if api_base_url.is_empty() {
		Err(InferenceProviderError::EmptyApiBaseUrl)
	} else {
		Ok(api_base_url.to_string())
	}
}

// ============================================================================
// Agent-provider binding table methods
// ============================================================================

/// Data model for an agent-provider binding row.
///
/// Active state is NOT stored here; it is derived by comparing the agent's
/// current config against the bound provider's details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProviderBindingRow {
	pub id: String,
	pub agent_id: String,
	pub inference_provider_id: String,
	pub model: Option<String>,
}

impl<C: CredentialStore> InferenceProviderStore<C> {
	async fn fetch_agent_binding(
		conn: &mut SqliteConnection,
		agent_id: &str,
		binding_id: &str,
	) -> Result<AgentProviderBindingRow> {
		let row = sqlx::query(
			"SELECT id, agent_id, inference_provider_id, model \
			 FROM agent_provider_bindings \
			 WHERE agent_id = ? AND id = ?",
		)
		.bind(agent_id)
		.bind(binding_id)
		.fetch_optional(&mut *conn)
		.await?;

		match row {
			Some(row) => Ok(AgentProviderBindingRow {
				id: row.try_get("id")?,
				agent_id: row.try_get("agent_id")?,
				inference_provider_id: row.try_get("inference_provider_id")?,
				model: row.try_get("model")?,
			}),
			None => {
				Err(InferenceProviderError::NotFound(binding_id.to_string()))
			}
		}
	}

	/// List all bindings for a given agent.
	pub fn list_agent_bindings(
		&self,
		agent_id: &str,
	) -> Result<Vec<AgentProviderBindingRow>> {
		self.block_on(async {
			let mut conn = self.open_db().await?;
			let rows = sqlx::query(
				"SELECT id, agent_id, inference_provider_id, model \
				 FROM agent_provider_bindings \
				 WHERE agent_id = ? \
				 ORDER BY created_at",
			)
			.bind(agent_id)
			.fetch_all(&mut conn)
			.await?;

			rows.into_iter()
				.map(|row| {
					Ok(AgentProviderBindingRow {
						id: row.try_get("id")?,
						agent_id: row.try_get("agent_id")?,
						inference_provider_id: row
							.try_get("inference_provider_id")?,
						model: row.try_get("model")?,
					})
				})
				.collect::<Result<Vec<_>>>()
		})
	}

	/// Get a single binding by its id and agent.
	pub fn get_agent_binding(
		&self,
		agent_id: &str,
		binding_id: &str,
	) -> Result<AgentProviderBindingRow> {
		self.block_on(async {
			let mut conn = self.open_db().await?;
			Self::fetch_agent_binding(&mut conn, agent_id, binding_id).await
		})
	}

	/// Create a binding.
	pub fn create_agent_binding(
		&self,
		agent_id: &str,
		inference_provider_id: &str,
		model: Option<&str>,
	) -> Result<AgentProviderBindingRow> {
		self.block_on(async {
			let mut conn = self.open_db().await?;

			// Verify the inference provider exists.
			let _: InferenceProvider =
				Self::fetch_by_id(&mut conn, inference_provider_id).await?;

			let binding = AgentProviderBindingRow {
				id: uuid::Uuid::new_v4().to_string(),
				agent_id: agent_id.to_string(),
				inference_provider_id: inference_provider_id.to_string(),
				model: model.map(ToString::to_string),
			};

			sqlx::query(
				"INSERT INTO agent_provider_bindings \
				 (id, agent_id, inference_provider_id, model) \
				 VALUES (?, ?, ?, ?)",
			)
			.bind(&binding.id)
			.bind(&binding.agent_id)
			.bind(&binding.inference_provider_id)
			.bind(&binding.model)
			.execute(&mut conn)
			.await?;

			Ok(binding)
		})
	}

	/// Create or replace a binding with an explicit public id.
	pub fn upsert_agent_binding(
		&self,
		agent_id: &str,
		binding_id: &str,
		inference_provider_id: &str,
		model: Option<&str>,
	) -> Result<AgentProviderBindingRow> {
		self.block_on(async {
			let mut conn = self.open_db().await?;

			let _: InferenceProvider =
				Self::fetch_by_id(&mut conn, inference_provider_id).await?;

			let binding = AgentProviderBindingRow {
				id: binding_id.to_string(),
				agent_id: agent_id.to_string(),
				inference_provider_id: inference_provider_id.to_string(),
				model: model.map(ToString::to_string),
			};

			sqlx::query(
				"INSERT INTO agent_provider_bindings \
				 (id, agent_id, inference_provider_id, model) \
				 VALUES (?, ?, ?, ?) \
				 ON CONFLICT(agent_id, id) DO UPDATE SET \
				 inference_provider_id = excluded.inference_provider_id, \
				 model = excluded.model, \
				 updated_at = datetime('now')",
			)
			.bind(&binding.id)
			.bind(&binding.agent_id)
			.bind(&binding.inference_provider_id)
			.bind(&binding.model)
			.execute(&mut conn)
			.await?;

			Ok(binding)
		})
	}

	/// Update a binding's model.
	pub fn update_agent_binding(
		&self,
		agent_id: &str,
		binding_id: &str,
		model: Option<Option<String>>,
	) -> Result<AgentProviderBindingRow> {
		self.block_on(async {
			let mut conn = self.open_db().await?;
			let mut binding =
				Self::fetch_agent_binding(&mut conn, agent_id, binding_id)
					.await?;

			if let Some(model) = model {
				binding.model = model;
			}

			sqlx::query(
				"UPDATE agent_provider_bindings \
				 SET model = ? \
				 WHERE agent_id = ? AND id = ?",
			)
			.bind(&binding.model)
			.bind(agent_id)
			.bind(binding_id)
			.execute(&mut conn)
			.await?;

			Ok(binding)
		})
	}

	/// Delete a binding by id.
	pub fn delete_agent_binding(
		&self,
		agent_id: &str,
		binding_id: &str,
	) -> Result<AgentProviderBindingRow> {
		self.block_on(async {
			let mut conn = self.open_db().await?;
			let binding =
				Self::fetch_agent_binding(&mut conn, agent_id, binding_id)
					.await?;

			sqlx::query(
				"DELETE FROM agent_provider_bindings \
				 WHERE agent_id = ? AND id = ?",
			)
			.bind(agent_id)
			.bind(binding_id)
			.execute(&mut conn)
			.await?;

			Ok(binding)
		})
	}

	/// Build an `AgentProviderBinding` from a binding row + inventory provider.
	pub fn binding_from_row(
		&self,
		row: &AgentProviderBindingRow,
	) -> Result<AgentProviderBinding> {
		let provider = self.get(&row.inference_provider_id)?;
		let api_key = self.credentials.get_api_key(&provider.id)?;

		AgentProviderBinding::from_inventory(
			row.id.clone(),
			&provider,
			match api_key {
				Some(_) => AgentProviderCredential::EnvVar {
					name: "AGHUB_INFERENCE_API_KEY".to_string(),
				},
				None => AgentProviderCredential::None,
			},
			AgentProviderSource::Custom,
		)
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;
	use std::sync::{Arc, Mutex};

	use super::*;
	use crate::model::InferenceProviderFormat;

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

	fn store() -> (
		tempfile::TempDir,
		InferenceProviderStore<MemoryCredentialStore>,
	) {
		let temp = tempfile::tempdir().unwrap();
		let store = InferenceProviderStore::with_credentials(
			temp.path(),
			MemoryCredentialStore::default(),
		);
		(temp, store)
	}

	fn create_provider(
		store: &InferenceProviderStore<MemoryCredentialStore>,
		latin_name: &str,
	) -> InferenceProvider {
		store
			.create(CreateInferenceProvider {
				latin_name: latin_name.to_string(),
				display_name: latin_name.to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: Vec::new(),
			})
			.unwrap()
	}

	#[test]
	fn test_migrates_existing_v6_binding_database() {
		let (temp, store) = store();
		let db_path = temp.path().join(INFERENCE_PROVIDERS_FILE);
		store.block_on(async {
			let mut conn = SqliteConnectOptions::new()
				.filename(&db_path)
				.create_if_missing(true)
				.connect()
				.await
				.unwrap();
			sqlx::query(
				"CREATE TABLE _sqlx_migrations (
					version BIGINT PRIMARY KEY,
					description TEXT NOT NULL,
					installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
					success BOOLEAN NOT NULL,
					checksum BLOB NOT NULL,
					execution_time BIGINT NOT NULL
				)",
			)
			.execute(&mut conn)
			.await
			.unwrap();
			sqlx::query(
				"CREATE TABLE inference_providers (
					id TEXT PRIMARY KEY NOT NULL,
					name TEXT NOT NULL UNIQUE,
					format TEXT NOT NULL,
					api_base_url TEXT NOT NULL,
					masked_api_key TEXT NOT NULL DEFAULT '',
					display_name TEXT NOT NULL DEFAULT ''
				)",
			)
			.execute(&mut conn)
			.await
			.unwrap();
			sqlx::query(
				"CREATE TABLE inference_models (
					id TEXT PRIMARY KEY NOT NULL,
					provider_id TEXT NOT NULL,
					name TEXT NOT NULL,
					created_at TEXT NOT NULL DEFAULT (datetime('now')),
					FOREIGN KEY (provider_id)
						REFERENCES inference_providers(id)
						ON DELETE CASCADE,
					UNIQUE (provider_id, name)
				)",
			)
			.execute(&mut conn)
			.await
			.unwrap();
			sqlx::query(
				"CREATE TABLE agent_provider_bindings (
					id TEXT PRIMARY KEY NOT NULL,
					agent_id TEXT NOT NULL,
					inference_provider_id TEXT NOT NULL,
					model TEXT,
					created_at TEXT NOT NULL DEFAULT (datetime('now')),
					updated_at TEXT NOT NULL DEFAULT (datetime('now')),
					FOREIGN KEY (inference_provider_id)
						REFERENCES inference_providers(id)
						ON DELETE CASCADE
				)",
			)
			.execute(&mut conn)
			.await
			.unwrap();

			let migrations = [
				(
					1,
					"create inference providers",
					"bf935d5229df4e204f7e0cc2f14721dbfeb45c9a15c229dca50127407ec9fc2311906fa98f567db786f9bf3a4dbc7412",
				),
				(
					2,
					"create inference models",
					"95502c735b08fa0f0074c6885090be68f2b033da1c918e0a825279169faace7fe9fa4fd64b08bf2417f892d65597d0fd",
				),
				(
					3,
					"add masked api key",
					"ff15b82d3ce15332f0ee9c0264db8340326bfa025fb554c32275c33b1be11d29cea5ad4386f4c2c2350e42c61a145b1e",
				),
				(
					4,
					"add display name",
					"5bdac59333690e70da935d043ababf088fe53f7c836a250855472288b38bda5952c3117936bf1b054f662adb3cd7ce48",
				),
				(
					5,
					"create agent provider bindings",
					"14ca7c3e31001e23f4646d99d9dbd27badfcc4bf595eec9ccd4bcbe7bd69e17cae9d5c1eba1ea515764b395c41abf27a",
				),
				(
					6,
					"drop binding is active",
					"a442005806c4fa8c07d3beab28091ede276eea39ea6529df31c1bfa784ea70f14320223fd513d5a84f6121371800417c",
				),
			];
			for (version, description, checksum) in migrations {
				let query = format!(
					"INSERT INTO _sqlx_migrations
						(version, description, success, checksum, execution_time)
					VALUES ({version}, '{description}', 1, x'{checksum}', 0)"
				);
				sqlx::query(sqlx::AssertSqlSafe(query))
					.execute(&mut conn)
					.await
					.unwrap();
			}
		});

		assert!(store.list().unwrap().is_empty());

		store.block_on(async {
			let mut conn = SqliteConnectOptions::new()
				.filename(&db_path)
				.connect()
				.await
				.unwrap();
			let version: i64 =
				sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
					.fetch_one(&mut conn)
					.await
					.unwrap();
			assert_eq!(version, 9);

			let trigger_count: i64 = sqlx::query_scalar(
				"SELECT COUNT(*) FROM sqlite_master
				 WHERE type = 'trigger'
				 AND name = 'trg_agent_provider_bindings_updated_at'",
			)
			.fetch_one(&mut conn)
			.await
			.unwrap();
			assert_eq!(trigger_count, 1);

			let unique_count: i64 = sqlx::query_scalar(
				"SELECT COUNT(*) FROM pragma_index_list(
					'agent_provider_bindings'
				)
				WHERE [unique] = 1",
			)
			.fetch_one(&mut conn)
			.await
			.unwrap();
			assert_eq!(unique_count, 1);
		});
	}

	#[test]
	fn test_list_missing_file_is_empty() {
		let (_temp, store) = store();

		assert!(store.list().unwrap().is_empty());
	}

	#[test]
	fn test_create_stores_metadata_without_api_key() {
		let (_temp, store) = store();

		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "openai".to_string(),
				display_name: "OpenAI".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "sk-test".to_string(),
				models: Vec::new(),
			})
			.unwrap();

		// Metadata is persisted and retrievable
		let fetched = store.get(&provider.id).unwrap();
		assert_eq!(fetched.latin_name, "openai");
		assert_eq!(fetched.display_name, "OpenAI");
		assert_eq!(fetched.format, InferenceProviderFormat::OpenAiResponses);
		assert_eq!(fetched.api_base_url, "https://api.openai.com/v1");
		assert_eq!(fetched.preset, None);
		assert_eq!(fetched.masked_api_key, "s*****t");
		assert!(fetched.models.is_empty());

		// API key is kept in the credential store, not in provider metadata
		assert!(store
			.list()
			.unwrap()
			.iter()
			.all(|p| { !format!("{p:?}").contains("sk-test") }));
		assert_eq!(
			store.get_api_key(&provider.id).unwrap(),
			Some("sk-test".to_string())
		);
	}

	#[test]
	fn test_create_and_clear_provider_preset() {
		let (_temp, store) = store();
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "openrouter".to_string(),
				display_name: "OpenRouter".to_string(),
				format: InferenceProviderFormat::OpenAiCompletions,
				api_base_url: "https://openrouter.ai/api/v1".to_string(),
				preset: Some(" openrouter ".to_string()),
				api_key: "secret".to_string(),
				models: Vec::new(),
			})
			.unwrap();

		assert_eq!(provider.preset.as_deref(), Some("openrouter"));
		assert_eq!(
			store.get(&provider.id).unwrap().preset.as_deref(),
			Some("openrouter")
		);

		let updated = store
			.update(
				&provider.id,
				UpdateInferenceProvider {
					latin_name: None,
					display_name: None,
					format: None,
					api_base_url: None,
					preset: Some(None),
					api_key: None,
					models: None,
				},
			)
			.unwrap();

		assert_eq!(updated.preset, None);
	}

	#[test]
	fn test_mask_api_key_uses_length_ratio() {
		assert_eq!(mask_api_key("abc"), "***");
		assert_eq!(mask_api_key("abcde"), "a***e");
		assert_eq!(mask_api_key("sk-test"), "s*****t");
		assert_eq!(
			mask_api_key("sk-v1-abcdefghijklmnopqrstuvwxyz"),
			"sk-v1**********************vwxyz"
		);
	}

	#[test]
	fn test_update_provider_metadata_and_api_key() {
		let (_temp, store) = store();
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "anthropic".to_string(),
				display_name: "Anthropic".to_string(),
				format: InferenceProviderFormat::Anthropic,
				api_base_url: "https://api.anthropic.com/v1".to_string(),
				preset: None,
				api_key: "first-key".to_string(),
				models: Vec::new(),
			})
			.unwrap();

		let updated = store
			.update(
				&provider.id,
				UpdateInferenceProvider {
					latin_name: Some("claude".to_string()),
					display_name: Some("Claude Team".to_string()),
					format: Some(InferenceProviderFormat::OpenAiCompletions),
					api_base_url: Some(
						"https://gateway.example.com/v1".to_string(),
					),
					preset: None,
					api_key: Some("second-key".to_string()),
					models: None,
				},
			)
			.unwrap();

		assert_eq!(updated.latin_name, "claude");
		assert_eq!(updated.display_name, "Claude Team");
		assert_eq!(updated.format, InferenceProviderFormat::OpenAiCompletions);
		assert_eq!(updated.api_base_url, "https://gateway.example.com/v1");
		assert_eq!(updated.masked_api_key, "s********y");
		assert_eq!(
			store.get_api_key(&provider.id).unwrap(),
			Some("second-key".to_string())
		);
	}

	#[test]
	fn test_delete_provider_and_api_key() {
		let (_temp, store) = store();
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "anthropic".to_string(),
				display_name: "Anthropic".to_string(),
				format: InferenceProviderFormat::Anthropic,
				api_base_url: "https://api.anthropic.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: Vec::new(),
			})
			.unwrap();

		let deleted = store.delete(&provider.id).unwrap();

		assert_eq!(deleted.id, provider.id);
		assert!(store.list().unwrap().is_empty());
		assert_eq!(store.credentials.get_api_key(&provider.id).unwrap(), None);
	}

	/// `get_api_key` always errors (simulating a keyring read that is
	/// permanently/transiently broken); `set_api_key`/`delete_api_key`
	/// operate on a real shared map so a rollback-restore is directly
	/// observable, bypassing the broken read.
	#[derive(Debug, Clone, Default)]
	struct BrokenReadCredentialStore {
		values: Arc<Mutex<HashMap<String, String>>>,
	}

	impl CredentialStore for BrokenReadCredentialStore {
		fn get_api_key(&self, _provider_id: &str) -> Result<Option<String>> {
			Err(InferenceProviderError::KeyringUnavailable(
				"simulated transient read failure".to_string(),
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
			self.values.lock().unwrap().remove(provider_id);
			Ok(())
		}
	}

	/// Regression (GitHub #15 P1c + P2-5 redesign, Codex-found): `delete`
	/// used to accept the api key as a caller-supplied `known_api_key`
	/// parameter instead of reading it itself. That was a stale-read race —
	/// the caller's read ran OUTSIDE this method's lock, so a concurrent
	/// `set_api_key` could commit a newer key in between, and this method's
	/// rollback would clobber it with the caller's now-stale value. The fix
	/// makes `delete` read the current key itself, under its own lock, and
	/// fail HARD (not degrade to `None`) if that read errors —
	/// `BrokenReadCredentialStore::get_api_key` always errors, proving
	/// `delete` returns that error immediately without ever touching the
	/// keyring delete or the database. The provider row and its key must
	/// both survive untouched.
	#[test]
	fn test_delete_fails_hard_when_credential_read_fails() {
		let temp = tempfile::tempdir().unwrap();
		let credentials = BrokenReadCredentialStore::default();
		let store = InferenceProviderStore::with_credentials(
			temp.path(),
			credentials.clone(),
		);
		// Seed the backing map directly (bypassing `create`, whose own
		// `set_api_key` call would still succeed even though reads are
		// broken) so the provider's key is really present before `delete`.
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "acme".to_string(),
				display_name: "Acme".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
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
			Some("secret".to_string()),
			"precondition: the key is really in the backend"
		);

		let err = store.delete(&provider.id).unwrap_err();
		assert!(
			matches!(err, InferenceProviderError::KeyringUnavailable(_)),
			"a broken credential read must fail delete() HARD instead of \
			 degrading to None and proceeding, got {err:?}"
		);

		assert_eq!(
			credentials
				.values
				.lock()
				.unwrap()
				.get(&provider.id)
				.cloned(),
			Some("secret".to_string()),
			"the key must survive untouched -- delete() must never reach \
			 the keyring-delete step when its own precondition read fails"
		);
		assert!(
			store.get(&provider.id).is_ok(),
			"the provider row must survive when delete() fails hard on the \
			 credential read, before any SQL delete is attempted"
		);
	}

	/// Wraps a real credential map, but instruments every call so a test can
	/// detect two threads inside the store's credential methods AT THE SAME
	/// TIME. `enter`/`exit` bracket each method; a short sleep between them
	/// widens the race window so an unsynchronized caller reliably overlaps
	/// two threads instead of getting lucky.
	#[derive(Debug, Clone, Default)]
	struct RaceDetectingCredentialStore {
		values: Arc<Mutex<HashMap<String, String>>>,
		in_flight: Arc<std::sync::atomic::AtomicUsize>,
		max_observed: Arc<std::sync::atomic::AtomicUsize>,
	}

	impl RaceDetectingCredentialStore {
		fn enter(&self) {
			use std::sync::atomic::Ordering;
			let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
			self.max_observed.fetch_max(now, Ordering::SeqCst);
			std::thread::sleep(std::time::Duration::from_millis(5));
		}

		fn exit(&self) {
			self.in_flight
				.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
		}
	}

	impl CredentialStore for RaceDetectingCredentialStore {
		fn get_api_key(&self, provider_id: &str) -> Result<Option<String>> {
			self.enter();
			let result =
				Ok(self.values.lock().unwrap().get(provider_id).cloned());
			self.exit();
			result
		}

		fn set_api_key(&self, provider_id: &str, api_key: &str) -> Result<()> {
			self.enter();
			self.values
				.lock()
				.unwrap()
				.insert(provider_id.to_string(), api_key.to_string());
			self.exit();
			Ok(())
		}

		fn delete_api_key(&self, provider_id: &str) -> Result<()> {
			self.enter();
			self.values.lock().unwrap().remove(provider_id);
			self.exit();
			Ok(())
		}
	}

	/// Regression (GitHub #15 P2-5, Codex-found): two concurrent
	/// `set_api_key` calls for the SAME provider both used to read the old
	/// key and write a new key with no synchronization — a losing SQL write
	/// could roll back over a winning one's already-committed key. Fix:
	/// `credential_lock_for` (keyed by `app_data_dir`) serializes the whole
	/// read-modify-write sequence. Prove serialization directly: spawn many
	/// threads hammering `set_api_key` on CLONES of the same store (cloned
	/// stores sharing one `app_data_dir` must still share the lock — that's
	/// the whole point of keying by path instead of a field on the struct),
	/// instrumented so overlapping calls are detected, and assert the
	/// maximum observed concurrency inside the credential store is exactly
	/// one. Without the fix this reliably observes more than one (the 5ms
	/// sleep widens the window); with the fix it can never exceed one.
	#[test]
	fn concurrent_set_api_key_calls_are_serialized() {
		let temp = tempfile::tempdir().unwrap();
		let credentials = RaceDetectingCredentialStore::default();
		let store = InferenceProviderStore::with_credentials(
			temp.path(),
			credentials.clone(),
		);
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "acme".to_string(),
				display_name: "Acme".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "initial".to_string(),
				models: Vec::new(),
			})
			.unwrap();

		let handles: Vec<_> = (0..8)
			.map(|i| {
				let store = store.clone();
				let provider_id = provider.id.clone();
				std::thread::spawn(move || {
					store
						.set_api_key(&provider_id, &format!("key-{i}"))
						.unwrap();
				})
			})
			.collect();
		for handle in handles {
			handle.join().unwrap();
		}

		assert_eq!(
			credentials
				.max_observed
				.load(std::sync::atomic::Ordering::SeqCst),
			1,
			"concurrent set_api_key calls for the same provider must never \
			 overlap inside the credential store — a value > 1 means two \
			 read-modify-write sequences interleaved, the exact race that \
			 lets one rollback stomp another's committed key"
		);
	}

	#[test]
	fn test_set_and_delete_api_key_updates_masked_preview() {
		let (_temp, store) = store();
		let provider = create_provider(&store, "openai");

		store.set_api_key(&provider.id, "replacement-key").unwrap();
		assert_eq!(
			store.get(&provider.id).unwrap().masked_api_key,
			"re***********ey"
		);

		store.delete_api_key(&provider.id).unwrap();
		assert_eq!(store.get(&provider.id).unwrap().masked_api_key, "");
	}

	#[test]
	fn test_duplicate_name_is_rejected() {
		let (_temp, store) = store();
		store
			.create(CreateInferenceProvider {
				latin_name: "openai".to_string(),
				display_name: "OpenAI".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "first".to_string(),
				models: Vec::new(),
			})
			.unwrap();

		let error = store
			.create(CreateInferenceProvider {
				latin_name: "openai".to_string(),
				display_name: "OpenAI Gateway".to_string(),
				format: InferenceProviderFormat::OpenAiCompletions,
				api_base_url: "https://gateway.example.com/v1".to_string(),
				preset: None,
				api_key: "second".to_string(),
				models: Vec::new(),
			})
			.unwrap_err();

		assert!(matches!(error, InferenceProviderError::AlreadyExists(_)));
	}

	#[test]
	fn test_invalid_latin_name_is_rejected() {
		let (_temp, store) = store();

		for latin_name in ["OpenAI", "openai1", "open_ai", "open-ai", "兔子"]
		{
			let error = store
				.create(CreateInferenceProvider {
					latin_name: latin_name.to_string(),
					display_name: "OpenAI".to_string(),
					format: InferenceProviderFormat::OpenAiResponses,
					api_base_url: "https://api.openai.com/v1".to_string(),
					preset: None,
					api_key: "secret".to_string(),
					models: Vec::new(),
				})
				.unwrap_err();

			assert!(matches!(
				error,
				InferenceProviderError::InvalidLatinName(_)
			));
		}
	}

	#[test]
	fn test_empty_api_base_url_is_rejected() {
		let (_temp, store) = store();

		let error = store
			.create(CreateInferenceProvider {
				latin_name: "openai".to_string(),
				display_name: "OpenAI".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: " ".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: Vec::new(),
			})
			.unwrap_err();

		assert!(matches!(error, InferenceProviderError::EmptyApiBaseUrl));
	}

	#[test]
	fn test_create_and_list_provider_models() {
		let (_temp, store) = store();
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "openai".to_string(),
				display_name: "OpenAI".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: vec![
					" gpt-5.4 ".to_string(),
					"gpt-5.4-mini".to_string(),
				],
			})
			.unwrap();

		assert_eq!(
			provider.models,
			vec!["gpt-5.4".to_string(), "gpt-5.4-mini".to_string()]
		);
		assert_eq!(store.get(&provider.id).unwrap().models, provider.models);
		assert_eq!(store.list().unwrap()[0].models, provider.models);
	}

	#[test]
	fn test_provider_model_names_are_unique_case_insensitive() {
		let (_temp, store) = store();

		let error = store
			.create(CreateInferenceProvider {
				latin_name: "openai".to_string(),
				display_name: "OpenAI".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: vec!["gpt-5.4".to_string(), "GPT-5.4".to_string()],
			})
			.unwrap_err();

		assert!(matches!(
			error,
			InferenceProviderError::ModelAlreadyExists(_)
		));
	}

	#[test]
	fn test_update_and_delete_provider_model() {
		let (_temp, store) = store();
		let provider = create_provider(&store, "openai");

		let updated = store
			.update(
				&provider.id,
				UpdateInferenceProvider {
					latin_name: None,
					display_name: None,
					format: None,
					api_base_url: None,
					preset: None,
					api_key: None,
					models: Some(vec!["gpt-5.5".to_string()]),
				},
			)
			.unwrap();

		assert_eq!(updated.models, vec!["gpt-5.5".to_string()]);
		assert_eq!(store.get(&provider.id).unwrap().models, updated.models);

		let updated = store
			.update(
				&provider.id,
				UpdateInferenceProvider {
					latin_name: None,
					display_name: None,
					format: None,
					api_base_url: None,
					preset: None,
					api_key: None,
					models: Some(Vec::new()),
				},
			)
			.unwrap();

		assert!(updated.models.is_empty());
		assert!(store.get(&provider.id).unwrap().models.is_empty());
	}

	#[test]
	fn test_delete_provider_cascades_models() {
		let (_temp, store) = store();
		let provider = store
			.create(CreateInferenceProvider {
				latin_name: "openai".to_string(),
				display_name: "OpenAI".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: vec!["gpt-5.4".to_string()],
			})
			.unwrap();

		store.delete(&provider.id).unwrap();

		store.block_on(async {
			let mut conn = store.open_db().await.unwrap();
			let count: i64 =
				sqlx::query_scalar("SELECT COUNT(*) FROM inference_models")
					.fetch_one(&mut conn)
					.await
					.unwrap();
			assert_eq!(count, 0);
		});
	}

	#[test]
	fn test_empty_model_name_is_rejected() {
		let (_temp, store) = store();

		let error = store
			.create(CreateInferenceProvider {
				latin_name: "openai".to_string(),
				display_name: "OpenAI".to_string(),
				format: InferenceProviderFormat::OpenAiResponses,
				api_base_url: "https://api.openai.com/v1".to_string(),
				preset: None,
				api_key: "secret".to_string(),
				models: vec![" ".to_string()],
			})
			.unwrap_err();

		assert!(matches!(error, InferenceProviderError::EmptyModelName));
	}

	// ── Finding #3: rollback-failure observability ──────────────────────────
	//
	// The store's best-effort rollback steps used to be `let _ = ...`, hiding a
	// keyring rollback failure behind the primary DB error. They now route
	// through `log_rollback_failure` → `rollback_failure_message`, which surfaces
	// the dropped failure at WARN. The message builder is pure, so we assert its
	// contract directly (op + id + cause on Err; nothing on Ok) without a logger.

	#[test]
	fn rollback_failure_message_surfaces_op_id_and_cause() {
		let message = rollback_failure_message(
			"restore_api_key",
			"prov-123",
			&Err(InferenceProviderError::Keyring("boom".to_string())),
		)
		.expect("a failed rollback must produce a message");
		assert!(
			message.contains("restore_api_key"),
			"names the op: {message}"
		);
		assert!(message.contains("prov-123"), "names the id: {message}");
		assert!(message.contains("boom"), "carries the cause: {message}");
	}

	#[test]
	fn rollback_ok_produces_no_message() {
		assert!(
			rollback_failure_message("restore_api_key", "prov-456", &Ok(()))
				.is_none(),
			"a successful rollback must be silent"
		);
	}
}
