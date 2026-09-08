//! Marketplace scanner for reading marketplace.json definitions

use anyhow::Result;
use serde::{Deserialize, Deserializer};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

// ── Types ──

#[derive(Debug, Deserialize)]
pub struct MarketplaceConfig {
	pub name: String,
	pub owner: MarketplaceOwner,
	pub plugins: Vec<MarketplacePlugin>,
}

#[derive(Debug, Deserialize)]
pub struct MarketplaceOwner {
	pub name: String,
	pub email: Option<String>,
}

/// Plugin definition in marketplace.json
#[derive(Debug, Deserialize)]
pub struct MarketplacePlugin {
	pub name: String,
	#[serde(default)]
	pub description: String,
	#[serde(default)]
	pub version: Option<String>,
	pub author: Option<MarketplaceAuthor>,
	pub source: MarketplaceSource,
	#[serde(default)]
	pub category: Option<String>,
	#[serde(default)]
	pub homepage: Option<String>,
	#[serde(default, flatten)]
	pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MarketplaceAuthor {
	pub name: String,
	pub email: Option<String>,
}

/// Source definition in marketplace.json
/// This uses a tagged enum structure based on the "source" field
#[derive(Debug, Clone)]
pub enum MarketplaceSource {
	/// Simple string path like "./plugins/xxx"
	Local(String),
	/// Object with source type
	GitHub {
		repo: String,
		git_ref: Option<String>,
		sha: Option<String>,
	},
	Url {
		url: String,
		sha: Option<String>,
	},
	GitSubdir {
		url: String,
		path: String,
		git_ref: Option<String>,
		sha: Option<String>,
	},
	Npm {
		package: String,
		version: Option<String>,
		registry: Option<String>,
	},
}

impl<'de> Deserialize<'de> for MarketplaceSource {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = serde_json::Value::deserialize(deserializer)?;

		match value {
			serde_json::Value::String(path) => Ok(Self::Local(path)),
			serde_json::Value::Object(mut map) => {
				let source: String = take_required_string(&mut map, "source")?;

				match source.as_str() {
					"github" => Ok(Self::GitHub {
						repo: take_required_string(&mut map, "repo")?,
						git_ref: take_optional_string(&mut map, "ref")?,
						sha: take_optional_string(&mut map, "sha")?,
					}),
					"url" => Ok(Self::Url {
						url: take_required_string(&mut map, "url")?,
						sha: take_optional_string(&mut map, "sha")?,
					}),
					"git-subdir" => Ok(Self::GitSubdir {
						url: take_required_string(&mut map, "url")?,
						path: take_required_string(&mut map, "path")?,
						git_ref: take_optional_string(&mut map, "ref")?,
						sha: take_optional_string(&mut map, "sha")?,
					}),
					"npm" => Ok(Self::Npm {
						package: take_required_string(&mut map, "package")?,
						version: take_optional_string(&mut map, "version")?,
						registry: take_optional_string(&mut map, "registry")?,
					}),
					other => Err(serde::de::Error::custom(format!(
						"Unsupported marketplace source type: {other}"
					))),
				}
			}
			_ => Err(serde::de::Error::custom(
				"Marketplace source must be a string path or object",
			)),
		}
	}
}

fn take_required_string<E>(
	map: &mut serde_json::Map<String, serde_json::Value>,
	key: &str,
) -> Result<String, E>
where
	E: serde::de::Error,
{
	match map.remove(key) {
		Some(serde_json::Value::String(value)) if !value.is_empty() => {
			Ok(value)
		}
		Some(_) => Err(E::custom(format!(
			"Marketplace source field '{key}' must be a string"
		))),
		None => Err(E::custom(format!(
			"Marketplace source field '{key}' is required"
		))),
	}
}

fn take_optional_string<E>(
	map: &mut serde_json::Map<String, serde_json::Value>,
	key: &str,
) -> Result<Option<String>, E>
where
	E: serde::de::Error,
{
	match map.remove(key) {
		Some(serde_json::Value::String(value)) => Ok(Some(value)),
		Some(serde_json::Value::Null) | None => Ok(None),
		Some(_) => Err(E::custom(format!(
			"Marketplace source field '{key}' must be a string"
		))),
	}
}

/// One entry of `~/.claude/plugins/known_marketplaces.json` (keyed by
/// marketplace name). Every field is optional so an unknown source kind or a
/// shape change does not cost us the whole file.
#[derive(Debug, Deserialize)]
struct KnownMarketplaceEntry {
	#[serde(rename = "installLocation")]
	install_location: Option<String>,
}

/// Marketplace name → root, as the `claude` CLI recorded it in
/// `known_marketplaces.json`. A `directory` source stays wherever the user
/// pointed at it — nothing is copied under `marketplaces/` — so any code that
/// derives a marketplace's path from that one directory misses every external
/// source. Sync (the file is small and local) so both the async discovery scan
/// and the installer's sync path lookups share ONE reader; ordered so a
/// duplicate name resolves the same way on every run.
pub fn known_marketplaces(plugins_dir: &Path) -> BTreeMap<String, PathBuf> {
	let path = plugins_dir.join("known_marketplaces.json");

	let content = match std::fs::read_to_string(&path) {
		Ok(content) => content,
		Err(e) => {
			if e.kind() != std::io::ErrorKind::NotFound {
				log::warn!("Cannot read {}: {}", path.display(), e);
			}
			return BTreeMap::new();
		}
	};

	match serde_json::from_str::<BTreeMap<String, KnownMarketplaceEntry>>(
		&content,
	) {
		Ok(entries) => entries
			.into_iter()
			.filter_map(|(name, entry)| {
				let location = entry.install_location?;
				(!location.is_empty()).then(|| (name, PathBuf::from(location)))
			})
			.collect(),
		Err(e) => {
			log::warn!("Failed to parse {}: {}", path.display(), e);
			BTreeMap::new()
		}
	}
}

/// Read the manifest of ONE marketplace root. `None` when there is no manifest
/// there or it does not parse — both are logged, neither is fatal.
pub async fn load_marketplace(root: &Path) -> Option<MarketplaceConfig> {
	let manifest_path = root.join(".claude-plugin/marketplace.json");

	match tokio::fs::try_exists(&manifest_path).await {
		Ok(true) => {}
		Ok(false) => return None,
		Err(e) => {
			log::warn!(
				"Cannot check manifest at {}: {}",
				manifest_path.display(),
				e
			);
			return None;
		}
	}

	let content = match tokio::fs::read_to_string(&manifest_path).await {
		Ok(content) => content,
		Err(e) => {
			log::warn!("Failed to read {}: {}", manifest_path.display(), e);
			return None;
		}
	};

	match serde_json::from_str::<MarketplaceConfig>(&content) {
		Ok(config) => Some(config),
		Err(e) => {
			log::warn!("Failed to parse {}: {}", manifest_path.display(), e);
			None
		}
	}
}

/// Scan all marketplaces in a directory
pub async fn scan_marketplaces(
	marketplaces_dir: &Path,
) -> Result<Vec<PathBuf>> {
	let mut results: Vec<PathBuf> = Vec::new();

	match tokio::fs::try_exists(marketplaces_dir).await {
		Ok(true) => {}
		Ok(false) => {
			log::warn!(
				"Marketplaces directory does not exist: {}",
				marketplaces_dir.display()
			);
			return Ok(results);
		}
		Err(e) => {
			log::warn!(
				"Cannot check marketplaces directory at {}: {}",
				marketplaces_dir.display(),
				e
			);
			return Ok(results);
		}
	}

	let mut entries = tokio::fs::read_dir(marketplaces_dir).await?;

	while let Some(entry) = entries.next_entry().await? {
		if !entry.file_type().await?.is_dir() {
			continue;
		}

		results.push(entry.path());
	}

	Ok(results)
}

#[cfg(test)]
mod tests {
	use super::{known_marketplaces, load_marketplace, MarketplaceConfig};
	use std::path::Path;

	fn write(path: &Path, body: &str) {
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(path, body).unwrap();
	}

	const MANIFEST: &str = r#"{"name":"ui-test-market","owner":{"name":"UI"},
        "plugins":[{"name":"ui-fixture","source":"./plugins/ui-fixture"}]}"#;

	/// A `directory` source is registered by PATH — nothing is copied under
	/// `marketplaces/` — so the roots must come off `known_marketplaces.json`
	/// or the catalog is empty for a source the sources page just listed.
	#[tokio::test]
	async fn external_directory_source_is_a_known_root() {
		let tmp = tempfile::tempdir().unwrap();
		let plugins_dir = tmp.path().join(".claude/plugins");
		let external = tmp.path().join("elsewhere/plugin-market");
		write(&external.join(".claude-plugin/marketplace.json"), MANIFEST);
		write(
			&plugins_dir.join("known_marketplaces.json"),
			&format!(
				r#"{{"ui-test-market":{{"source":{{"source":"directory","path":"{p}"}},"installLocation":"{p}"}}}}"#,
				p = external.display()
			),
		);

		let known = known_marketplaces(&plugins_dir);
		assert_eq!(known.get("ui-test-market"), Some(&external));
		assert_eq!(
			load_marketplace(&external).await.unwrap().name,
			"ui-test-market"
		);
	}

	/// Fail open: no registry file, a malformed one, or an entry with no
	/// `installLocation` yields no roots instead of an error.
	#[tokio::test]
	async fn a_missing_or_broken_registry_yields_no_roots() {
		let tmp = tempfile::tempdir().unwrap();
		let plugins_dir = tmp.path().join("plugins");
		assert!(known_marketplaces(&plugins_dir).is_empty());

		write(&plugins_dir.join("known_marketplaces.json"), "{ not json");
		assert!(known_marketplaces(&plugins_dir).is_empty());

		write(
			&plugins_dir.join("known_marketplaces.json"),
			r#"{"m":{"source":{"source":"github","repo":"o/r"}}}"#,
		);
		assert!(known_marketplaces(&plugins_dir).is_empty());
	}

	#[tokio::test]
	async fn a_root_without_a_manifest_is_not_a_marketplace() {
		let tmp = tempfile::tempdir().unwrap();
		assert!(load_marketplace(tmp.path()).await.is_none());

		write(&tmp.path().join(".claude-plugin/marketplace.json"), "{");
		assert!(load_marketplace(tmp.path()).await.is_none());
	}

	#[test]
	fn test_marketplace_config_parsing() {
		let json = r#"{
            "$schema": "https://anthropic.com/claude-code/marketplace.schema.json",
            "name": "test-marketplace",
            "description": "Test marketplace",
            "owner": { "name": "Test" },
            "plugins": [
                {
                    "name": "local-plugin",
                    "description": "A local plugin",
                    "source": "./plugins/local-plugin"
                },
                {
                    "name": "github-plugin",
                    "description": "A GitHub plugin",
                    "source": { "source": "github", "repo": "owner/repo" }
                },
                {
                    "name": "url-plugin",
                    "description": "A URL plugin",
                    "source": { "source": "url", "url": "https://example.com/repo.git" }
                }
            ]
        }"#;

		let config: MarketplaceConfig = serde_json::from_str(json).unwrap();
		assert_eq!(config.name, "test-marketplace");
		assert_eq!(config.plugins.len(), 3);
	}
}
