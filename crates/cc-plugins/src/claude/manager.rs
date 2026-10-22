use super::manifest::read_manifest;
use super::{settings, ClaudePluginInfo, PluginScopeInfo};
use crate::cli::types::CliInstalledPlugin;
use crate::cli::ClaudeCli;
use crate::{PluginId, PluginSource};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

pub struct ClaudePluginManager {
	pub(super) settings: settings::ClaudeSettings,
	pub(super) installed: Vec<ClaudePluginInfo>,
}

impl ClaudePluginManager {
	pub async fn new() -> Result<Self> {
		let settings_path = dirs::home_dir()
			.context("Cannot find home directory")?
			.join(".claude/settings.json");
		let settings =
			settings::ClaudeSettings::load_from_path(&settings_path)?;
		let installed = load_via_cli().await?;
		Ok(Self {
			settings,
			installed,
		})
	}

	pub fn get_plugin(&self, id: &PluginId) -> Option<&ClaudePluginInfo> {
		self.installed.iter().find(|p| p.id == *id)
	}

	pub fn list_plugins(&self) -> &[ClaudePluginInfo] {
		&self.installed
	}

	pub fn plugin_owning_path(&self, path: &Path) -> Option<&ClaudePluginInfo> {
		self.installed.iter().find(|plugin| plugin.owns_path(path))
	}

	/// `(plugin name, install root)` for every installed plugin — the ownership
	/// list core's skill mutation entries take (same roots `owns_path` checks).
	/// Fails open: when the plugin list cannot be read (no `claude` CLI, bad
	/// settings) it returns an empty list, which refuses nothing.
	///
	/// Cached process-wide (see `RootsCache`): the long-lived API otherwise
	/// spawns `claude plugin list` (~0.5s) on every mutation.
	pub async fn owned_roots() -> Vec<(String, PathBuf)> {
		static CACHE: Mutex<RootsCache> = Mutex::new(RootsCache::new());
		cached_roots(
			&CACHE,
			registry_key(&registry_path()),
			Instant::now(),
			|| async {
				let manager = Self::new().await.ok()?;
				Some(
					manager
						.installed
						.iter()
						.flat_map(|plugin| {
							plugin
								.all_install_paths()
								.into_iter()
								.map(|root| (plugin.display_name.clone(), root))
						})
						.collect(),
				)
			},
		)
		.await
	}

	/// Blocking `owned_roots` for core's injected provider. Runs on its own
	/// thread with its own runtime, so it is safe from any caller, including
	/// code already inside a tokio runtime. Fails open.
	pub fn owned_roots_blocking() -> Vec<(String, PathBuf)> {
		std::thread::spawn(|| {
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.map(|runtime| runtime.block_on(Self::owned_roots()))
				.unwrap_or_default()
		})
		.join()
		.unwrap_or_default()
	}

	pub fn is_enabled(&self, id: &PluginId) -> bool {
		self.installed
			.iter()
			.find(|p| p.id == *id)
			.map(|p| p.enabled)
			.unwrap_or(false)
	}

	pub async fn enable(
		&mut self,
		id: &PluginId,
		scope: Option<settings::InstallScope>,
	) -> Result<()> {
		let cli = ClaudeCli::new()?;
		cli.plugin_enable(id, scope).await?;
		self.refresh_after_toggle().await
	}

	pub async fn disable(
		&mut self,
		id: &PluginId,
		scope: Option<settings::InstallScope>,
	) -> Result<()> {
		let cli = ClaudeCli::new()?;
		cli.plugin_disable(id, scope).await?;
		self.refresh_after_toggle().await
	}

	async fn refresh_after_toggle(&mut self) -> Result<()> {
		self.installed = load_via_cli().await?;
		Ok(())
	}

	pub fn get_plugin_config(
		&self,
		id: &PluginId,
	) -> Option<&serde_json::Value> {
		self.settings.get_plugin_config(id)
	}

	pub fn set_plugin_config(
		&mut self,
		id: &PluginId,
		config: serde_json::Value,
	) -> Result<()> {
		settings::ClaudeSettings::update(|settings| {
			settings.set_plugin_config(id, config.clone());
		})?;
		self.settings.set_plugin_config(id, config);
		Ok(())
	}

	pub fn remove_plugin_config(&mut self, id: &PluginId) -> Result<()> {
		settings::ClaudeSettings::update(|settings| {
			settings.remove_plugin_config(id);
		})?;
		self.settings.remove_plugin_config(id);
		Ok(())
	}
}

type Roots = Vec<(String, PathBuf)>;
/// Registry path plus its `(mtime, len)` (`None` = file missing). The path is
/// part of the key so a different `CLAUDE_CONFIG_DIR`/home never reuses roots.
type RegistryKey = (PathBuf, Option<(SystemTime, u64)>);

/// Backstop expiry for cached roots. `claude plugin list` also merges sources
/// that never touch `installed_plugins.json` (`~/.claude/plugins/synced/`, and
/// `~/.claude/skills/<dir>` carrying `.claude-plugin/`); a change there is only
/// picked up once this TTL elapses.
const ROOTS_TTL: Duration = Duration::from_secs(10);

/// Decision state for the owned-roots cache. Pure (clock passed in) so it is
/// testable without spawning `claude`.
struct RootsCache {
	entry: Option<(RegistryKey, Instant, Roots)>,
}

impl RootsCache {
	const fn new() -> Self {
		Self { entry: None }
	}

	fn get(&self, key: &RegistryKey, now: Instant) -> Option<Roots> {
		let (k, at, roots) = self.entry.as_ref()?;
		(k == key && now.saturating_duration_since(*at) < ROOTS_TTL)
			.then(|| roots.clone())
	}

	fn put(&mut self, key: RegistryKey, now: Instant, roots: Roots) {
		self.entry = Some((key, now, roots));
	}
}

fn registry_path() -> PathBuf {
	let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
		.filter(|v| !v.is_empty())
		.map(PathBuf::from)
		.or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
		.unwrap_or_default();
	dir.join("plugins/installed_plugins.json")
}

fn registry_key(path: &Path) -> RegistryKey {
	let stat = std::fs::metadata(path)
		.ok()
		.and_then(|m| Some((m.modified().ok()?, m.len())));
	(path.to_path_buf(), stat)
}

/// Return the cached roots for `key`, else run `load` and store its result
/// under that same pre-load `key` (a write landing mid-load then misses on the
/// next call). A failed load (`None`) is not stored and yields an empty list
/// (fail-open). The lock is never held across `.await`; concurrent misses may
/// both load.
async fn cached_roots<F, Fut>(
	cache: &Mutex<RootsCache>,
	key: RegistryKey,
	now: Instant,
	load: F,
) -> Roots
where
	F: FnOnce() -> Fut,
	Fut: Future<Output = Option<Roots>>,
{
	if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key, now)) {
		return hit;
	}
	match load().await {
		Some(roots) => {
			if let Ok(mut c) = cache.lock() {
				c.put(key, now, roots.clone());
			}
			roots
		}
		None => Vec::new(),
	}
}

async fn load_via_cli() -> Result<Vec<ClaudePluginInfo>> {
	let cli = ClaudeCli::new()?;
	let cli_plugins = cli.plugin_list().await?;

	let mut by_id: HashMap<String, Vec<CliInstalledPlugin>> = HashMap::new();
	for plugin in cli_plugins {
		by_id.entry(plugin.id.clone()).or_default().push(plugin);
	}

	let mut plugins = Vec::with_capacity(by_id.len());
	for (id_str, scope_entries) in by_id {
		if let Some(plugin) = build_plugin_info(id_str, scope_entries) {
			plugins.push(plugin);
		}
	}
	Ok(plugins)
}

fn build_plugin_info(
	id_str: String,
	scope_entries: Vec<CliInstalledPlugin>,
) -> Option<ClaudePluginInfo> {
	let id = PluginId::parse(&id_str).ok()?;
	let source = PluginSource::parse(&id.source)
		.unwrap_or(PluginSource::OfficialRegistry);
	let display_name = id.name.clone();

	let primary = &scope_entries[0];
	let install_path = PathBuf::from(&primary.install_path);
	let manifest = read_manifest(&install_path).ok().flatten();

	let scopes = scope_entries
		.iter()
		.map(|entry| PluginScopeInfo {
			scope: cli_scope_to_aghub(&entry.scope),
			install_path: PathBuf::from(&entry.install_path),
			version: entry.version.clone(),
			installed_at: entry.installed_at.clone(),
			last_updated: entry.last_updated.clone(),
			git_commit_sha: None,
		})
		.collect();

	let enabled = scope_entries.iter().any(|entry| entry.enabled);
	Some(ClaudePluginInfo {
		id,
		display_name,
		version: primary.version.clone(),
		description: manifest.as_ref().map(|m| m.description.clone()),
		author: manifest
			.as_ref()
			.and_then(|m| (!m.author.is_empty()).then(|| m.author.clone())),
		repository: manifest.as_ref().and_then(|m| m.repository.clone()),
		license: manifest.as_ref().and_then(|m| m.license.clone()),
		keywords: manifest.as_ref().and_then(|m| m.keywords.clone()),
		source,
		install_path,
		enabled,
		commit_hash: String::new(),
		scopes,
	})
}

fn cli_scope_to_aghub(cli_scope: &str) -> String {
	match cli_scope {
		"user" => "global".to_string(),
		// project, local, managed map through as-is
		other => other.to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::cli::types::CliInstalledPlugin;

	fn make_cli_entry(id: &str, enabled: bool) -> CliInstalledPlugin {
		CliInstalledPlugin {
			id: id.to_string(),
			version: "1.0.0".to_string(),
			scope: "user".to_string(),
			enabled,
			install_path: "/tmp/x".to_string(),
			installed_at: "2026-01-01T00:00:00.000Z".to_string(),
			last_updated: "2026-01-02T00:00:00.000Z".to_string(),
		}
	}

	#[test]
	fn build_plugin_info_carries_cli_enabled_through() {
		let info = build_plugin_info(
			"foo@bar".to_string(),
			vec![make_cli_entry("foo@bar", true)],
		)
		.expect("plugin info");
		assert!(info.enabled);

		let info = build_plugin_info(
			"foo@bar".to_string(),
			vec![make_cli_entry("foo@bar", false)],
		)
		.expect("plugin info");
		assert!(!info.enabled);
	}

	#[test]
	fn build_plugin_info_aggregates_multiple_scopes_enabled() {
		let info = build_plugin_info(
			"foo@bar".to_string(),
			vec![
				make_cli_entry("foo@bar", false),
				make_cli_entry("foo@bar", true),
			],
		)
		.expect("plugin info");
		assert!(info.enabled);

		let info = build_plugin_info(
			"foo@bar".to_string(),
			vec![
				make_cli_entry("foo@bar", false),
				make_cli_entry("foo@bar", false),
			],
		)
		.expect("plugin info");
		assert!(!info.enabled);
	}
}

#[cfg(test)]
mod roots_cache_tests {
	use super::*;
	use std::cell::Cell;

	fn roots(n: &str) -> Roots {
		vec![(n.to_string(), PathBuf::from(format!("/p/{n}")))]
	}

	fn nokey() -> RegistryKey {
		(PathBuf::from("/none"), None)
	}

	fn run(
		cache: &Mutex<RootsCache>,
		key: RegistryKey,
		now: Instant,
		calls: &Cell<u32>,
		result: Option<Roots>,
	) -> Roots {
		tokio::runtime::Builder::new_current_thread()
			.build()
			.unwrap()
			.block_on(cached_roots(cache, key, now, || async {
				calls.set(calls.get() + 1);
				result
			}))
	}

	#[test]
	fn same_key_within_ttl_returns_cached() {
		let cache = Mutex::new(RootsCache::new());
		let calls = Cell::new(0);
		let t = Instant::now();
		assert_eq!(
			run(&cache, nokey(), t, &calls, Some(roots("a"))),
			roots("a")
		);
		let again = run(&cache, nokey(), t, &calls, Some(roots("b")));
		assert_eq!(again, roots("a"));
		assert_eq!(calls.get(), 1);
	}

	#[test]
	fn changed_registry_file_reloads_within_ttl() {
		let dir = tempfile::tempdir().unwrap();
		let file = dir.path().join("installed_plugins.json");
		std::fs::write(&file, "{}").unwrap();
		let cache = Mutex::new(RootsCache::new());
		let calls = Cell::new(0);
		let t = Instant::now();
		let k1 = registry_key(&file);
		assert!(k1.1.is_some());
		assert_eq!(
			run(&cache, k1.clone(), t, &calls, Some(roots("a"))),
			roots("a")
		);
		std::fs::write(&file, r#"{"version":2}"#).unwrap();
		let k2 = registry_key(&file);
		assert_ne!(k1, k2);
		assert_eq!(run(&cache, k2, t, &calls, Some(roots("b"))), roots("b"));
		assert_eq!(calls.get(), 2);
	}

	#[test]
	fn ttl_elapsed_reloads() {
		let cache = Mutex::new(RootsCache::new());
		let calls = Cell::new(0);
		let t = Instant::now();
		run(&cache, nokey(), t, &calls, Some(roots("a")));
		let later = t + ROOTS_TTL;
		assert_eq!(
			run(&cache, nokey(), later, &calls, Some(roots("b"))),
			roots("b")
		);
		assert_eq!(calls.get(), 2);
	}

	#[test]
	fn failed_load_is_not_cached() {
		let cache = Mutex::new(RootsCache::new());
		let calls = Cell::new(0);
		let t = Instant::now();
		assert!(run(&cache, nokey(), t, &calls, None).is_empty());
		assert_eq!(
			run(&cache, nokey(), t, &calls, Some(roots("a"))),
			roots("a")
		);
		assert_eq!(calls.get(), 2);
	}
}
