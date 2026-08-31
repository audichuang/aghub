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
use anyhow::{bail, Context, Result};
use serde::Serialize;
use skill_update::{
	check_updates, projection, CheckDeps, EntryInput, Fetcher, GitFetcher,
	RefResolver, ResultCache,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tabled::builder::Builder;
use tabled::settings::Style;

/// Flattened, camelCase status mirroring `aghub-api`'s `SkillUpdateStatusResponse`
/// (the CLI does not depend on the api crate, so the shape is duplicated here).
#[derive(Clone, Debug, Serialize)]
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
#[derive(Clone, Debug, Serialize)]
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

/// What a reader of the sidecar should DO about an uncheckable row.
///
/// The desktop's schedule summary used to call EVERY uncheckable row a
/// failure, so a healthy machine whose sources are private or local reported
/// "N failed" every single day. Only a row an online run really attempted and
/// could not finish is a failure.
#[derive(Debug, PartialEq, Eq)]
enum UncheckableBucket {
	/// Tried and did not finish — worth surfacing.
	Failed,
	/// The source needs credentials this run did not have. EXPECTED under the
	/// OS schedule: the CLI resolves tokens from `GIT_PASSWORD` /
	/// `GITHUB_TOKEN`, never from the desktop keyring, so every private source
	/// lands here.
	NeedsAuth,
	/// Nothing could have been fetched: a local/ssh/unsupported source, a lock
	/// entry with no path, or the offline default — where `network` means
	/// "we did not look", not "the network failed".
	Skipped,
}

fn uncheckable_bucket(reason: &str, online: bool) -> UncheckableBucket {
	if !online {
		return UncheckableBucket::Skipped;
	}
	match reason {
		"auth" => UncheckableBucket::NeedsAuth,
		"network" | "timeout" => UncheckableBucket::Failed,
		_ => UncheckableBucket::Skipped,
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
	write_result: Option<PathBuf>,
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
	run_check(locks, project_root, scope, online, json, write_result)
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
	scope: ResourceScope,
	online: bool,
	json: bool,
	write_result: Option<PathBuf>,
) -> Result<()> {
	// Stamped before any fetch so the sidecar's `startedAt` is the real start,
	// not a second copy of `finishedAt`.
	let started_at = chrono::Utc::now().to_rfc3339();
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
	if let Some(path) = write_result {
		let path = if path.as_os_str().is_empty() {
			default_sidecar_path()
		} else {
			path
		};
		if let Err(error) = write_check_sidecar(
			&path,
			project_root,
			started_at,
			online,
			scope_label(scope),
			&views,
		) {
			// stdout already carries the check answer under `--json`; the
			// shared failure reporter must not append a second JSON document
			// (see `note_answer_on_stdout`). The prose still goes to stderr and
			// the exit code is still non-zero.
			if json {
				crate::note_answer_on_stdout();
			}
			return Err(error);
		}
	}
	Ok(())
}

fn scope_label(scope: ResourceScope) -> &'static str {
	match scope {
		ResourceScope::GlobalOnly => "global",
		ResourceScope::ProjectOnly => "project",
		ResourceScope::Both => "both",
	}
}

fn default_sidecar_path() -> PathBuf {
	crate::commands::app_data_dir().join("skill-check-last.json")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckSidecar {
	started_at: String,
	finished_at: String,
	online: bool,
	scope: String,
	results: Vec<SkillUpdateView>,
	/// Rows an online run tried and could not finish.
	failed: usize,
	/// Rows whose source needs credentials the run did not have.
	needs_auth: usize,
	/// Rows nothing could have answered (local/ssh/unsupported/no path, or the
	/// whole offline default).
	skipped: usize,
	update_available: usize,
}

fn sidecar_from_views(
	started_at: String,
	finished_at: String,
	online: bool,
	scope: &str,
	views: &[SkillUpdateView],
) -> CheckSidecar {
	let mut failed = 0usize;
	let mut needs_auth = 0usize;
	let mut skipped = 0usize;
	for view in views {
		if let StatusView::Uncheckable { reason } = &view.status {
			match uncheckable_bucket(reason, online) {
				UncheckableBucket::Failed => failed += 1,
				UncheckableBucket::NeedsAuth => needs_auth += 1,
				UncheckableBucket::Skipped => skipped += 1,
			}
		}
	}
	let update_available = views
		.iter()
		.filter(|view| {
			matches!(view.status, StatusView::UpdateAvailable { .. })
		})
		.count();
	CheckSidecar {
		started_at,
		finished_at,
		online,
		scope: scope.to_string(),
		results: views.to_vec(),
		failed,
		needs_auth,
		skipped,
		update_available,
	}
}

/// `check` is READ-ONLY. `--write-result` takes an arbitrary path, so without
/// this a caller could aim the sidecar at managed state and have the atomic
/// write replace it — no `MutationGuard`, no rollback, and in the lock's case
/// the very file this command just read as its answer.
///
/// Refused: both skill locks in every spelling, and anything inside a
/// `.agents/skills` Master (an online check HASHES those folders, so a write
/// there would rewrite skill content the command just measured).
///
/// Normalization is `skill::lock::resolve_existing` — the tested one the
/// mutation lock already uses to give one directory one identity. A hand-rolled
/// parent/file_name walk is NOT good enough: `file_name()` is `None` for a path
/// ending in `..`, so `<root>/missing/../skills-lock.json` walked off the end
/// unnormalized and wrote straight through to the real lock.
fn refuse_lock_targets(path: &Path, project_root: Option<&Path>) -> Result<()> {
	let target = skill::lock::resolve_existing(path);

	let mut forbidden_files = vec![
		skill::lock::get_skill_lock_path(),
		// Both spellings: the resolved project root AND the cwd, which differ
		// whenever the command was run from a subdirectory.
		skill::lock::local::get_local_lock_path(None),
	];
	if let Some(root) = project_root {
		forbidden_files
			.push(skill::lock::local::get_local_lock_path(Some(root)));
	}
	for lock in forbidden_files {
		if same_path(&skill::lock::resolve_existing(&lock), &target) {
			bail!(
				"--write-result must not target a skill lock ({}): check is read-only",
				lock.display()
			);
		}
	}

	let mut forbidden_dirs = Vec::new();
	if let Some(home) = dirs::home_dir() {
		forbidden_dirs.push(home.join(".agents").join("skills"));
	}
	if let Some(root) = project_root {
		forbidden_dirs.push(root.join(".agents").join("skills"));
	}
	for dir in forbidden_dirs {
		let dir = skill::lock::resolve_existing(&dir);
		if target.starts_with(&dir) {
			bail!(
				"--write-result must not target managed skill content ({}): check is read-only",
				dir.display()
			);
		}
	}
	Ok(())
}

/// Path equality that matches the filesystem's own answer: byte-exact on Linux,
/// case-insensitive where the default filesystem is (macOS, Windows) — a
/// case-variant spelling reaches the same file there.
fn same_path(a: &Path, b: &Path) -> bool {
	if cfg!(any(target_os = "macos", target_os = "windows")) {
		let lower = |p: &Path| p.to_string_lossy().to_lowercase();
		lower(a) == lower(b)
	} else {
		a == b
	}
}

fn write_check_sidecar(
	path: &Path,
	project_root: Option<&Path>,
	started_at: String,
	online: bool,
	scope: &str,
	views: &[SkillUpdateView],
) -> Result<()> {
	refuse_lock_targets(path, project_root)?;
	let payload = sidecar_from_views(
		started_at,
		chrono::Utc::now().to_rfc3339(),
		online,
		scope,
		views,
	);
	write_sidecar_atomic(path, &payload)
}

fn write_sidecar_atomic(path: &Path, payload: &CheckSidecar) -> Result<()> {
	if path.is_dir() {
		bail!(
			"--write-result path is a directory, not a file: {}",
			path.display()
		);
	}
	let body = serde_json::to_string_pretty(payload)
		.context("serialize skill-check sidecar")?
		+ "\n";
	// The lock writer's atomic write, not a second one: a unique temp file in
	// the destination directory, fsync, then a replacing persist. A fixed
	// `.json.tmp` name would also have two concurrent runs clobber each other.
	skill::lock::atomic_write_json(path, &body)
		.with_context(|| format!("write sidecar {}", path.display()))?;
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

	#[test]
	fn sidecar_write_creates_parseable_file_with_results() {
		let tmp = tempfile::tempdir().unwrap();
		let path = tmp.path().join("skill-check-last.json");
		let payload = sidecar_from_views(
			"t0".into(),
			"t1".into(),
			false,
			"global",
			&[uncheckable("network")],
		);
		write_sidecar_atomic(&path, &payload).unwrap();
		let parsed: serde_json::Value =
			serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
		assert!(parsed["results"].is_array());
		assert_eq!(parsed["results"].as_array().unwrap().len(), 1);
		// Offline: `network` means "we did not look", so nothing failed.
		assert_eq!(parsed["failed"], 0);
		assert_eq!(parsed["skipped"], 1);
		assert_eq!(parsed["updateAvailable"], 0);
		assert_eq!(parsed["online"], false);
		assert_eq!(parsed["scope"], "global");
		assert_eq!(parsed["startedAt"], "t0");
		assert_eq!(parsed["finishedAt"], "t1");
	}

	/// A scheduled run over private + local sources is HEALTHY. Counting those
	/// rows as failures made the desktop report "N failed" every single day on
	/// a machine where nothing was wrong.
	#[test]
	fn online_sidecar_separates_real_failures_from_auth_and_unfetchable() {
		let payload = sidecar_from_views(
			"t0".into(),
			"t1".into(),
			true,
			"global",
			&[
				uncheckable("auth"),
				uncheckable("auth"),
				uncheckable("local"),
				uncheckable("ssh"),
				uncheckable("unsupportedScheme"),
				uncheckable("noPath"),
				uncheckable("network"),
				uncheckable("timeout"),
			],
		);
		assert_eq!(payload.failed, 2, "only network/timeout are failures");
		assert_eq!(payload.needs_auth, 2);
		assert_eq!(payload.skipped, 4);
	}

	/// Offline is not a failure report at all: no row was attempted.
	#[test]
	fn offline_sidecar_never_reports_a_failure() {
		let payload = sidecar_from_views(
			"t0".into(),
			"t1".into(),
			false,
			"global",
			&[uncheckable("network"), uncheckable("auth")],
		);
		assert_eq!(payload.failed, 0);
		assert_eq!(payload.needs_auth, 0);
		assert_eq!(payload.skipped, 2);
	}

	#[test]
	fn sidecar_refuses_a_directory_without_deleting_it() {
		let tmp = tempfile::tempdir().unwrap();
		let payload =
			sidecar_from_views("t0".into(), "t1".into(), false, "global", &[]);
		let err = write_sidecar_atomic(tmp.path(), &payload).unwrap_err();
		assert!(tmp.path().is_dir());
		assert!(err.to_string().contains("directory"));
	}
}
