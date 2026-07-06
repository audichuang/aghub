//! Read-only `check skills` subcommand.
//!
//! Reports each locked skill's update status as a `SkillUpdateResponse`-shaped
//! JSON array (camelCase, `status`-tagged).
//!
//! **Default (offline).** No network: any skill whose freshness needs a remote
//! fetch is reported `Uncheckable` (reason `network` for remote sources,
//! `local` for local-only sources).
//!
//! **`--online` (alias `--check-remote`).** Opt-in network check that runs the
//! shared [`skill_update`] orchestrator with the same env token resolver as
//! the `source` commands (`GIT_PASSWORD` on any host, `GITHUB_TOKEN` bound to
//! github.com): a cheap ls-refs preflight skips the fetch when the upstream
//! tip is unchanged and the installed copy is provably unmodified, otherwise a
//! treeless fetch + hash compare yields real `upToDate`/`updateAvailable`.
//!
//! Either way `check` is **read-only**: it never mutates either lock (the
//! desktop API owns global-lock self-heal; the project lock is VCS-tracked).

use crate::{eprintln_verbose, ResourceType};
use aghub_core::models::ResourceScope;
use aghub_core::skills::removal::skill_root;
use aghub_core::skills::update::{SkillUpdateStatus, UncheckableReason};
use anyhow::Result;
use serde::Serialize;
use skill_update::{
	check_updates, CheckDeps, EntryInput, Fetcher, GitFetcherWithFallback,
	GitRefResolver, ResultCache, SourceRef,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Flattened, camelCase status mirroring `aghub-api`'s `SkillUpdateStatusResponse`
/// (the CLI does not depend on the api crate, so the shape is duplicated here).
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
// Mirrors `aghub-api`'s `SkillUpdateStatusResponse`. The offline path only ever
// emits `Uncheckable`; `--online` emits all three.
enum StatusView {
	UpToDate,
	UpdateAvailable {
		current: String,
		available: String,
	},
	Renamed {
		#[serde(rename = "newName")]
		new_name: String,
	},
	Uncheckable {
		reason: String,
	},
}

/// One skill's name plus its flattened update status. Mirrors
/// `aghub-api`'s `SkillUpdateResponse`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillUpdateView {
	name: String,
	scope: String,
	#[serde(flatten)]
	status: StatusView,
}

/// Map a lock entry's `source_type` to the offline `Uncheckable` reason.
/// Local sources can never be checked from a committed lock without a remote;
/// everything else needs a network fetch that lives in `crates/api`.
fn offline_reason(source_type: &str) -> &'static str {
	if source_type.eq_ignore_ascii_case("local") {
		"local"
	} else {
		"network"
	}
}

/// Map an orchestrator [`UncheckableReason`] to the camelCase reason string used
/// in the response (parity with `aghub-api`'s `SkillUpdateStatusResponse`).
fn uncheckable_reason_str(reason: UncheckableReason) -> &'static str {
	match reason {
		UncheckableReason::Auth => "auth",
		UncheckableReason::Network => "network",
		UncheckableReason::Local => "local",
		UncheckableReason::Ssh => "ssh",
		UncheckableReason::UnsupportedScheme => "unsupportedScheme",
		UncheckableReason::NoPath => "noPath",
		UncheckableReason::Timeout => "timeout",
	}
}

/// Flatten an orchestrator [`SkillUpdateStatus`] into the CLI's `StatusView`.
fn status_view(status: SkillUpdateStatus) -> StatusView {
	match status {
		SkillUpdateStatus::UpToDate => StatusView::UpToDate,
		SkillUpdateStatus::UpdateAvailable {
			current, available, ..
		} => StatusView::UpdateAvailable { current, available },
		SkillUpdateStatus::Renamed { new_name } => {
			StatusView::Renamed { new_name }
		}
		SkillUpdateStatus::Uncheckable { reason } => StatusView::Uncheckable {
			reason: uncheckable_reason_str(reason).to_string(),
		},
	}
}

pub fn execute(
	resource: ResourceType,
	scope: ResourceScope,
	project_root: Option<&Path>,
	online: bool,
	_json: bool,
) -> Result<()> {
	match resource {
		ResourceType::Mcps => {
			anyhow::bail!("`check` only supports skills");
		}
		ResourceType::Skills => {}
	}

	if online {
		return execute_online(scope, project_root);
	}

	let mut views: Vec<SkillUpdateView> = Vec::new();

	let want_global =
		matches!(scope, ResourceScope::GlobalOnly | ResourceScope::Both);
	let want_project =
		matches!(scope, ResourceScope::ProjectOnly | ResourceScope::Both);

	if want_global {
		let locked = skill::get_all_locked_skills();
		eprintln_verbose!("Checking {} global locked skill(s)", locked.len());
		for (name, entry) in locked {
			views.push(SkillUpdateView {
				name,
				scope: "global".to_string(),
				status: StatusView::Uncheckable {
					reason: offline_reason(&entry.source_type).to_string(),
				},
			});
		}
	}

	if want_project {
		let lock = skill::read_local_lock(project_root);
		eprintln_verbose!(
			"Checking {} project locked skill(s)",
			lock.skills.len()
		);
		for (name, entry) in lock.skills {
			views.push(SkillUpdateView {
				name,
				scope: "project".to_string(),
				status: StatusView::Uncheckable {
					reason: offline_reason(&entry.source_type).to_string(),
				},
			});
		}
	}

	println!("{}", serde_json::to_string_pretty(&views)?);
	Ok(())
}

/// Per-fetch timeout / deadline / concurrency for an online check (mirrors the
/// desktop API defaults so both surfaces behave the same).
const PER_FETCH: Duration = Duration::from_secs(30);
const OVERALL_DEADLINE: Duration = Duration::from_secs(120);
const CONCURRENCY: usize = 4;
const CACHE_TTL: Duration = Duration::from_secs(60);

// Token policy is shared with the `source` commands (`GIT_PASSWORD` on any
// host; `GITHUB_TOKEN` bound to github.com) so `check --online` accepts the
// same credentials as `source diff`/`sync`. `apply-update` keeps its own
// `GIT_USERNAME`/`GIT_PASSWORD` basic-auth semantics.
use super::source::EnvTokenResolver;

/// `--online` update check: run the shared `skill-update` orchestrator with the
/// env token resolver and the default git adapters. **Read-only** — it never
/// heals either lock (the desktop API owns global-lock self-heal; the CLI
/// `check` stays non-mutating, and the project lock is VCS-tracked).
fn execute_online(
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Result<()> {
	let want_global =
		matches!(scope, ResourceScope::GlobalOnly | ResourceScope::Both);
	let want_project =
		matches!(scope, ResourceScope::ProjectOnly | ResourceScope::Both);

	let mut entries: Vec<EntryInput> = Vec::new();
	if want_global {
		let local = local_hashes_for_scope(ResourceScope::GlobalOnly, None);
		entries.extend(global_lock_entries(&local));
	}
	if want_project {
		let local =
			local_hashes_for_scope(ResourceScope::ProjectOnly, project_root);
		entries.extend(project_lock_entries(project_root, &local));
	}
	eprintln_verbose!("Checking {} locked skill(s) online", entries.len());

	// System-git fallback so a private TFS/Azure DevOps repo whose only auth is
	// an OS credential helper is still checkable (GITHUB_TOKEN/GIT_PASSWORD
	// still take precedence — see GitFetcherWithFallback).
	let fetcher: Arc<dyn Fetcher> = Arc::new(GitFetcherWithFallback);
	let resolver = EnvTokenResolver;
	let mut cache = ResultCache::new(CACHE_TTL);
	let deps = CheckDeps {
		fetcher,
		ref_resolver: Some(Arc::new(GitRefResolver)),
		resolver: &resolver,
		cache: &mut cache,
		per_fetch: PER_FETCH,
		concurrency: CONCURRENCY,
		offline: false,
		overall_deadline: OVERALL_DEADLINE,
	};

	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()?;
	let outputs = runtime.block_on(check_updates(entries, deps));

	let mut views: Vec<SkillUpdateView> = outputs
		.into_iter()
		.map(|output| SkillUpdateView {
			name: output.key.name,
			scope: output.key.scope,
			status: status_view(output.status),
		})
		.collect();
	views.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.name.cmp(&b.name)));

	println!("{}", serde_json::to_string_pretty(&views)?);
	Ok(())
}

/// Hash each installed skill folder so the C1 trustworthiness gate has a
/// `local_hash` baseline. Names that resolve to differing hashes across agents
/// are dropped as ambiguous. Mirrors the API route's `local_hashes_for_scope`.
fn local_hashes_for_scope(
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> HashMap<String, String> {
	let mut hashes = HashMap::new();
	let mut ambiguous = HashSet::new();
	for agent in aghub_core::load_all_agents(resource_scope, project_root) {
		for skill in agent.skills {
			if ambiguous.contains(&skill.name) {
				continue;
			}
			let Some(root) = skill_root(&skill) else {
				continue;
			};
			let Ok(hash) = skill::compute_skill_folder_hash(&root) else {
				continue;
			};
			match hashes.get(&skill.name) {
				Some(existing) if existing != &hash => {
					hashes.remove(&skill.name);
					ambiguous.insert(skill.name);
				}
				Some(_) => {}
				None => {
					hashes.insert(skill.name, hash);
				}
			}
		}
	}
	hashes
}

/// Project the global skill lock into the orchestrator's per-entry inputs.
fn global_lock_entries(
	local_hashes: &HashMap<String, String>,
) -> Vec<EntryInput> {
	let lock = skill::lock::global::read_skill_lock();
	lock.skills
		.into_iter()
		.map(|(name, entry)| EntryInput {
			local_hash: local_hashes.get(&name).cloned(),
			name,
			scope: "global".to_string(),
			source_ref: SourceRef {
				source: entry.source_url,
				ref_: entry.ref_name,
			},
			source_type: entry.source_type,
			skill_path: entry.skill_path,
			stored_hash: entry.content_hash,
			ref_commit: entry.ref_commit,
		})
		.collect()
}

/// Project the project skill lock into the orchestrator's per-entry inputs.
fn project_lock_entries(
	project_root: Option<&Path>,
	local_hashes: &HashMap<String, String>,
) -> Vec<EntryInput> {
	let lock = skill::lock::local::read_local_lock(project_root);
	lock.skills
		.into_iter()
		.map(|(name, entry)| EntryInput {
			local_hash: local_hashes.get(&name).cloned(),
			name,
			scope: "project".to_string(),
			source_ref: SourceRef {
				source: entry.source,
				ref_: entry.ref_name,
			},
			source_type: entry.source_type,
			skill_path: entry.skill_path,
			stored_hash: Some(entry.computed_hash),
			ref_commit: entry.ref_commit,
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::update::{SkillUpdateStatus, UncheckableReason};

	fn status_json(status: SkillUpdateStatus) -> serde_json::Value {
		let view = SkillUpdateView {
			name: "n".to_string(),
			scope: "global".to_string(),
			status: status_view(status),
		};
		serde_json::to_value(view).unwrap()
	}

	#[test]
	fn status_view_maps_up_to_date() {
		let json = status_json(SkillUpdateStatus::UpToDate);
		assert_eq!(json["status"], "upToDate");
	}

	#[test]
	fn status_view_maps_update_available() {
		let json = status_json(SkillUpdateStatus::UpdateAvailable {
			current: "a".to_string(),
			available: "b".to_string(),
			upstream_commit_time: None,
		});
		assert_eq!(json["status"], "updateAvailable");
		assert_eq!(json["current"], "a");
		assert_eq!(json["available"], "b");
	}

	#[test]
	fn status_view_maps_uncheckable_reason_strings() {
		let json = status_json(SkillUpdateStatus::Uncheckable {
			reason: UncheckableReason::Auth,
		});
		assert_eq!(json["status"], "uncheckable");
		assert_eq!(json["reason"], "auth");
	}
}
