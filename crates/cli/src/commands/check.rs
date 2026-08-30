//! Read-only `check skills` subcommand.
//!
//! Reports each locked skill's update status as a `SkillUpdateResponse`-shaped
//! JSON array (camelCase, `status`-tagged).
//!
//! **Default (offline).** No network, and no disk hashing either: the SHARED
//! orchestrator is run with `offline`, so a source it could never fetch keeps
//! its permanent reason (`local`, `ssh`, `unsupportedScheme`) and everything
//! else is reported `Uncheckable { network }` — "we did not look".
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
use aghub_core::skills::update::{SkillUpdateStatus, UncheckableReason};
use anyhow::Result;
use serde::Serialize;
use skill_update::{
	check_updates, projection, CheckDeps, EntryInput, Fetcher, GitFetcher,
	RefResolver, ResultCache,
};
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
	// ONE read, consumed below. Reading the lock again after a yes/no probe
	// left a window a non-aghub writer could truncate the file in, and the
	// second read would fall open to an empty lock — the same "no skills
	// installed" answer the probe exists to prevent.
	let locks = crate::commands::read_locks_checked(
		want_global,
		want_project.then_some(project_root).flatten(),
	)?;

	// ONE path for both modes. `offline` is a flag on the shared orchestrator,
	// not a second implementation: the CLI used to build its own offline rows
	// and map a `local` lock entry to reason `local` while the orchestrator
	// answered `network` for the very same entry under the very same flag.
	run_check(locks, project_root, online, json)
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

/// Run the shared `skill-update` orchestrator over the already-read locks, with
/// the env token resolver and the default git adapters.
///
/// `online == false` is the DEFAULT `check`: the orchestrator is told `offline`
/// and answers every row without touching the network — a source it could never
/// fetch (local / ssh / unsupported scheme) still gets its permanent reason,
/// everything else gets `network`, meaning "we did not look". That distinction
/// is the orchestrator's, not this file's.
///
/// **Read-only** either way — it never heals either lock (the desktop API owns
/// global-lock self-heal; the CLI `check` stays non-mutating, and the project
/// lock is VCS-tracked).
fn run_check(
	locks: crate::commands::LockSnapshot,
	project_root: Option<&Path>,
	online: bool,
	json: bool,
) -> Result<()> {
	// The SHARED projection: `wanted`-scoped hashing, the per-root memo, and the
	// lock-before-disk read order all come from `skill_update::projection`
	// rather than a private copy that drifted from the API's.
	//
	// The lock closures hand back the ALREADY-READ snapshot. Reading it again
	// here would reopen the window the fail-closed probe exists to close — see
	// `LockSnapshot`.
	let mut entries: Vec<EntryInput> = Vec::new();
	if let Some(global) = locks.global {
		entries.extend(projection::global_lock_entries(!online, || global).0);
	}
	if let Some(project) = locks.project {
		entries.extend(
			projection::project_lock_entries(!online, project_root, || project)
				.0,
		);
	}
	eprintln_verbose!(
		"Checking {} locked skill(s) ({})",
		entries.len(),
		if online { "online" } else { "offline" }
	);

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
		offline: !online,
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
			// Only `--online` looked upstream, so an `Uncheckable { reason:
			// "network" }` is a genuine failure there and "we did not look"
			// otherwise.
			checked: online,
			status: status_view(output.status),
		})
		.collect();
	views.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.name.cmp(&b.name)));

	print_updates(&views, json, online)?;
	Ok(())
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
