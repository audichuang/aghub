# Hermes Agent Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `hermes` (Nous Research Hermes Agent) as a first-class aghub agent — manage its `~/.hermes/skills` (SKILL.md) and its `mcp_servers` in `~/.hermes/config.yaml`.

**Architecture:** Follow the standard descriptor pattern. The only new machinery is a YAML MCP format module (`format/yaml_hermes.rs`) — today `format/` has JSON and TOML but no YAML. Hermes is global-only (no project scope); paths are hand-written (not the macros, which force project paths). Sub-agents unsupported (Hermes has no file-based definition).

**Tech Stack:** Rust (workspace crates `aghub-agents`, `aghub-core`), `serde_yaml` (already a dep), existing `mcp_strategy` / `load_scoped_mcps` / `save_scoped_mcps` seam.

**Design spec:** `docs/specs/2026-07-14-hermes-agent-support.md` (Codex-reviewed, 5 findings folded in).

## Global Constraints

- **Rust style**: hard tabs (width 4), 80-col, `rustfmt`, `clippy -D warnings`. Comments in English.
- **No new workspace deps** — `serde_yaml`, `dirs` already available in `aghub-agents`.
- **Never write real `~/.hermes`** in tests — pure string tests where possible; FS tests use `tempfile`/isolated `$HOME`.
- **serde_yaml errors** wrap in `ConfigError::InvalidConfig(format!(...))` — do **not** add an error variant (no `Yaml` variant exists; do not touch `errors.rs`).
- Run package-scoped tests only: `cargo test -p aghub-agents` / `-p aghub-core`. `git push` is NOT part of any task.
- Commit messages in 繁體中文, no attribution/Co-Authored-By lines.

---

### Task 1: Shared save-path read-error fix (data-safety)

Fix `save_mcps_to_file` so a read failure other than NotFound propagates instead of silently discarding the original file — otherwise a shared `config.yaml` gets rewritten from `mcp_servers` alone, nuking every other key (Codex #1). This is shared by all agents and strictly safer.

**Files:**

- Modify: `crates/agents/src/descriptor.rs` (`save_mcps_to_file`, ≈ line 291)
- Test: `crates/agents/src/descriptor.rs` (in-module `#[cfg(test)]`)

**Interfaces:**

- Consumes: nothing new.
- Produces: no signature change — `save_mcps_to_file(path, mcps, serialize)` behaves identically for existing/absent files, but now returns `Err` on a non-NotFound read error.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module in `crates/agents/src/descriptor.rs` (create the module if absent):

```rust
#[test]
fn save_mcps_to_file_preserves_unrelated_content_via_serializer() {
	// A serializer that appends " KEEP" proves the original was read, not
	// discarded. Regression for the `.ok()`-swallows-errors bug.
	use crate::models::AgentConfig;
	fn ser(_c: &AgentConfig, original: Option<&str>) -> Result<String> {
		Ok(format!("{} KEEP", original.unwrap_or("NONE")))
	}
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("config.yaml");
	std::fs::write(&path, "model: gpt\n").unwrap();
	save_mcps_to_file(&path, &[], ser).unwrap();
	let out = std::fs::read_to_string(&path).unwrap();
	assert_eq!(out, "model: gpt\n KEEP");
}
```

- [ ] **Step 2: Run test to verify it passes/fails**

Run: `cargo test -p aghub-agents save_mcps_to_file_preserves_unrelated_content_via_serializer -- --exact`
Expected: PASS already for the present-file case (the `.ok()` bug only bites on read _errors_, hard to trigger portably). This test locks in "readable original is passed through". Proceed regardless — the fix below hardens the error branch.

- [ ] **Step 3: Apply the fix**

Replace in `save_mcps_to_file`:

```rust
	let original_content = fs::read_to_string(path).ok();
```

with:

```rust
	let original_content = match fs::read_to_string(path) {
		Ok(content) => Some(content),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
		Err(e) => return Err(e.into()),
	};
```

Confirm `tempfile` is available to `aghub-agents` dev tests. Run:
`grep -n "tempfile" crates/agents/Cargo.toml`
If it is not under `[dev-dependencies]`, add `tempfile = { workspace = true }` there (it is already a workspace dependency used elsewhere).

- [ ] **Step 4: Run tests**

Run: `cargo test -p aghub-agents` then `cargo clippy -p aghub-agents --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/agents/src/descriptor.rs crates/agents/Cargo.toml
git commit -m "fix(agents): save_mcps_to_file 只把 NotFound 當缺檔，其餘讀取錯誤上拋（防共用設定被覆蓋）"
```

---

### Task 2: `yaml_hermes` MCP format module

The heart of the feature: parse/serialize Hermes' `mcp_servers` YAML with strict parsing, non-mcp-key preservation, per-server field preservation, stale-key removal, and an exhaustive transport match (Codex #2/#4/#5).

**Files:**

- Create: `crates/agents/src/format/yaml_hermes.rs`
- Modify: `crates/agents/src/format/mod.rs` (add `pub mod yaml_hermes;`)
- Test: in-module `#[cfg(test)]` in `yaml_hermes.rs`

**Interfaces:**

- Consumes: `crate::errors::{ConfigError, Result}`, `crate::models::{AgentConfig, McpServer, McpTransport}`.
- Produces:
    - `pub fn parse(content: &str) -> Result<AgentConfig>`
    - `pub fn serialize(config: &AgentConfig, original: Option<&str>) -> Result<String>`
      (Same shape as `json_map::{parse,serialize}` minus the `server_key` arg.)

- [ ] **Step 1: Declare the module**

In `crates/agents/src/format/mod.rs` add (keep alphabetical with the others):

```rust
pub mod yaml_hermes;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/agents/src/format/yaml_hermes.rs` with ONLY the test module first (so it fails to compile → red):

```rust
#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::{AgentConfig, McpServer, McpTransport};

	#[test]
	fn parse_stdio_and_remote() {
		let yaml = "
mcp_servers:
  time:
    command: uvx
    args: [\"mcp-server-time\"]
    env:
      TZ: UTC
  notion:
    url: https://mcp.notion.com/mcp
";
		let cfg = parse(yaml).unwrap();
		assert_eq!(cfg.mcps.len(), 2);
		let time = cfg.mcps.iter().find(|m| m.name == "time").unwrap();
		assert!(matches!(time.transport, McpTransport::Stdio { .. }));
		let notion = cfg.mcps.iter().find(|m| m.name == "notion").unwrap();
		assert!(matches!(
			notion.transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn parse_enabled_flag() {
		let yaml = "
mcp_servers:
  on:
    command: a
  off:
    command: b
    enabled: false
";
		let cfg = parse(yaml).unwrap();
		assert!(cfg.mcps.iter().find(|m| m.name == "on").unwrap().enabled);
		assert!(!cfg.mcps.iter().find(|m| m.name == "off").unwrap().enabled);
	}

	#[test]
	fn parse_rejects_non_mapping_servers() {
		assert!(parse("mcp_servers: 5\n").is_err());
	}

	#[test]
	fn parse_rejects_entry_without_command_or_url() {
		assert!(parse("mcp_servers:\n  bad:\n    timeout: 5\n").is_err());
	}

	#[test]
	fn parse_empty_is_ok() {
		assert!(parse("").unwrap().mcps.is_empty());
		assert!(parse("model: gpt\n").unwrap().mcps.is_empty());
	}

	#[test]
	fn serialize_preserves_other_top_level_keys() {
		let original = "model: gpt-x\nagent:\n  foo: bar\nmcp_servers:\n  old:\n    command: c\n";
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"srv",
				McpTransport::stdio("run", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, Some(original)).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		assert_eq!(v.get("model").unwrap().as_str(), Some("gpt-x"));
		assert_eq!(
			v.get("agent").unwrap().get("foo").unwrap().as_str(),
			Some("bar")
		);
		// old server replaced by the new desired set
		let servers = v.get("mcp_servers").unwrap();
		assert!(servers.get("srv").is_some());
		assert!(servers.get("old").is_none());
	}

	#[test]
	fn serialize_preserves_per_server_extra_fields() {
		let original = "mcp_servers:\n  srv:\n    command: old\n    timeout: 120\n    sampling:\n      enabled: true\n";
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"srv",
				McpTransport::stdio("newcmd", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, Some(original)).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("command").unwrap().as_str(), Some("newcmd"));
		assert_eq!(srv.get("timeout").unwrap().as_u64(), Some(120));
		assert!(srv.get("sampling").is_some());
	}

	#[test]
	fn serialize_removes_stale_transport_keys_on_switch() {
		// stdio server re-saved as remote must not keep command/args
		let original = "mcp_servers:\n  srv:\n    command: old\n    args: [\"a\"]\n";
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"srv",
				McpTransport::StreamableHttp {
					url: "https://x/mcp".into(),
					headers: None,
					timeout: None,
				},
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, Some(original)).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("url").unwrap().as_str(), Some("https://x/mcp"));
		assert!(srv.get("command").is_none());
		assert!(srv.get("args").is_none());
	}

	#[test]
	fn serialize_sse_emitted_as_url() {
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"legacy",
				McpTransport::Sse {
					url: "https://x/sse".into(),
					headers: None,
					timeout: None,
				},
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, None).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("legacy").unwrap();
		assert_eq!(srv.get("url").unwrap().as_str(), Some("https://x/sse"));
	}

	#[test]
	fn serialize_keeps_disabled_server() {
		let mut m = McpServer::new("srv", McpTransport::stdio("c", vec![]));
		m.enabled = false;
		let cfg = AgentConfig {
			mcps: vec![m],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, None).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("enabled").unwrap().as_bool(), Some(false));
	}

	#[test]
	fn roundtrip_stable() {
		let yaml = "mcp_servers:\n  a:\n    command: x\n    args: [\"1\"]\n";
		let cfg = parse(yaml).unwrap();
		let out = serialize(&cfg, Some(yaml)).unwrap();
		let cfg2 = parse(&out).unwrap();
		assert_eq!(cfg.mcps.len(), cfg2.mcps.len());
		assert_eq!(cfg.mcps[0].name, cfg2.mcps[0].name);
	}
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p aghub-agents --lib format::yaml_hermes`
Expected: FAIL to compile — `parse`/`serialize` not defined.

- [ ] **Step 4: Write the implementation**

Prepend the module code above the test module in `crates/agents/src/format/yaml_hermes.rs`:

```rust
//! Hermes (`~/.hermes/config.yaml`) MCP serializer.
//!
//! Hermes stores MCP servers under the `mcp_servers` key of one large,
//! machine-managed YAML config with many unrelated keys. Parse is strict (no
//! silent drop); serialize preserves every other top-level key and every
//! per-server field it does not own, replacing only the transport keys.

use crate::errors::{ConfigError, Result};
use crate::models::{AgentConfig, McpServer, McpTransport};
use serde_yaml::{Mapping, Value};
use std::collections::HashMap;

const TRANSPORT_KEYS: [&str; 5] = ["command", "args", "env", "url", "headers"];

fn value_to_string_map(v: &Value) -> Option<HashMap<String, String>> {
	let map = v.as_mapping()?;
	let mut out = HashMap::new();
	for (k, val) in map {
		if let (Some(k), Some(val)) = (k.as_str(), val.as_str()) {
			out.insert(k.to_string(), val.to_string());
		}
	}
	Some(out)
}

fn string_map_to_value(map: &HashMap<String, String>) -> Value {
	// Sort keys for deterministic, diff-stable output.
	let mut keys: Vec<&String> = map.keys().collect();
	keys.sort();
	let mut out = Mapping::new();
	for k in keys {
		out.insert(Value::String(k.clone()), Value::String(map[k].clone()));
	}
	Value::Mapping(out)
}

pub fn parse(content: &str) -> Result<AgentConfig> {
	let mut config = AgentConfig::new();
	if content.trim().is_empty() {
		return Ok(config);
	}
	let root: Value = serde_yaml::from_str(content).map_err(|e| {
		ConfigError::InvalidConfig(format!("invalid Hermes config YAML: {e}"))
	})?;
	let Some(servers_val) = root.get("mcp_servers") else {
		return Ok(config);
	};
	if servers_val.is_null() {
		return Ok(config);
	}
	let servers = servers_val.as_mapping().ok_or_else(|| {
		ConfigError::InvalidConfig("`mcp_servers` is not a mapping".to_string())
	})?;
	for (name_val, server_val) in servers {
		let name = name_val.as_str().ok_or_else(|| {
			ConfigError::InvalidConfig(
				"`mcp_servers` has a non-string server name".to_string(),
			)
		})?;
		let server = server_val.as_mapping().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"Hermes MCP server `{name}` is not a mapping"
			))
		})?;
		let enabled = server
			.get("enabled")
			.and_then(Value::as_bool)
			.unwrap_or(true);
		let transport = if let Some(cmd) = server.get("command") {
			let command = cmd.as_str().unwrap_or_default().to_string();
			let args = server
				.get("args")
				.and_then(Value::as_sequence)
				.map(|seq| {
					seq.iter()
						.filter_map(|v| v.as_str().map(str::to_string))
						.collect()
				})
				.unwrap_or_default();
			let env = server.get("env").and_then(value_to_string_map);
			McpTransport::Stdio {
				command,
				args,
				env,
				timeout: None,
			}
		} else if let Some(url_val) = server.get("url") {
			let url = url_val.as_str().unwrap_or_default().to_string();
			let headers = server.get("headers").and_then(value_to_string_map);
			McpTransport::StreamableHttp {
				url,
				headers,
				timeout: None,
			}
		} else {
			return Err(ConfigError::InvalidConfig(format!(
				"Hermes MCP server `{name}` has neither `command` nor `url`"
			)));
		};
		config.mcps.push(McpServer {
			name: name.to_string(),
			enabled,
			transport,
			timeout: None,
			config_source: None,
		});
	}
	Ok(config)
}

pub fn serialize(config: &AgentConfig, original: Option<&str>) -> Result<String> {
	let mut root: Value = match original {
		Some(c) if !c.trim().is_empty() => {
			serde_yaml::from_str(c).map_err(|e| {
				ConfigError::InvalidConfig(format!(
					"failed to parse existing Hermes config: {e}"
				))
			})?
		}
		_ => Value::Mapping(Mapping::new()),
	};
	let root_map = root.as_mapping_mut().ok_or_else(|| {
		ConfigError::InvalidConfig(
			"Hermes config root is not a mapping".to_string(),
		)
	})?;

	// Existing per-server entries preserve fields we do not own.
	let existing: Mapping = root_map
		.get("mcp_servers")
		.and_then(Value::as_mapping)
		.cloned()
		.unwrap_or_default();

	let mut servers = Mapping::new();
	for mcp in &config.mcps {
		let mut entry = existing
			.get(mcp.name.as_str())
			.and_then(Value::as_mapping)
			.cloned()
			.unwrap_or_default();
		// Remove all transport-owned keys before re-inserting (avoids stale
		// keys when a server's transport changes).
		for k in TRANSPORT_KEYS {
			entry.remove(k);
		}
		match &mcp.transport {
			McpTransport::Stdio {
				command, args, env, ..
			} => {
				entry.insert(
					Value::String("command".to_string()),
					Value::String(command.clone()),
				);
				entry.insert(
					Value::String("args".to_string()),
					Value::Sequence(
						args.iter()
							.map(|a| Value::String(a.clone()))
							.collect(),
					),
				);
				if let Some(env) = env {
					entry.insert(
						Value::String("env".to_string()),
						string_map_to_value(env),
					);
				}
			}
			// Hermes has a single remote transport (`url`); serialize the
			// deprecated Sse arm identically so a transferred SSE server
			// survives and the match stays exhaustive.
			McpTransport::Sse { url, headers, .. }
			| McpTransport::StreamableHttp { url, headers, .. } => {
				entry.insert(
					Value::String("url".to_string()),
					Value::String(url.clone()),
				);
				if let Some(headers) = headers {
					entry.insert(
						Value::String("headers".to_string()),
						string_map_to_value(headers),
					);
				}
			}
		}
		entry.insert(
			Value::String("enabled".to_string()),
			Value::Bool(mcp.enabled),
		);
		servers.insert(
			Value::String(mcp.name.clone()),
			Value::Mapping(entry),
		);
	}
	root_map.insert(
		Value::String("mcp_servers".to_string()),
		Value::Mapping(servers),
	);
	serde_yaml::to_string(&root).map_err(|e| {
		ConfigError::InvalidConfig(format!(
			"failed to serialize Hermes config: {e}"
		))
	})
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p aghub-agents --lib format::yaml_hermes`
Expected: all PASS.
Then: `cargo clippy -p aghub-agents --all-targets -- -D warnings` → no warnings.
(If clippy flags `McpServer::new`/`McpTransport::stdio` signatures differing from the tests, adjust the test constructors to the real ones in `models.rs` — do not change the module.)

- [ ] **Step 6: Commit**

```bash
git add crates/agents/src/format/yaml_hermes.rs crates/agents/src/format/mod.rs
git commit -m "feat(agents): 新增 Hermes YAML MCP 格式模組（嚴格解析、保留其餘設定與 per-server 欄位）"
```

---

### Task 3: `mcp_strategy` wrappers + Hermes descriptor

Wire the format module into the strategy seam and create the agent descriptor (global-only, hand-written paths, `hermes_home()` platform helper).

**Files:**

- Modify: `crates/agents/src/descriptor.rs` (`mcp_strategy` module)
- Create: `crates/agents/src/agents/hermes.rs`
- Modify: `crates/agents/src/agents/mod.rs` (add `pub mod hermes;`)

**Interfaces:**

- Consumes: `crate::format::yaml_hermes::{parse, serialize}` (Task 2); `load_scoped_mcps`/`save_scoped_mcps`; `home_dir`; `AgentDescriptor`, `Capabilities`, `GlobalSkillPaths`, `load_sub_agents_noop`, `save_sub_agents_noop`.
- Produces:
    - `mcp_strategy::parse_yaml_hermes_mcp_servers`, `mcp_strategy::serialize_yaml_hermes_mcp_servers`
    - `agents::hermes::DESCRIPTOR: AgentDescriptor` (id `"hermes"`).

- [ ] **Step 1: Add the strategy wrappers**

In `crates/agents/src/descriptor.rs`, inside `pub mod mcp_strategy { … }` (next to `parse_json_map_mcp_servers`), add:

```rust
	pub fn parse_yaml_hermes_mcp_servers(content: &str) -> Result<AgentConfig> {
		yaml_hermes::parse(content)
	}
	pub fn serialize_yaml_hermes_mcp_servers(
		config: &AgentConfig,
		original: Option<&str>,
	) -> Result<String> {
		yaml_hermes::serialize(config, original)
	}
```

Ensure the module can see `yaml_hermes` — the existing `mcp_strategy` module already refers to `json_map`; add the matching import at the top of the `mcp_strategy` module body (mirror how `json_map` is brought in, e.g. `use crate::format::yaml_hermes;`).

- [ ] **Step 2: Create the descriptor**

Create `crates/agents/src/agents/hermes.rs`:

```rust
use crate::descriptor::*;
use std::path::{Path, PathBuf};

// Hermes home: `~/.hermes` on POSIX/WSL2, `%LOCALAPPDATA%\hermes` on native
// Windows. Both arms are compiled on every platform (cfg blocks inside one fn)
// so there is no unused-fn / Windows-clippy gap.
fn hermes_home() -> Option<PathBuf> {
	#[cfg(windows)]
	{
		dirs::data_local_dir().map(|d| d.join("hermes"))
	}
	#[cfg(not(windows))]
	{
		home_dir().map(|h| h.join(".hermes"))
	}
}

fn mcp_global_path() -> Option<PathBuf> {
	hermes_home().map(|h| h.join("config.yaml"))
}

fn global_data_dir() -> Option<PathBuf> {
	hermes_home()
}

fn load_mcps(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
) -> crate::Result<Vec<crate::McpServer>> {
	load_scoped_mcps(
		project_root,
		scope,
		Some(mcp_global_path),
		None,
		mcp_strategy::parse_yaml_hermes_mcp_servers,
	)
}

fn save_mcps(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
	mcps: &[crate::McpServer],
) -> crate::Result<()> {
	save_scoped_mcps(
		project_root,
		scope,
		mcps,
		Some(mcp_global_path),
		None,
		mcp_strategy::serialize_yaml_hermes_mcp_servers,
	)
}

fn global_skills_read() -> Vec<PathBuf> {
	match hermes_home() {
		Some(h) => vec![h.join("skills")],
		None => Vec::new(),
	}
}

fn global_skills_write() -> Option<PathBuf> {
	hermes_home().map(|h| h.join("skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "hermes",
	display_name: "Hermes",
	mcp_parse_config: Some(mcp_strategy::parse_yaml_hermes_mcp_servers),
	mcp_serialize_config: Some(mcp_strategy::serialize_yaml_hermes_mcp_servers),
	load_mcps,
	save_mcps,
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: None,
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: false,
			},
			universal: false,
		},
		mcp: McpCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: false,
			},
			stdio: true,
			remote: true,
			enable_disable: true,
		},
		sub_agents: SubAgentCapabilities {
			scopes: ScopeSupport {
				global: false,
				project: false,
			},
		},
	},
	global_skill_paths: Some(GlobalSkillPaths {
		read: global_skills_read,
		write: global_skills_write,
	}),
	project_skill_paths: None,
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	cli_name: "hermes",
	validate_args: &["--version"],
	project_markers: &[],
	skills_cli_name: None,
};
```

Note on `skills_cli_name`: set to `None` unless you confirm the `hermes` CLI has a skills subcommand — run `hermes --help 2>/dev/null | grep -i skill` (or check `~/research/hermes-agent/cli.py`). If Hermes exposes `hermes skills …`, change to `Some("hermes")`. `None` is the safe default (skill install still works via symlink; only Hermes-CLI-driven sync/usage is gated).

- [ ] **Step 3: Register the module**

In `crates/agents/src/agents/mod.rs` add (keep alphabetical):

```rust
pub mod hermes;
```

- [ ] **Step 4: Build**

Run: `cargo build -p aghub-agents`
Expected: compiles. If `AgentDescriptor` has fields not set above (compile error naming a missing field), match a sibling descriptor (`augmentcode.rs`) for the exact field set — do not invent values.
Then: `cargo clippy -p aghub-agents --all-targets -- -D warnings`

- [ ] **Step 5: Commit**

```bash
git add crates/agents/src/descriptor.rs crates/agents/src/agents/hermes.rs crates/agents/src/agents/mod.rs
git commit -m "feat(agents): 新增 Hermes descriptor（global-only、平台感知 hermes_home、skills+MCP）"
```

---

### Task 4: Wire `AgentType` + core registry (make Hermes usable)

Add the enum variant and register the descriptor so `AgentType`/CLI/API/desktop all reach Hermes, and prove it via an integration test.

**Files:**

- Modify: `crates/agents/src/models.rs` (`AgentType` enum, `ALL`, `as_str`, `from_str`)
- Modify: `crates/core/src/registry/mod.rs` (`ALL_AGENTS`)
- Test: `crates/agents/src/models.rs` in-module test + `crates/core/tests/` (a small integration check)

**Interfaces:**

- Consumes: `agents::hermes::DESCRIPTOR` (Task 3).
- Produces: `AgentType::Hermes` (`as_str()` → `"hermes"`, `from_str("hermes")` → `Ok(Hermes)`); `registry::get("hermes")` → the Hermes descriptor.

- [ ] **Step 1: Write the failing tests**

In `crates/agents/src/models.rs` `#[cfg(test)]` module add:

```rust
	#[test]
	fn hermes_agent_type_roundtrip() {
		let a = AgentType::from_str("hermes").unwrap();
		assert_eq!(a, AgentType::Hermes);
		assert_eq!(a.as_str(), "hermes");
		assert!(AgentType::ALL.contains(&AgentType::Hermes));
	}
```

In `crates/core/tests/` add a new file `crates/core/tests/hermes_agent.rs`:

```rust
use aghub_core::registry;

#[test]
fn registry_resolves_hermes_not_fallback() {
	let d = registry::get("hermes");
	assert_eq!(d.id, "hermes");
	// global-only: has a global MCP path, no project path
	assert!(d.mcp_global_path.is_some());
	assert!(d.mcp_project_path.is_none());
	assert!(d.capabilities.skills.scopes.global);
	assert!(!d.capabilities.skills.scopes.project);
	assert!(d.capabilities.mcp.enable_disable);
}
```

Confirm the real import path for `registry::get` (check `crates/core/tests/integration_tests.rs` for how it imports `aghub_core`). Adjust the `use` line to match.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aghub-agents hermes_agent_type_roundtrip -- --exact`
Expected: FAIL to compile — `AgentType::Hermes` missing.

- [ ] **Step 3: Add the enum variant + mappings**

In `crates/agents/src/models.rs`:

Add to the enum (after `JetBrainsAi,`):

```rust
	Hermes,
```

Add to `ALL` (after `AgentType::JetBrainsAi,`):

```rust
		AgentType::Hermes,
```

Add to `as_str()` match (after the `JetBrainsAi` arm):

```rust
			AgentType::Hermes => "hermes",
```

Add to `from_str()` match (after the `jetbrains-ai` arm):

```rust
			"hermes" => Ok(AgentType::Hermes),
```

- [ ] **Step 4: Register in core**

In `crates/core/src/registry/mod.rs`, add to the `ALL_AGENTS` array (anywhere in the list):

```rust
	&agents::hermes::DESCRIPTOR,
```

- [ ] **Step 5: Run tests**

Run:

```
cargo test -p aghub-agents hermes_agent_type_roundtrip -- --exact
cargo test -p aghub-core --test hermes_agent
cargo clippy -p aghub-agents -p aghub-core --all-targets -- -D warnings
```

Expected: PASS, no warnings. If `from_str` returns a custom error type, match the existing arms' `Ok(...)`/error shape exactly.

- [ ] **Step 6: Commit**

```bash
git add crates/agents/src/models.rs crates/core/src/registry/mod.rs crates/core/tests/hermes_agent.rs
git commit -m "feat(core): 註冊 Hermes agent（AgentType + registry ALL_AGENTS）"
```

---

### Task 5: Docs + doc-sync

Update the navigation docs and confirm no doc-sync/count assertion breaks.

**Files:**

- Modify: `crates/agents/AGENTS.md` (agent count 23 → 24)
- Modify: `AGENTS.md` (root — add a Hermes bullet under "Agent-Specific Behavior")
- Modify: `UPSTREAM.md` (note Hermes as fork-only)

**Interfaces:** none (docs only).

- [ ] **Step 1: Check for a doc-sync / count assertion**

Run:

```
grep -rn "AgentType::ALL" crates --include=*.rs | grep -i test
grep -rn "doc.sync\|doc_sync" crates .github 2>/dev/null
grep -rn "\b23\b" crates/agents/AGENTS.md AGENTS.md
```

If any test asserts a literal agent count or an exact agent-id list, update its expectation to include `hermes`. (Design review found none, but re-verify against current tree.)

- [ ] **Step 2: Update `crates/agents/AGENTS.md`**

Change the line reading `` `AgentType::ALL` = 23 `` (in the STRUCTURE section, `agents/` bullet) to `24`.

- [ ] **Step 3: Update root `AGENTS.md`**

Under "## Agent-Specific Behavior", add a bullet:

```markdown
- **Hermes** (Nous Research): global-only. Skills from `~/.hermes/skills/`
  (SKILL.md). MCP under the `mcp_servers` key of `~/.hermes/config.yaml` (YAML;
  the only YAML MCP agent) — single remote transport (`url`), native `enabled`
  flag (`enable_disable: true`); other top-level keys preserved on rewrite
  (comments are not). Windows home is `%LOCALAPPDATA%\hermes`. No project scope,
  no sub-agents.
```

- [ ] **Step 4: Update `UPSTREAM.md`**

Add a row/note recording Hermes as a **fork-only** agent (not present upstream `AkaraChen/aghub`), referencing this spec/plan date.

- [ ] **Step 5: Full verification gate**

Run:

```
just fmt
cargo test -p aghub-agents -p aghub-core
cargo clippy -p aghub-agents -p aghub-core --all-targets -- -D warnings
```

Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/agents/AGENTS.md AGENTS.md UPSTREAM.md
git commit -m "docs: 記錄 Hermes agent（fork-only、YAML config、global-only）"
```

---

## Self-Review

**Spec coverage:**

- §1 YAML module → Task 2 ✅
- §1b shared save fix → Task 1 ✅
- §2 mcp_strategy wrappers → Task 3 ✅
- §3 descriptor (hermes_home, paths, capabilities) → Task 3 ✅
- §4 wiring (mod.rs, models.rs, registry) → Tasks 3 & 4 ✅
- §5 tests (yaml_hermes units, wiring, shared fix) → Tasks 1, 2, 4 ✅
- §6 docs (AGENTS.md ×2, UPSTREAM, doc-sync) → Task 5 ✅; optional hermes.svg noted (not a task — polish)
- Codex #1→Task1, #2/#4/#5→Task2, #3→Task3 ✅

**Placeholder scan:** no TBD/TODO; every code step has complete code.

**Type consistency:** `parse(&str)->Result<AgentConfig>` / `serialize(&AgentConfig, Option<&str>)->Result<String>` consistent across Tasks 2/3; `mcp_strategy::{parse,serialize}_yaml_hermes_mcp_servers` names consistent Tasks 3/4; `AgentType::Hermes` / id `"hermes"` consistent Tasks 3/4/5.

**Known verification points flagged inline for implementers:** exact `McpServer`/`McpTransport` constructor signatures (Task 2 Step 5), `AgentDescriptor` field set vs `augmentcode.rs` (Task 3 Step 4), `registry::get` import path (Task 4 Step 1), `from_str` error shape (Task 4 Step 5).
