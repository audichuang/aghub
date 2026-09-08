//! Unified Plugin Registry implementation
//!
//! Merges data from three layers:
//! - L1: Local installed plugins
//! - L2: Marketplace definitions (marketplace.json)
//! - L3: Remote install statistics

use super::{
	marketplace::{
		known_marketplace_roots, load_marketplace, scan_marketplaces,
	},
	DiscoveryConfig, InstallCountsCache, MarketplaceSource, PluginAuthor,
	PluginCatalogCache, PluginInfo, PluginSource,
};
use crate::claude::types::PluginManifest as ClaudePluginManifest;
use crate::claude::ClaudePluginManager;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct UnifiedPluginRegistry {
	plugins: HashMap<String, PluginInfo>,
	install_counts: HashMap<String, u64>,
	config: DiscoveryConfig,
}

impl UnifiedPluginRegistry {
	pub async fn new_async(config: &DiscoveryConfig) -> Result<Self> {
		let mut registry = Self {
			plugins: HashMap::new(),
			install_counts: HashMap::new(),
			config: config.clone(),
		};

		registry.load_install_counts().await?;
		registry.scan_marketplaces().await?;
		registry.scan_local_installs().await?;

		log::info!(
			"UnifiedPluginRegistry initialized with {} plugins",
			registry.plugins.len()
		);

		Ok(registry)
	}

	pub fn all_plugins(&self) -> Vec<&PluginInfo> {
		self.plugins.values().collect()
	}

	pub fn get_plugin(&self, id: &str) -> Option<&PluginInfo> {
		self.plugins.get(id)
	}

	async fn load_install_counts(&mut self) -> Result<()> {
		// Prefer CC's richer `plugin-catalog-cache.json` whose keys are already
		// `name@marketplace`, matching our lookup id directly.
		let catalog_path = self.config.plugin_catalog_path();
		if catalog_path.exists() {
			let content = tokio::fs::read_to_string(&catalog_path).await?;
			if let Ok(cache) =
				serde_json::from_str::<PluginCatalogCache>(&content)
			{
				for (plugin_id, entry) in cache.catalog.plugins {
					if let Some(installs) = entry.unique_installs {
						self.install_counts.insert(plugin_id, installs);
					}
				}
			}
		}

		// Legacy fallback: older CC versions wrote a flat
		// `install-counts-cache.json`. Skipped if the file is absent.
		let legacy_path = self.config.install_counts_path();
		if legacy_path.exists() {
			let content = tokio::fs::read_to_string(&legacy_path).await?;
			if let Ok(cache) =
				serde_json::from_str::<InstallCountsCache>(&content)
			{
				for entry in cache.counts {
					self.install_counts
						.entry(entry.plugin)
						.or_insert(entry.unique_installs);
				}
			}
		}
		Ok(())
	}
}

// ── Local ──

impl UnifiedPluginRegistry {
	async fn enrich_from_local_manifest(
		plugin_dir: &Path,
		info: &mut PluginInfo,
	) {
		let Some(manifest_path) =
			crate::installer::registry::find_plugin_manifest_path(plugin_dir)
		else {
			return;
		};
		let Ok(content) = tokio::fs::read_to_string(manifest_path).await else {
			return;
		};
		let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
		else {
			return;
		};
		let manifest =
			serde_json::from_value::<ClaudePluginManifest>(json.clone())
				.inspect_err(|e| {
					log::debug!("Failed to parse plugin manifest: {e}")
				})
				.ok();

		if let Some(m) = &manifest {
			if info.version.is_none() {
				info.version = m.version.clone();
			}
			if info.author.is_none() && !m.author.is_empty() {
				info.author = Some(PluginAuthor {
					name: m.author.name.clone(),
					email: m.author.email.clone(),
				});
			}
			if info.homepage.is_none() {
				info.homepage = m.homepage.clone();
			}
			if info.repository.is_none() {
				info.repository = m.repository.clone();
			}
			if info.keywords.is_empty() {
				info.keywords = m.keywords.clone().unwrap_or_default();
			}
		}

		info.has_mcp = json.get("mcpServers").is_some()
			|| json.get("mcp_servers").is_some();
		info.has_skills =
			json.get("skills").is_some() && !json["skills"].is_null();

		let has_hooks_dir = tokio::fs::try_exists(plugin_dir.join("hooks"))
			.await
			.unwrap_or(false)
			|| tokio::fs::try_exists(plugin_dir.join("hooks.json"))
				.await
				.unwrap_or(false);
		info.has_hooks = json.get("hooks").is_some() || has_hooks_dir;
	}

	pub(super) async fn scan_local_installs(&mut self) -> Result<()> {
		match ClaudePluginManager::new().await {
			Ok(manager) => {
				let installed = manager.list_plugins();
				log::debug!("Found {} installed plugins", installed.len());

				for plugin in installed {
					let plugin_id = plugin.id.to_string();
					let name = plugin.id.name.clone();
					let marketplace = plugin.id.source.clone();

					if let Some(existing) = self.plugins.get_mut(&plugin_id) {
						existing.installed = true;
						existing.enabled = Some(plugin.enabled);
						existing.local_path = Some(plugin.install_path.clone());
						existing.has_mcp = existing.has_mcp || plugin.has_mcp();
						existing.has_skills =
							existing.has_skills || plugin.has_skills();
						existing.has_hooks =
							existing.has_hooks || plugin.has_hooks();
						if existing.version.is_none()
							&& !plugin.version.is_empty()
						{
							existing.version = Some(plugin.version.clone());
						}
					} else {
						let install_count =
							self.install_counts.get(&plugin_id).copied();

						let plugin_info = PluginInfo {
							id: plugin_id.clone(),
							name: name.clone(),
							description: String::new(),
							version: if plugin.version.is_empty() {
								None
							} else {
								Some(plugin.version.clone())
							},
							author: None,
							category: None,
							source: PluginSource::LocalRelative {
								path: plugin
									.install_path
									.to_string_lossy()
									.to_string(),
							},
							marketplace,
							local_path: Some(plugin.install_path.clone()),
							installed: true,
							enabled: Some(plugin.enabled),
							install_count,
							homepage: None,
							repository: None,
							keywords: Vec::new(),
							git_sha: if plugin.commit_hash.is_empty() {
								None
							} else {
								Some(plugin.commit_hash.clone())
							},
							has_mcp: plugin.has_mcp(),
							has_skills: plugin.has_skills(),
							has_hooks: plugin.has_hooks(),
						};

						self.plugins.insert(plugin_id, plugin_info);
					}
				}
			}
			Err(error) => {
				log::warn!("Failed to load installed plugins: {}", error);
			}
		}

		Ok(())
	}
}

// ── Marketplaces ──

impl UnifiedPluginRegistry {
	/// Every marketplace root to read, in priority order: the ones the `claude`
	/// CLI recorded (which is where an external `directory` source actually
	/// lives) first, then whatever sits under `marketplaces/` — the second half
	/// keeps a hand-copied marketplace working even with no registry entry.
	async fn marketplace_roots(&self) -> Vec<PathBuf> {
		let mut roots = known_marketplace_roots(&self.config.plugins_dir).await;

		let marketplaces_dir = self
			.config
			.plugins_dir
			.join(&self.config.marketplaces_subdir);

		if marketplaces_dir.exists() {
			match scan_marketplaces(&marketplaces_dir).await {
				Ok(scanned) => roots.extend(scanned),
				Err(e) => log::warn!(
					"Cannot scan {}: {}",
					marketplaces_dir.display(),
					e
				),
			}
		} else {
			log::warn!(
				"Marketplaces directory not found: {}",
				marketplaces_dir.display()
			);
		}

		// Same root reachable two ways (a registry entry plus a copy under the
		// scanned dir) must not be read twice. Compare resolved paths so a
		// symlinked or `..`-spelled root counts once.
		let mut seen = HashSet::new();
		let mut unique = Vec::with_capacity(roots.len());
		for root in roots {
			let key = tokio::fs::canonicalize(&root)
				.await
				.unwrap_or_else(|_| root.clone());
			if seen.insert(key) {
				unique.push(root);
			}
		}

		unique
	}

	pub(super) async fn scan_marketplaces(&mut self) -> Result<()> {
		let mut seen_names = HashSet::new();

		for root in self.marketplace_roots().await {
			let Some(config) = load_marketplace(&root).await else {
				continue;
			};
			let marketplace_name = config.name.clone();
			if !seen_names.insert(marketplace_name.clone()) {
				continue;
			}
			log::debug!(
				"Scanning marketplace '{}' with {} plugins",
				marketplace_name,
				config.plugins.len()
			);
			let owner_name = config.owner.name.clone();
			let owner_email = config.owner.email.clone();

			for plugin_def in config.plugins {
				let plugin_id =
					format!("{}@{}", plugin_def.name, marketplace_name);
				let source = PluginSource::from_marketplace(&plugin_def.source);
				let git_sha = match &plugin_def.source {
					MarketplaceSource::GitHub { sha, .. }
					| MarketplaceSource::Url { sha, .. }
					| MarketplaceSource::GitSubdir { sha, .. } => sha.clone(),
					_ => None,
				};
				let install_count =
					self.install_counts.get(&plugin_id).copied();
				let author = plugin_def
					.author
					.map(|author| PluginAuthor {
						name: author.name,
						email: author.email,
					})
					.or_else(|| {
						Some(PluginAuthor {
							name: owner_name.clone(),
							email: owner_email.clone(),
						})
					});

				let plugin_info = PluginInfo {
					id: plugin_id.clone(),
					name: plugin_def.name.clone(),
					description: plugin_def.description,
					version: plugin_def.version,
					author,
					category: plugin_def.category.clone(),
					source,
					marketplace: marketplace_name.clone(),
					local_path: None,
					installed: false,
					enabled: None,
					install_count,
					homepage: plugin_def.homepage.clone(),
					repository: None,
					keywords: Vec::new(),
					git_sha,
					has_mcp: false,
					has_skills: false,
					has_hooks: false,
				};

				let local_path = match &plugin_def.source {
					MarketplaceSource::Local(path) => {
						Some(root.join(path.trim_start_matches("./")))
					}
					_ => None,
				};
				if let Some(local_path) = local_path {
					if tokio::fs::try_exists(&local_path).await.unwrap_or(false)
					{
						let mut info = plugin_info;
						info.local_path = Some(local_path.clone());
						Self::enrich_from_local_manifest(
							&local_path,
							&mut info,
						)
						.await;
						self.plugins.insert(plugin_id, info);
					} else {
						self.plugins.insert(plugin_id, plugin_info);
					}
				} else {
					self.plugins.insert(plugin_id, plugin_info);
				}
			}
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::tempdir;

	fn write(path: &Path, body: &str) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, body).unwrap();
	}

	fn registry(plugins_dir: PathBuf) -> UnifiedPluginRegistry {
		UnifiedPluginRegistry {
			plugins: HashMap::new(),
			install_counts: HashMap::new(),
			config: DiscoveryConfig {
				plugins_dir,
				marketplaces_subdir: "marketplaces".to_string(),
				known_marketplaces: Vec::new(),
			},
		}
	}

	const MANIFEST: &str = r#"{"name":"ui-test-market","owner":{"name":"UI"},
        "plugins":[{"name":"ui-fixture","source":"./plugins/ui-fixture"}]}"#;

	/// The regression: adding an EXTERNAL local marketplace succeeded on the
	/// sources page while the plugin catalog stayed empty, because discovery
	/// only ever walked `<plugins_dir>/marketplaces`. Nothing is copied there
	/// for a `directory` source, so the registry has to read the roots the
	/// `claude` CLI recorded in `known_marketplaces.json`.
	#[tokio::test]
	async fn a_marketplace_outside_the_scanned_dir_is_discovered() {
		let temp_dir = tempdir().unwrap();
		let plugins_dir = temp_dir.path().join(".claude/plugins");
		let external = temp_dir.path().join("elsewhere/plugin-market");
		write(&external.join(".claude-plugin/marketplace.json"), MANIFEST);
		write(&external.join("plugins/ui-fixture/.keep"), "");
		write(
			&plugins_dir.join("known_marketplaces.json"),
			&format!(
				r#"{{"ui-test-market":{{"installLocation":"{}"}}}}"#,
				external.display()
			),
		);

		let mut registry = registry(plugins_dir);
		registry.scan_marketplaces().await.unwrap();

		let plugin = registry
			.get_plugin("ui-fixture@ui-test-market")
			.expect("external marketplace plugin should be in the catalog");
		// A plugin's own directory resolves against ITS marketplace root, not
		// against the scanned dir.
		assert_eq!(
			plugin.local_path.as_deref(),
			Some(external.join("plugins/ui-fixture").as_path())
		);
	}

	/// A marketplace sitting under the scanned dir with no registry entry keeps
	/// working, and a root reachable both ways is read once.
	#[tokio::test]
	async fn the_scanned_dir_still_works_and_overlap_is_deduped() {
		let temp_dir = tempdir().unwrap();
		let plugins_dir = temp_dir.path().join(".claude/plugins");
		let inside = plugins_dir.join("marketplaces/ui-test-market");
		write(&inside.join(".claude-plugin/marketplace.json"), MANIFEST);

		let mut only_scanned = registry(plugins_dir.clone());
		only_scanned.scan_marketplaces().await.unwrap();
		assert_eq!(only_scanned.all_plugins().len(), 1);

		// Register that very same directory as well.
		write(
			&plugins_dir.join("known_marketplaces.json"),
			&format!(
				r#"{{"ui-test-market":{{"installLocation":"{}"}}}}"#,
				inside.display()
			),
		);
		let mut both_ways = registry(plugins_dir);
		both_ways.scan_marketplaces().await.unwrap();
		assert_eq!(both_ways.all_plugins().len(), 1);
	}

	#[tokio::test]
	async fn enrich_from_local_manifest_reads_manifest_fields() {
		let temp_dir = tempdir().unwrap();
		let plugin_dir = temp_dir.path().join("superpowers");
		fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
		fs::write(
			plugin_dir.join(".claude-plugin/plugin.json"),
			r#"{
				"name":"superpowers",
				"version":"5.0.7",
				"description":"test plugin",
				"author":{"name":"Anthropic","email":"team@example.com"},
				"homepage":"https://example.com",
				"repository":"https://github.com/example/superpowers",
				"keywords":["testing","debugging"],
				"skills":"skills",
				"mcpServers":{"docs":{"command":"node"}}
			}"#,
		)
		.unwrap();

		let mut info = PluginInfo {
			id: "superpowers@test".to_string(),
			name: "superpowers".to_string(),
			description: String::new(),
			version: None,
			author: None,
			category: None,
			source: PluginSource::LocalRelative {
				path: "./superpowers".to_string(),
			},
			marketplace: "test".to_string(),
			local_path: None,
			installed: false,
			enabled: None,
			install_count: None,
			homepage: None,
			repository: None,
			keywords: Vec::new(),
			git_sha: None,
			has_mcp: false,
			has_skills: false,
			has_hooks: false,
		};

		UnifiedPluginRegistry::enrich_from_local_manifest(
			&plugin_dir,
			&mut info,
		)
		.await;

		assert_eq!(info.version.as_deref(), Some("5.0.7"));
		assert_eq!(
			info.author.as_ref().map(|author| author.name.as_str()),
			Some("Anthropic")
		);
		assert_eq!(info.homepage.as_deref(), Some("https://example.com"));
		assert_eq!(
			info.repository.as_deref(),
			Some("https://github.com/example/superpowers")
		);
		assert_eq!(info.keywords, vec!["testing", "debugging"]);
		assert!(info.has_skills);
		assert!(info.has_mcp);
		assert!(!info.has_hooks);
	}
}
