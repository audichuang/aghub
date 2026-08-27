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
//! github.com): a tip preflight that downloads no objects skips the fetch when
//! the upstream tip is unchanged and the installed copy is provably unmodified,
//! otherwise a treeless fetch + hash compare yields real
//! `upToDate`/`updateAvailable`.
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
	check_updates, CheckDeps, EntryInput, Fetcher, GitFetcher, RefResolver,
	ResultCache, SourceRef,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tabled::builder::Builder;
use tabled::settings::Style;

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
	/// Did this row involve a real upstream lookup?
	///
	/// `false` for every row of the OFFLINE default, where `reason: "network"`
	/// means "we did not look" — a statement the payload could not previously
	/// make. The human renderer said so in a trailing note; the JSON branch
	/// returns before that note is even computed, so a stored or forwarded
	/// payload read as "the network failed" when nothing was attempted.
	checked: bool,
	#[serde(flatten)]
	status: StatusView,
}

/// Render the update statuses as a table, or the exact `SkillUpdateView` array
/// under `--json`. `check` is the command a human runs to decide whether to
/// bother with `apply-update`, so the default has to be readable; the JSON
/// shape stays byte-identical for scripts.
fn print_updates(
	views: &[SkillUpdateView],
	json: bool,
	online: bool,
) -> Result<()> {
	if json {
		println!("{}", serde_json::to_string_pretty(views)?);
		return Ok(());
	}
	if views.is_empty() {
		println!("No locked skills to check.");
		return Ok(());
	}
	let mut builder = Builder::default();
	builder.push_record(["SKILL", "SCOPE", "STATUS", "DETAIL"]);
	let mut outdated = 0usize;
	for v in views {
		let (status, detail) = match &v.status {
			StatusView::UpToDate => ("up-to-date", String::new()),
			StatusView::UpdateAvailable { current, available } => {
				outdated += 1;
				(
					"update-available",
					format!("{} -> {}", short(current), short(available)),
				)
			}
			StatusView::Renamed { new_name } => {
				("renamed", format!("upstream name is now '{new_name}'"))
			}
			StatusView::Uncheckable { reason } => {
				("uncheckable", reason.clone())
			}
		};
		builder.push_record([
			v.name.clone(),
			v.scope.clone(),
			status.to_string(),
			detail,
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	if outdated > 0 {
		println!(
			"{outdated} skill(s) can be updated: aghub-cli apply-update \
skills <name> --yes"
		);
	}
	if should_suggest_online(views, online) {
		println!(
			"note: remote sources report `network` because check is offline \
by default; pass --online for a real check"
		);
	}
	Ok(())
}

/// Whether to tell the user to re-run with `--online`.
///
/// Only for the OFFLINE default, where `network` means "we did not look".
/// After `--online` the same reason is a REAL fetch failure, and pointing that
/// user at the flag they already passed is both wrong and a dead end.
///
/// Split out from the renderer so it is testable without a network round-trip:
/// reaching the `--online` branch for real needs a live (failing) fetch.
fn should_suggest_online(views: &[SkillUpdateView], online: bool) -> bool {
	!online
		&& views.iter().any(|v| {
			matches!(&v.status, StatusView::Uncheckable { reason } if reason == "network")
		})
}

/// Shorten a content hash for the table; anything that is not a long hash (a
/// version string, say) is left alone.
fn short(hash: &str) -> String {
	// Char-based, not byte-based: `available` is a content hash today but the
	// field is a free-form String, and slicing a multi-byte value at byte 12
	// would panic instead of shortening it.
	let mut chars = hash.chars();
	let head: String = chars.by_ref().take(12).collect();
	if chars.next().is_some() {
		format!("{head}…")
	} else {
		head
	}
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
	json: bool,
) -> Result<()> {
	match resource {
		// Unreachable from the CLI: `Commands::Check` takes the narrowed
		// `SkillResource`, so clap rejects `mcps` at parse time with an exact
		// `[possible values: skills]`. Kept as defence-in-depth because this fn
		// still takes the full `ResourceType`.
		ResourceType::Mcps => {
			anyhow::bail!("`check` only supports skills");
		}
		ResourceType::Skills => {}
	}

	let want_global =
		matches!(scope, ResourceScope::GlobalOnly | ResourceScope::Both);
	let want_project =
		matches!(scope, ResourceScope::ProjectOnly | ResourceScope::Both);

	// `check`'s entire answer comes from the lock, so an unreadable one has to
	// fail rather than read as "no skills need updating".
	crate::commands::assert_locks_readable(
		want_global,
		want_project.then_some(project_root).flatten(),
	)?;

	if online {
		return execute_online(scope, project_root, json);
	}

	let mut views: Vec<SkillUpdateView> = Vec::new();

	if want_global {
		let locked = skill::get_all_locked_skills();
		eprintln_verbose!("Checking {} global locked skill(s)", locked.len());
		for (name, entry) in locked {
			views.push(SkillUpdateView {
				name,
				scope: "global".to_string(),
				// Offline default: nothing was looked up.
				checked: false,
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
				checked: false,
				status: StatusView::Uncheckable {
					reason: offline_reason(&entry.source_type).to_string(),
				},
			});
		}
	}

	print_updates(&views, json, false)?;
	Ok(())
}

/// Per-fetch timeout / deadline / concurrency for an online check (mirrors the
/// desktop API defaults so both surfaces behave the same).
const PER_FETCH: Duration = Duration::from_secs(30);
const OVERALL_DEADLINE: Duration = Duration::from_secs(120);
/// See the API's constant of the same name for why this stays at 4: it is an
/// OUTER cap over a fetch that already runs 16 blob workers, so it multiplies.
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
	json: bool,
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

	// One repository behind both: the preflight's tip resolution and the fetch
	// that may follow it share the composite, its snapshot memo, and its token
	// context.
	let git_fetcher = GitFetcher::new();
	let ref_resolver: Arc<dyn RefResolver> =
		Arc::new(git_fetcher.ref_resolver());
	let fetcher: Arc<dyn Fetcher> = Arc::new(git_fetcher);
	let resolver = EnvTokenResolver;
	let mut cache = ResultCache::new(CACHE_TTL);
	let deps = CheckDeps {
		fetcher,
		ref_resolver: Some(ref_resolver),
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
			// --online: this row IS the result of a real lookup, so an
			// `Uncheckable { reason: "network" }` here is a genuine failure.
			checked: true,
			status: status_view(output.status),
		})
		.collect();
	views.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.name.cmp(&b.name)));

	print_updates(&views, json, true)?;
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
				source: skill_update::sources::entry_clone_source(
					&entry.source,
					Some(&entry.source_url),
					&entry.source_type,
				),
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
				// The shared coordinate — NOT a local `source_url.unwrap_or(
				// source)`, which reads a legacy GitLab entry's `group/repo` as
				// GitHub shorthand and checks it against the wrong repository.
				source: skill_update::sources::entry_clone_source(
					&entry.source,
					entry.source_url.as_deref(),
					&entry.source_type,
				),
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
			checked: true,
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

	fn uncheckable(reason: &str) -> SkillUpdateView {
		SkillUpdateView {
			name: "n".to_string(),
			scope: "global".to_string(),
			checked: false,
			status: StatusView::Uncheckable {
				reason: reason.to_string(),
			},
		}
	}

	#[test]
	fn offline_network_rows_suggest_online() {
		assert!(super::should_suggest_online(
			&[uncheckable("network")],
			false
		));
	}

	#[test]
	fn online_network_rows_do_not_suggest_online() {
		// After --online, `network` is a real fetch failure; re-running with
		// the flag already passed changes nothing.
		assert!(!super::should_suggest_online(
			&[uncheckable("network")],
			true
		));
	}

	#[test]
	fn a_local_only_reason_never_suggests_online() {
		// `local` sources have no remote to check; --online cannot help.
		assert!(!super::should_suggest_online(
			&[uncheckable("local")],
			false
		));
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
