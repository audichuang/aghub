//! Sources domain service. Extracted from `crates/api/src/routes/sources.rs`
//! so the API and the CLI share one implementation. Fetch + credentials are
//! injected via [`crate::Fetcher`] / [`crate::TokenResolver`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{
	FetchError, FetchSelection, Fetcher, SourceRef, TokenResolution,
	TokenResolver,
};
use aghub_core::models::ResourceScope;
use aghub_core::skills::update::{
	compare_known_hashes, detect_rename, precheck_source, SkillUpdateStatus,
	UncheckableReason,
};

#[derive(Clone, Debug)]
pub enum SourceScope {
	Global,
	Project { root: PathBuf },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceScopeKind {
	Global,
	Project,
}

#[derive(Clone, Debug)]
pub struct SourceSummary {
	pub source: String,
	pub source_url: String,
	pub source_type: String,
	pub scope: SourceScopeKind,
	pub skill_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSkillState {
	NotInstalled,
	InstalledCurrent,
	InstalledOutdated,
	Renamed,
	Removed,
	Deprecated,
	Uncheckable,
}

impl SourceSkillState {
	pub fn as_wire(&self) -> &'static str {
		match self {
			Self::NotInstalled => "notInstalled",
			Self::InstalledCurrent => "installedCurrent",
			Self::InstalledOutdated => "installedOutdated",
			Self::Renamed => "renamed",
			Self::Removed => "removed",
			Self::Deprecated => "deprecated",
			Self::Uncheckable => "uncheckable",
		}
	}
}

#[derive(Clone, Debug)]
pub struct SourceSkillDiff {
	pub name: String,
	pub skill_path: String,
	pub description: Option<String>,
	pub version: Option<String>,
	pub author: Option<String>,
	pub state: SourceSkillState,
	pub previous_name: Option<String>,
	/// Wire reason string (e.g. "noPath", "local"); preserves the DTO `reason`
	/// field and the removed→noPath / uncheckable→reason signals.
	pub reason: Option<String>,
	/// Scope labels where this skill is installed ("global"/"project").
	pub installed_paths: Vec<String>,
	/// RFC 3339 author-time of the upstream tip commit. Set only when the
	/// skill is `InstalledOutdated`; `None` otherwise. Best-effort.
	pub upstream_commit_time: Option<String>,
}

/// skill_path -> installed baseline metadata.
pub(crate) struct BaselineEntry {
	pub installed_name: String,
	pub stored_hash: String,
	pub local_hashes: Vec<String>,
	pub scope_label: String,
	/// The ref THIS entry is pinned to. A Sources row is one repository, but its
	/// entries need not share a branch/tag, and an entry may only be judged
	/// against its own ref's tree — see [`baseline_by_ref`].
	pub ref_name: Option<String>,
}
pub(crate) type Baseline = BTreeMap<String, BaselineEntry>;

/// Split a baseline into one cohort per recorded ref.
///
/// A row's entries share a repository, not a tree. Judging a `v1`-pinned entry
/// against `main`'s tree reports it outdated forever (its apply fetches `v1`, so
/// the hash never converges) or — when its folder does not exist on `main` —
/// `Removed { noPath }`, which the desktop offers to delete in one click.
///
/// The mutation seam already fetches once per `(source, ref)` group; this is the
/// same grouping on the read side.
pub(crate) fn baseline_by_ref(
	baseline: Baseline,
) -> BTreeMap<Option<String>, Baseline> {
	let mut cohorts: BTreeMap<Option<String>, Baseline> = BTreeMap::new();
	for (skill_path, entry) in baseline {
		cohorts
			.entry(entry.ref_name.clone())
			.or_default()
			.insert(skill_path, entry);
	}
	cohorts
}

#[derive(Debug)]
pub enum SourceDiffOutcome {
	/// Flat skill list (API-compatible: merged baseline, classified once).
	/// Carries the resolved `git_ref` (query override → recorded ref → None)
	/// so the API response keeps the old recorded-ref fallback.
	Ok {
		git_ref: Option<String>,
		skills: Vec<SourceSkillDiff>,
	},
	/// Resolved `git_ref` (query override -> recorded ref -> None) -- same as
	/// `Ok`, so the API response keeps the recorded-ref fallback on a
	/// credential miss.
	NeedsCredential {
		git_ref: Option<String>,
	},
	FetchFailed,
	/// Local/ssh/unsupported scheme — known before any fetch. Carries the
	/// resolved git_ref too (the old route returned it on the early-out).
	UncheckableSource {
		git_ref: Option<String>,
		reason: UncheckableReason,
	},
	/// The named source spans more than one forge, so no single fetched tree can
	/// judge it. Refused instead of silently picking one — this diff drives a
	/// one-click "clean up removed" and CLI `source sync --update`, both of which
	/// would act on the other forge's entries. Carries the origins so the caller
	/// can name them.
	AmbiguousSource {
		origins: Vec<String>,
	},
}

pub struct SourceListInput {
	pub scopes: Vec<SourceScope>,
}

pub struct SourceDiffInput {
	pub source: String,
	pub git_ref: Option<String>,
	pub scopes: Vec<SourceScope>,
}

pub struct SourceDiffDeps<'a> {
	pub fetcher: &'a dyn Fetcher,
	pub resolver: &'a dyn TokenResolver,
}

pub fn list_sources(input: SourceListInput) -> Vec<SourceSummary> {
	let mut sources = Vec::new();
	for scope in &input.scopes {
		match scope {
			SourceScope::Global => sources.extend(global_sources()),
			SourceScope::Project { root } => {
				sources.extend(project_sources(root))
			}
		}
	}
	sources
}

/// Group the global lock's skills by source (the global lock carries
/// `sourceUrl`).
fn global_sources() -> Vec<SourceSummary> {
	// Keyed on the repository ORIGIN (`host[:port]/path`), so two forges serving
	// one `owner/repo` are two rows and a row's `sourceUrl` covers every entry in
	// it — which is what lets its diff and its apply resolve to the same
	// repository.
	//
	// The origin is an INTERNAL key only. A row's reported `source` stays the
	// lock's own identifier: it is what every consumer already matches against
	// (credential bindings, the skill list's source groups, `source diff <x>`),
	// and unlike an origin it is a coordinate a caller can feed back. Row
	// UNIQUENESS lives in `source_url`, which is unique per origin by
	// construction.
	//
	// origin -> (lock source, source_url, source_type, count)
	let mut by_origin: BTreeMap<String, (String, String, String, u32)> =
		BTreeMap::new();
	for (_name, entry) in skill::get_all_locked_skills() {
		let origin = source_origin(
			&entry.source,
			Some(&entry.source_url),
			&entry.source_type,
		);
		let agg = by_origin.entry(origin).or_insert_with(|| {
			(
				entry.source.clone(),
				// The global `source_url` is a required String, but nothing
				// validates it as non-empty. Advertising a blank one gave the
				// desktop a truthy-but-unusable coordinate while check and apply
				// reconstructed a real one — the same row disagreeing with
				// itself.
				entry_clone_source(
					&entry.source,
					Some(&entry.source_url),
					&entry.source_type,
				),
				entry.source_type.clone(),
				0,
			)
		});
		agg.3 += 1;
	}
	by_origin
		.into_values()
		.map(
			|(source, source_url, source_type, skill_count)| SourceSummary {
				source,
				source_url,
				source_type,
				scope: SourceScopeKind::Global,
				skill_count,
			},
		)
		.collect()
}

/// Group a project lock's skills by source. Newer locks record `sourceUrl`
/// (so a non-github host survives); older/npx locks omit it, so the fetch URL
/// is reconstructed from `owner/repo` (GitHub etc.).
fn project_sources(root: &Path) -> Vec<SourceSummary> {
	let lock = skill::read_local_lock(Some(root));
	// source (owner/repo) -> (recorded source_url, source_type, count)
	// origin -> (lock source, recorded source_url, source_type, count). See
	// `global_sources` for why the origin stays internal and the reported
	// `source` remains the lock's own identifier.
	let mut by_origin: BTreeMap<String, (String, Option<String>, String, u32)> =
		BTreeMap::new();
	for (_name, entry) in lock.skills {
		let origin = source_origin(
			&entry.source,
			entry.source_url.as_deref(),
			&entry.source_type,
		);
		let agg = by_origin.entry(origin).or_insert_with(|| {
			(
				entry.source.clone(),
				entry.source_url.clone(),
				entry.source_type.clone(),
				0,
			)
		});
		agg.3 += 1;
	}
	by_origin
		.into_values()
		.map(|(source, source_url, source_type, skill_count)| {
			let source_url = entry_clone_source(
				&source,
				source_url.as_deref(),
				&source_type,
			);
			SourceSummary {
				source,
				source_url,
				source_type,
				scope: SourceScopeKind::Project,
				skill_count,
			}
		})
		.collect()
}

/// The clone coordinate a lock entry's content comes from — the ONE answer every
/// consumer must use. `owner/repo` is the shape EVERY forge's lock identifier
/// takes, so whoever resolves it alone reads it as GitHub shorthand: while the
/// Sources row reconstructed a GitLab URL and the bulk apply resolved the raw
/// `group/repo` on its own, applying stamped GitHub's commit into a GitLab
/// entry, silently, whenever a same-path repo existed there.
///
/// Callers that must agree, and only agree because they all call this: the
/// Sources row's advertised `source_url`, the grouping origin, `diff_source`'s
/// fetch, both update-check projections (API and CLI), CLI `source diff`/`sync`,
/// and the bulk apply's `SourceRef`. Hand-mirroring
/// `source_url.unwrap_or(source)` at each of them is what let them drift.
pub fn entry_clone_source(
	source: &str,
	source_url: Option<&str>,
	source_type: &str,
) -> String {
	source_url
		.map(str::trim)
		.filter(|recorded| !recorded.is_empty())
		.map(str::to_string)
		.unwrap_or_else(|| reconstruct_source_url(source, source_type))
}

/// The [`entry_clone_source`] half that has to invent a coordinate, because the
/// entry recorded no `sourceUrl`.
///
/// Only two things can then name a host: the string itself, when it already
/// carries a transport or an authority, and the recorded provider — `github` and
/// `gitlab` are single-host services. A `git`/custom/`local` type keeps its own
/// spelling; inventing a host advertises, and then fetches from, a repository
/// nobody installed from.
pub fn reconstruct_source_url(source: &str, source_type: &str) -> String {
	let source = source.trim();
	if source_type.eq_ignore_ascii_case("local") {
		// A Windows path (`C:\skills\a`) would otherwise read as an SCP-like URL
		// whose host is the drive letter.
		return source.to_string();
	}
	// `://`, `@` or `:` means the string carries its own transport/authority, so
	// the resolver can read the host out of it and the provider is irrelevant.
	let carries_authority =
		source.contains("://") || source.contains('@') || source.contains(':');
	let provider_host = match source_type.to_ascii_lowercase().as_str() {
		"github" => Some("github.com"),
		"gitlab" => Some("gitlab.com"),
		_ => None,
	};
	let candidate = match provider_host {
		Some(host) if !carries_authority => {
			let path = source.trim_matches('/');
			// An origin echoed back from a previous response already carries the
			// host; re-prefixing it would produce `host/host/path`. Hostnames are
			// case-insensitive, so the comparison is too — but it must land on a
			// SEGMENT boundary: `gitlab.comtools/sub/repo` is an owner literally
			// named `gitlab.comtools`, and stripping `gitlab.com` off it names a
			// different repository that may well exist.
			let path = path
				.get(..host.len())
				.filter(|head| head.eq_ignore_ascii_case(host))
				.map(|_| &path[host.len()..])
				// A trailing dot is the DNS root label: `gitlab.com.` names the
				// same host as `gitlab.com`.
				.map(|rest| rest.strip_prefix('.').unwrap_or(rest))
				.filter(|rest| rest.is_empty() || rest.starts_with('/'))
				.map_or(path, |rest| rest.trim_start_matches('/'));
			let path = path.trim_end_matches(".git").trim_matches('/');
			if path.is_empty() {
				return source.to_string();
			}
			format!("https://{host}/{path}.git")
		}
		_ if carries_authority => source.to_string(),
		// A self-hosted forge behind a `git`/custom type: unknowable.
		_ => return source.to_string(),
	};
	aghub_git::resolve_remote_source(&candidate)
		.map(|resolved| resolved.clone_url)
		.unwrap_or_else(|_| source.to_string())
}

/// Fetch a source with the source-scoped token on the first attempt when one is
/// available. Without a token, performs one anonymous attempt. The repository
/// backend owns the gix→system-git tail.
pub fn fetch_source_with_resolver(
	source_ref: &SourceRef,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
	selection: FetchSelection<'_>,
) -> Result<crate::FetchedRepo, FetchError> {
	let token = match resolver.resolve(&source_ref.source) {
		TokenResolution::Token(token) => Some(token),
		TokenResolution::NoToken => None,
		TokenResolution::BackendUnavailable => {
			return Err(FetchError::BackendUnavailable);
		}
	};
	fetcher.fetch(source_ref, token.as_deref(), selection)
}

#[cfg(test)]
mod fetch_with_resolver_tests {
	use std::sync::Mutex;

	use super::*;

	struct RecordingFetcher {
		tokens: Mutex<Vec<Option<String>>>,
	}

	impl Fetcher for RecordingFetcher {
		fn fetch(
			&self,
			_source_ref: &SourceRef,
			token: Option<&str>,
			_selection: FetchSelection<'_>,
		) -> Result<crate::FetchedRepo, FetchError> {
			self.tokens.lock().unwrap().push(token.map(str::to_string));
			Err(FetchError::Network)
		}
	}

	struct StaticResolver;

	impl TokenResolver for StaticResolver {
		fn resolve(&self, _source: &str) -> TokenResolution {
			TokenResolution::Token("configured-token".to_string())
		}
	}

	#[test]
	fn configured_token_is_used_on_the_first_fetch_attempt() {
		let fetcher = RecordingFetcher {
			tokens: Mutex::new(Vec::new()),
		};
		let source = SourceRef {
			source: "https://github.com/acme/private.git".to_string(),
			ref_: Some("main".to_string()),
		};

		let result = fetch_source_with_resolver(
			&source,
			&fetcher,
			&StaticResolver,
			FetchSelection::CatalogSnapshot,
		);

		assert!(matches!(result, Err(FetchError::Network)));
		assert_eq!(
			*fetcher.tokens.lock().unwrap(),
			vec![Some("configured-token".to_string())],
			"a configured token must be used first, with no anonymous round-trip"
		);
	}
}

/// A Source's identity: the repository's ORIGIN — `host[:port]/path`, lower-cased
/// authority, no userinfo, no `.git`. Two forges serving the same path are two
/// Sources; two spellings of one repo (ssh vs https, with or without `.git`) are
/// one. The explicit-transport normalization is shared with the install-time
/// owner check and `EntryIdentity` via `aghub_core`; provider fallback and
/// unresolved-entry identity are intentionally owned here.
///
/// Accepts whatever a caller holds: the recorded `sourceUrl`, an `owner/repo`
/// shorthand, or an origin echoed back from a previous response — all through
/// [`reconstruct_source_url`], the same coordinate apply fetches.
///
/// Unresolvable entries get a `unresolved:<type>:<spelling>` key rather than
/// their bare spelling: a local directory can be named `github.com/owner/repo`
/// (not GitHub shorthand — that is exactly two segments), and the bare spelling
/// would then be byte-identical to a real GitHub entry's origin.
///
/// Some legacy shapes remain undecidable: `sourceType: "git"` (or another
/// custom type) plus no `sourceUrl` cannot reveal which self-hosted forge owns
/// `owner/repo`, and a TFS collection path cannot reveal its server. Two such
/// entries with the same type and spelling still cannot be separated.
pub(crate) fn source_origin(
	entry_source: &str,
	entry_source_url: Option<&str>,
	entry_source_type: &str,
) -> String {
	resolvable_origin(entry_source, entry_source_url, entry_source_type)
		.unwrap_or_else(|| {
			format!(
				"unresolved:{}:{}",
				entry_source_type.trim().to_ascii_lowercase(),
				entry_source.trim()
			)
		})
}

/// `Some` when a host can be derived, `None` for a HOST-BLIND identifier — a
/// lock `source` whose forge is not recoverable from the string alone (a TFS
/// collection path, a local directory). Callers that must not merge two forges
/// need that distinction: an unresolvable spelling cannot separate them, so
/// treating its echo as an origin would compare unlike things.
fn resolvable_origin(
	entry_source: &str,
	entry_source_url: Option<&str>,
	entry_source_type: &str,
) -> Option<String> {
	// Recorded URLs first — they carry the real host. Reconstruction applies the
	// recorded provider to a shorthand. The RAW `entry_source` is
	// deliberately absent: `remote_owner_from_url` reads `host:8443/owner/repo` as
	// an SCP-like URL whose path is `8443/owner/repo`, so feeding it a raw
	// authority-bearing string yields a DIFFERENT origin than the URL it came
	// from — the key would not be idempotent.
	let candidates = [
		entry_source_url.map(str::to_string),
		entry_source_url
			.map(|source| reconstruct_source_url(source, entry_source_type)),
		Some(reconstruct_source_url(entry_source, entry_source_type)),
	];
	candidates.into_iter().flatten().find_map(|candidate| {
		aghub_core::skills::install_fetched::remote_owner_from_url(&candidate)
	})
}

/// The origin a CALLER's `want` names, or `None` when it carries no host.
///
/// Deliberately stricter than [`resolvable_origin`], which reads a bare
/// `owner/repo` as a GitHub coordinate. That reading is right for an ENTRY — an
/// npx-written lock records exactly that and it IS GitHub — but wrong for a
/// `want`: `resolve_remote_source` strips the host when it records `source`, so
/// `owner/repo` is the shape EVERY forge's lock identifier takes, and it is what
/// `aghub source list` prints. Reading it as GitHub made a caller naming a GitLab
/// row select GitHub's tree instead, then report every skill as not-installed and
/// offer to install from that unrelated repository.
fn want_origin(want: &str) -> Option<String> {
	let trimmed = want.trim();
	// No transport and no authority separator ⇒ a host-blind identifier.
	if !trimmed.contains("://")
		&& !trimmed.contains('@')
		&& !trimmed.contains(':')
	{
		return None;
	}
	resolvable_origin(trimmed, None, "git")
}

/// Whether a lock entry belongs to the requested source.
///
/// This is the ONE definition of Source membership: the Sources list groups by
/// it, `diff_source` selects by it, and `mutation.rs` checks a bulk caller's row
/// identity with it. They MUST agree — when the grouping admitted an entry this
/// predicate's own resolution could not, a row's diff judged against one
/// repository while its apply installed from another, and a stricter check in
/// `mutation.rs` rejected rows the caller was correctly shown.
pub(crate) fn source_matches(
	want: &str,
	entry_source: &str,
	entry_source_url: Option<&str>,
	entry_source_type: &str,
) -> bool {
	if entry_source_url == Some(want) {
		return true;
	}
	let entry =
		source_origin(entry_source, entry_source_url, entry_source_type);
	match want_origin(want) {
		// `want` names a host, so compare exactly — this is what stops a row from
		// admitting another forge serving the same path. A bare
		// `entry_source == want` would not: the lock's identifier is host-blind.
		Some(origin) => origin == entry,
		// `want` is host-blind (a TFS collection path, a local directory). It
		// cannot separate forges, and the caller has given nothing finer, so fall
		// back to the identifier the lock itself records.
		None => entry_source == want,
	}
}

/// Local content hashes for every installed copy of `name`, against an
/// ALREADY-loaded agent set. Taking the set rather than a scope is what keeps
/// the caller's per-entry loop off the agent scan: that scan re-reads every
/// registered agent's config from disk and does not vary by name, so hoisting
/// it turns an O(entries x agents) sweep into one load per scope.
fn local_hashes_for_installed(
	agents: &[aghub_core::AgentResources],
	name: &str,
) -> Vec<String> {
	aghub_core::skills::removal::installed_skill_roots_in(agents, name)
		.into_iter()
		.filter_map(|root| skill::compute_skill_folder_hash(&root).ok())
		.collect()
}

/// Insert one scope's lock entries into a shared baseline. Reused by the merged
/// (API) and single-scope (CLI) baseline builders so the logic stays DRY. On a
/// duplicate `skill_path` the LAST inserted scope wins (the merged path inserts
/// global first, then project — so project shadows global).
fn insert_scope_entries(
	baseline: &mut Baseline,
	source_type: &mut String,
	recorded_ref: &mut Option<String>,
	scope: &SourceScope,
	want: &str,
) {
	// The agent scan is loaded at most ONCE per scope, and only if a matching
	// entry is actually reached: a source whose rows all belong to another scope
	// must not pay for a scan it never reads.
	let mut agents: Option<Vec<aghub_core::AgentResources>> = None;
	match scope {
		SourceScope::Global => {
			for (name, entry) in skill::get_all_locked_skills() {
				if !source_matches(
					want,
					&entry.source,
					Some(&entry.source_url),
					&entry.source_type,
				) {
					continue;
				}
				if source_type.is_empty() {
					*source_type = entry.source_type.clone();
				}
				if recorded_ref.is_none() {
					*recorded_ref = entry.ref_name.clone();
				}
				if let Some(skill_path) = entry.skill_path.clone() {
					let hash = entry.content_hash.clone().unwrap_or_default();
					let agents = agents.get_or_insert_with(|| {
						crate::mutation::scan_agents(
							ResourceScope::GlobalOnly,
							None,
						)
					});
					let local_hashes =
						local_hashes_for_installed(agents, &name);
					baseline.insert(
						skill_path,
						BaselineEntry {
							installed_name: name,
							stored_hash: hash,
							local_hashes,
							scope_label: "global".to_string(),
							ref_name: entry.ref_name.clone(),
						},
					);
				}
			}
		}
		SourceScope::Project { root } => {
			for (name, entry) in skill::read_local_lock(Some(root)).skills {
				if !source_matches(
					want,
					&entry.source,
					entry.source_url.as_deref(),
					&entry.source_type,
				) {
					continue;
				}
				if source_type.is_empty() {
					*source_type = entry.source_type.clone();
				}
				if recorded_ref.is_none() {
					*recorded_ref = entry.ref_name.clone();
				}
				if let Some(skill_path) = entry.skill_path.clone() {
					let agents = agents.get_or_insert_with(|| {
						crate::mutation::scan_agents(
							ResourceScope::ProjectOnly,
							Some(root),
						)
					});
					let local_hashes =
						local_hashes_for_installed(agents, &name);
					baseline.insert(
						skill_path,
						BaselineEntry {
							installed_name: name,
							stored_hash: entry.computed_hash,
							local_hashes,
							scope_label: "project".to_string(),
							ref_name: entry.ref_name.clone(),
						},
					);
				}
			}
		}
	}
}

/// API path: merge global + every project scope into ONE baseline keyed by
/// `skill_path` (project shadows global on a duplicate), and classify once.
/// Returns `(baseline, source_type, recorded_ref)`.
pub(crate) fn merged_baseline_for_source(
	scopes: &[SourceScope],
	source: &str,
) -> (Baseline, String, Option<String>) {
	let mut baseline = Baseline::new();
	let mut source_type = String::new();
	let mut recorded_ref: Option<String> = None;
	let want = source.trim();
	// Global first, then project, so project entries shadow global on a
	// duplicate `skill_path` (mirrors the old route's insert order).
	for scope in scopes {
		if matches!(scope, SourceScope::Global) {
			insert_scope_entries(
				&mut baseline,
				&mut source_type,
				&mut recorded_ref,
				scope,
				want,
			);
		}
	}
	for scope in scopes {
		if matches!(scope, SourceScope::Project { .. }) {
			insert_scope_entries(
				&mut baseline,
				&mut source_type,
				&mut recorded_ref,
				scope,
				want,
			);
		}
	}
	(baseline, source_type, recorded_ref)
}

/// CLI path: build the baseline for a SINGLE scope only.
/// Returns `(baseline, source_type, recorded_ref)`.
pub(crate) fn baseline_for_scope(
	scope: &SourceScope,
	source: &str,
) -> (Baseline, String, Option<String>) {
	let mut baseline = Baseline::new();
	let mut source_type = String::new();
	let mut recorded_ref: Option<String> = None;
	insert_scope_entries(
		&mut baseline,
		&mut source_type,
		&mut recorded_ref,
		scope,
		source.trim(),
	);
	(baseline, source_type, recorded_ref)
}

/// Discover only the recorded `source_type` + `ref_name` for a source across
/// the given scopes, WITHOUT building a baseline (no folder hashing, no fetch).
/// Mirrors the merged-baseline scan order (global first, then project) so the
/// "first non-empty wins" result matches [`merged_baseline_for_source`] — and,
/// for the fetch coordinate, matches the row `list_sources` advertises, which is
/// first-wins too. Preferring an HTTPS spelling here instead would diverge from
/// both, and with a host-blind `want` it could swap in another forge's URL.
/// Returns `(source_type, recorded_ref, recorded_source_url)` — all empty/None
/// when the source is not present in any lock. `recorded_source_url` is the
/// matching entry's recorded clone URL (the fetch coordinate), so a caller that
/// was handed a host-stripped `owner/repo` can recover the real non-github host
/// (TFS/Azure DevOps) instead of reconstructing `github.com`.
fn recorded_meta_for_source(
	scopes: &[SourceScope],
	source: &str,
) -> (String, Option<String>, Option<String>, Vec<String>) {
	let mut source_type = String::new();
	let mut recorded_ref: Option<String> = None;
	let mut recorded_source_url: Option<String> = None;
	let mut matched_origins: std::collections::BTreeSet<String> =
		std::collections::BTreeSet::new();
	let want = source.trim();
	let mut visit = |entry_source: &str,
	                 entry_source_url: Option<&str>,
	                 entry_source_type: &str,
	                 entry_ref: Option<&str>| {
		if !source_matches(
			want,
			entry_source,
			entry_source_url,
			entry_source_type,
		) {
			return;
		}
		matched_origins.insert(source_origin(
			entry_source,
			entry_source_url,
			entry_source_type,
		));
		if source_type.is_empty() {
			source_type = entry_source_type.to_string();
		}
		if recorded_ref.is_none() {
			recorded_ref = entry_ref.map(str::to_string);
		}
		if recorded_source_url.is_none() {
			// Reconstructed, not merely recorded: `diff_source` and CLI
			// `source diff`/`sync` fetch this, and falling back to the caller's
			// raw `group/repo` would send them to GitHub while `list_sources` and
			// apply went to the entry's real forge.
			recorded_source_url = Some(entry_clone_source(
				entry_source,
				entry_source_url,
				entry_source_type,
			));
		}
	};
	// Global first, then project — same order as `merged_baseline_for_source`.
	for scope in scopes {
		if matches!(scope, SourceScope::Global) {
			for (_name, entry) in skill::get_all_locked_skills() {
				visit(
					&entry.source,
					Some(&entry.source_url),
					&entry.source_type,
					entry.ref_name.as_deref(),
				);
			}
		}
	}
	for scope in scopes {
		if let SourceScope::Project { root } = scope {
			for (_name, entry) in skill::read_local_lock(Some(root)).skills {
				visit(
					&entry.source,
					entry.source_url.as_deref(),
					&entry.source_type,
					entry.ref_name.as_deref(),
				);
			}
		}
	}
	(
		source_type,
		recorded_ref,
		recorded_source_url,
		matched_origins.into_iter().collect(),
	)
}

/// The resolved fetch metadata for a source, derived from the lock entries
/// (NO fetch). Lets the CLI — which fetches once itself — agree with the API
/// `diff_source` on `(source_type, effective_ref)` BEFORE the single fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSourceMeta {
	/// The lock's recorded source type, defaulted to `"github"` when empty —
	/// the same default `diff_source` applies before its `precheck_source`.
	pub source_type: String,
	/// Explicit `--ref` override, else the source's recorded lock ref, else
	/// `None` (the upstream default branch).
	pub effective_ref: Option<String>,
	/// The matching lock entry's [`entry_clone_source`] — its recorded clone URL,
	/// or the coordinate reconstructed from `(source, source_type)` when it
	/// recorded none. Lets a caller handed a host-stripped `owner/repo` (e.g.
	/// `source diff <lock source>`) fetch the entry's real forge instead of
	/// reconstructing `github.com`.
	///
	/// `None` means ONLY that no lock entry matched — not "the entry recorded no
	/// URL". A matching entry always yields `Some`, so the diff resolves the same
	/// repository the row advertises and the apply installs from.
	pub effective_source: Option<String>,
	/// Every DISTINCT origin the caller's source selected, sorted. More than one
	/// means the caller named something host-blind that spans forges — see
	/// [`ResolvedSourceMeta::ambiguous_origins`].
	pub matched_origins: Vec<String>,
}

impl ResolvedSourceMeta {
	/// The origins a host-blind source spans, when it spans more than one.
	///
	/// `aghub source list` prints the lock's own host-blind `SOURCE`, and
	/// `source_matches` falls back to `entry_source == want` for it — so pasting
	/// that column back selects EVERY forge serving the same `owner/repo`. The
	/// caller then fetches ONE of them and judges (or overwrites) all of them
	/// against that single tree.
	///
	/// Only flows that apply one fetched tree to many entries need this. The bulk
	/// apply does not: each row fetches its own entry's coordinate, so a
	/// host-blind group merely admits more rows, each still updated from its own
	/// forge.
	pub fn ambiguous_origins(&self) -> Option<&[String]> {
		(self.matched_origins.len() > 1).then_some(&self.matched_origins[..])
	}
}

/// Resolve `(source_type, effective_ref)` for a source across `scopes` from the
/// lock entries, with NO fetch. This is the SINGLE resolution path: the API
/// [`diff_source`] calls it internally, and the CLI calls it before its own
/// fetch so both surfaces resolve identically.
///
/// `effective_ref = explicit_ref OR recorded lock ref OR None`; `source_type`
/// is the lock's recorded type, defaulted to `"github"` when empty.
pub fn resolve_source_meta(
	source: &str,
	scopes: &[SourceScope],
	explicit_ref: Option<&str>,
) -> ResolvedSourceMeta {
	let (mut source_type, recorded_ref, recorded_source_url, matched_origins) =
		recorded_meta_for_source(scopes, source);
	if source_type.is_empty() {
		source_type = "github".to_string();
	}
	let effective_ref = explicit_ref.map(str::to_string).or(recorded_ref);
	ResolvedSourceMeta {
		source_type,
		effective_ref,
		effective_source: recorded_source_url,
		matched_origins,
	}
}

/// Internal: classify discovered repo skills against a prebuilt baseline.
/// `Baseline`/`BaselineEntry` stay `pub(crate)` so they never leak across the
/// crate boundary; cross-crate callers use [`classify_scope`] / [`diff_source`].
///
/// Discovery (`skill::discover_repo_skills`) happens here so callers pass only
/// `root`. A repo with no discoverable skills yields the baseline-only
/// `removed` rows (matching the old route's empty-discovery early-return, which
/// produced an empty diff before the changelog/removed pass ran on an empty
/// discovered set).
pub(crate) fn classify_repo_skills(
	root: &Path,
	baseline: &Baseline,
	upstream_commit_time: Option<String>,
) -> Vec<SourceSkillDiff> {
	let discovered = match skill::discover_repo_skills(root, &[], true) {
		Ok(discovered) => discovered,
		// An upstream that carries no SKILL.md at all is a legitimate diff
		// input, not a failure: every baseline entry belongs in the removed
		// pass. Returning early here (as the old route did) hid the deletion of
		// a source's LAST skill entirely — the caller saw an empty diff.
		Err(skill::RepoDiscoveryError::NoSkillsFound) => Vec::new(),
		// A scan / relative-path failure says nothing about upstream content.
		// Reporting it as a wholesale deletion would be worse than silence, so
		// it keeps yielding an empty diff.
		Err(_) => return Vec::new(),
	};
	build_source_skill_diffs(root, discovered, baseline, upstream_commit_time)
}

fn build_source_skill_diffs(
	root: &Path,
	discovered: Vec<skill::RepoDiscoveredSkill>,
	baseline: &Baseline,
	upstream_commit_time: Option<String>,
) -> Vec<SourceSkillDiff> {
	use std::collections::BTreeSet;
	let mut out = Vec::with_capacity(discovered.len() + baseline.len());
	let mut seen_paths = BTreeSet::new();
	let mut indexes_by_name = BTreeMap::new();
	// Frontmatter names are not unique across skill paths; a name seen more
	// than once is ambiguous and must not be used as a rename redirect target.
	let mut ambiguous_names = BTreeSet::new();
	for d in discovered {
		let skill_path = skill::lock_skill_file_path(&d.relative_dir);
		seen_paths.insert(skill_path.clone());
		let (description, version, author) = parse_meta(&d.full_path);
		let name = d.name;

		match baseline.get(&skill_path) {
			None => {
				let (state, reason) = if is_deprecated_skill_path(&skill_path) {
					(SourceSkillState::Deprecated, None)
				} else {
					(SourceSkillState::NotInstalled, None)
				};
				if indexes_by_name.insert(name.clone(), out.len()).is_some() {
					ambiguous_names.insert(name.clone());
				}
				out.push(SourceSkillDiff {
					name,
					skill_path,
					description,
					version,
					author,
					state,
					previous_name: None,
					reason,
					installed_paths: Vec::new(),
					upstream_commit_time: None,
				});
			}
			Some(entry) => {
				let skill_dir =
					aghub_core::skills::skill_source_root(&d.full_path);
				let (state, previous_name, reason) =
					classify_source_skill_diff(entry, &name, &skill_dir);
				if indexes_by_name.insert(name.clone(), out.len()).is_some() {
					ambiguous_names.insert(name.clone());
				}
				// The upstream commit time is meaningful only for an
				// out-of-date install ("updated N days ago"); all other states
				// leave it `None`.
				let commit_time =
					if state == SourceSkillState::InstalledOutdated {
						upstream_commit_time.clone()
					} else {
						None
					};
				out.push(SourceSkillDiff {
					name,
					skill_path,
					description,
					version,
					author,
					state,
					previous_name,
					reason,
					installed_paths: vec![entry.scope_label.clone()],
					upstream_commit_time: commit_time,
				});
			}
		}
	}

	let successors = skill_successors_from_changelog(root);
	for (skill_path, entry) in baseline {
		if seen_paths.contains(skill_path) {
			continue;
		}
		if let Some(new_name) = successors.get(&entry.installed_name) {
			if !ambiguous_names.contains(new_name) {
				if let Some(index) = indexes_by_name.get(new_name) {
					let diff = &mut out[*index];
					// Only claim a not-yet-installed, non-deprecated successor.
					// A deprecated target keeps its signal; an already-installed
					// target means the old name is a stale leftover (→ removed).
					if diff.installed_paths.is_empty()
						&& diff.state != SourceSkillState::Deprecated
					{
						diff.state = SourceSkillState::Renamed;
						diff.previous_name = Some(entry.installed_name.clone());
						diff.reason = None;
						diff.installed_paths = vec![entry.scope_label.clone()];
						continue;
					}
				}
			}
		}
		out.push(SourceSkillDiff {
			name: entry.installed_name.clone(),
			skill_path: skill_path.clone(),
			description: None,
			version: None,
			author: None,
			state: SourceSkillState::Removed,
			previous_name: None,
			reason: Some("noPath".to_string()),
			installed_paths: vec![entry.scope_label.clone()],
			upstream_commit_time: None,
		});
	}

	out
}

fn skill_successors_from_changelog(root: &Path) -> BTreeMap<String, String> {
	let Ok(content) = std::fs::read_to_string(root.join("CHANGELOG.md")) else {
		return BTreeMap::new();
	};
	let mut successors = BTreeMap::new();
	for line in content.lines() {
		for (old_name, new_name) in skill_renames_in_line(line) {
			successors.entry(old_name).or_insert(new_name);
		}
	}
	successors
}

/// Extract `(old, new)` skill-rename pairs from a single changelog line.
///
/// Anchored on the `rename`/`replace` verb and its connective, not on the last
/// two backtick terms, so trailing tokens (commit/PR refs, parentheticals)
/// cannot hijack the new name and multiple renames on one line are all
/// captured. For each verb occurrence we search only within its clause (up to
/// the next `;`/`.`) for the connective `… to …` / `… with …`; OLD is the
/// backtick term that lies between the verb and the connective, NEW is the
/// first backtick term opening at/after the connective; direction is fixed
/// (old → new). A verb or connective that falls inside a backtick term, or a
/// clause whose terms do not bracket the connective, yields nothing — so a
/// connective appearing inside an unrelated backticked token (`` `x to y` ``)
/// or in a different clause cannot fabricate a pair. This mirrors the upstream
/// "Rename the `X` skill to `Y`" changelog convention; the less common
/// "`X` was renamed to `Y`" phrasing degrades to no-pair (the lock entry is
/// then reported `removed` rather than a wrong rename). Spurious pairs are
/// further filtered by the caller, which only applies a successor whose OLD
/// matches a removed lock entry and whose NEW resolves to a discovered skill.
fn skill_renames_in_line(line: &str) -> Vec<(String, String)> {
	let lower = line.to_ascii_lowercase();
	let spans = backtick_spans(line);
	if spans.len() < 2 {
		return Vec::new();
	}
	// `to_ascii_lowercase` preserves byte length, so positions in `lower` line
	// up with byte positions in `line` / `spans`. Backticks/connectives are
	// ASCII, so every index used here is a UTF-8 char boundary.
	let inside_span =
		|pos: usize| spans.iter().any(|(s, e, _)| *s < pos && pos < *e);
	let mut out = Vec::new();
	for (verb, connective) in [("rename", " to "), ("replace", " with ")] {
		let mut from = 0;
		while let Some(rel) = lower[from..].find(verb) {
			let verb_start = from + rel;
			let after_verb = verb_start + verb.len();
			from = after_verb;
			if inside_span(verb_start) {
				continue;
			}
			// Bound the connective search to this clause so a connective in a
			// later clause cannot bind to this verb.
			let clause_end = lower[after_verb..]
				.find([';', '.'])
				.map(|r| after_verb + r)
				.unwrap_or(lower.len());
			let Some(conn_rel) = lower[after_verb..clause_end].find(connective)
			else {
				continue;
			};
			let conn_start = after_verb + conn_rel;
			let conn_end = conn_start + connective.len();
			if inside_span(conn_start) {
				continue;
			}
			let old = spans
				.iter()
				.rfind(|(s, e, _)| *s >= verb_start && *e <= conn_start);
			let new = spans.iter().find(|(s, _, _)| *s >= conn_end);
			if let (Some((_, _, old_name)), Some((_, _, new_name))) = (old, new)
			{
				if old_name != new_name {
					out.push((old_name.clone(), new_name.clone()));
				}
			}
		}
	}
	out
}

/// Byte spans `(start, end, term)` for each backtick-delimited term, where
/// `start` is the opening backtick index and `end` is one past the closing
/// backtick. Empty/whitespace-only terms are skipped.
fn backtick_spans(line: &str) -> Vec<(usize, usize, String)> {
	let mut spans = Vec::new();
	let mut i = 0;
	while i < line.len() {
		if line.as_bytes()[i] == b'`' {
			let Some(rel) = line[i + 1..].find('`') else {
				break;
			};
			let close = i + 1 + rel;
			let term = line[i + 1..close].trim();
			if !term.is_empty() {
				spans.push((i, close + 1, term.to_string()));
			}
			i = close + 1;
		} else {
			i += 1;
		}
	}
	spans
}

fn is_deprecated_skill_path(skill_path: &str) -> bool {
	skill_path.split('/').any(|part| part == "deprecated")
}

fn classify_source_skill_diff(
	entry: &BaselineEntry,
	discovered_name: &str,
	skill_dir: &Path,
) -> (SourceSkillState, Option<String>, Option<String>) {
	if detect_rename(discovered_name, &entry.installed_name).is_some() {
		return (
			SourceSkillState::Renamed,
			Some(entry.installed_name.clone()),
			None,
		);
	}
	let (state, reason) = classify_installed(entry, skill_dir);
	(state, None, reason)
}

/// Classify an already-installed skill by comparing its upstream folder hash to
/// the installed baseline. Prefer actual installed folder hashes over the
/// stored lock hash because some locks were produced by npx/JS collation, while
/// this endpoint hashes fetched source with Rust collation. Comparing local
/// Rust hashes to fetched Rust hash avoids false updates for unchanged skills.
fn classify_installed(
	entry: &BaselineEntry,
	skill_dir: &Path,
) -> (SourceSkillState, Option<String>) {
	let fresh = match skill::compute_skill_folder_hash(skill_dir) {
		Ok(hash) => hash,
		Err(_) => {
			return (SourceSkillState::Uncheckable, Some("local".to_string()))
		}
	};

	if !entry.local_hashes.is_empty() {
		if entry.local_hashes.iter().all(|hash| {
			compare_known_hashes(hash, &fresh, None)
				== SkillUpdateStatus::UpToDate
		}) {
			return (SourceSkillState::InstalledCurrent, None);
		}
		return (SourceSkillState::InstalledOutdated, None);
	}

	let baseline = if entry.stored_hash.is_empty()
		|| skill::is_placeholder_digest(&entry.stored_hash)
	{
		None
	} else {
		Some(entry.stored_hash.as_str())
	};
	let Some(base_hash) = baseline else {
		return (SourceSkillState::InstalledCurrent, None);
	};

	// The rename path short-circuits in `classify_source_skill_diff` before
	// reaching this function (it inspects `entry.installed_name` vs the
	// discovered name up front). `compare_known_hashes` itself never returns
	// `Renamed` — it only yields `UpToDate` or `UpdateAvailable` — so the
	// `Renamed` arm is unreachable here and intentionally omitted.
	match compare_known_hashes(base_hash, &fresh, None) {
		SkillUpdateStatus::UpToDate => {
			(SourceSkillState::InstalledCurrent, None)
		}
		SkillUpdateStatus::UpdateAvailable { .. } => {
			(SourceSkillState::InstalledOutdated, None)
		}
		SkillUpdateStatus::Uncheckable { reason } => {
			(SourceSkillState::Uncheckable, Some(reason_str(reason)))
		}
		SkillUpdateStatus::Renamed { .. } => unreachable!(
			"compare_known_hashes cannot return Renamed; rename detection \
			 happens in classify_source_skill_diff before this match"
		),
	}
}

fn parse_meta(
	skill_md: &Path,
) -> (Option<String>, Option<String>, Option<String>) {
	match skill::parse(skill_md) {
		Ok(s) => (
			Some(s.description).filter(|d| !d.is_empty()),
			s.version,
			s.author,
		),
		Err(_) => (None, None, None),
	}
}

fn reason_str(reason: UncheckableReason) -> String {
	match reason {
		UncheckableReason::Auth => "auth",
		UncheckableReason::Network => "network",
		UncheckableReason::Local => "local",
		UncheckableReason::Ssh => "ssh",
		UncheckableReason::UnsupportedScheme => "unsupportedScheme",
		UncheckableReason::NoPath => "noPath",
		UncheckableReason::Timeout => "timeout",
	}
	.to_string()
}

/// PUBLIC CLI entry: build the baseline for one scope and classify the fetched
/// repo against it. Does NOT fetch (caller passes the already-fetched `root`),
/// so the CLI reuses one `FetchedRepo` for every scope and for install.
pub fn classify_scope(
	root: &Path,
	scope: &SourceScope,
	source: &str,
	upstream_commit_time: Option<String>,
) -> Vec<SourceSkillDiff> {
	let (baseline, _src_type, _ref) = baseline_for_scope(scope, source);
	classify_repo_skills(root, &baseline, upstream_commit_time)
}

/// The distinct refs the entries of `source` in `scope` are pinned to, sorted.
///
/// [`classify_scope`] judges a whole scope against ONE already-fetched tree, so
/// a caller holding a single tree must first know whether that is sound. More
/// than one ref means it is not: `diff_source`, which owns its fetches, splits
/// into cohorts instead ([`baseline_by_ref`]).
pub fn scope_ref_cohorts(
	scope: &SourceScope,
	source: &str,
) -> Vec<Option<String>> {
	let (baseline, _src_type, _ref) = baseline_for_scope(scope, source);
	baseline_by_ref(baseline).into_keys().collect()
}

/// PUBLIC API entry: merged-baseline, single-classification, flat output —
/// byte-identical to the old route. Fetches internally via `deps`.
pub fn diff_source(
	input: SourceDiffInput,
	deps: SourceDiffDeps<'_>,
) -> SourceDiffOutcome {
	let source = input.source.trim().to_string();
	// A caller may hand us a clone URL that still carries userinfo, and these
	// lines land in a log file the user exports and shares. Redact ONCE here so
	// no later log line has to remember to.
	let source_log = aghub_git::redact_url_userinfo(&source);
	let diff_started = std::time::Instant::now();
	let baseline_started = std::time::Instant::now();
	let (baseline, _src_type, _recorded_ref) =
		merged_baseline_for_source(&input.scopes, &source);
	// The baseline hashes every installed copy of every matching entry, so it is
	// local-disk work that scales with the source's row count — report it apart
	// from the fetch, or a slow diff cannot be attributed to disk vs network.
	log::info!(
		"source diff [{source_log}]: baseline entries={} took={:?}",
		baseline.len(),
		baseline_started.elapsed()
	);

	// Resolve `(source_type, effective_ref)` via the SHARED helper so the API
	// and CLI agree on the fetch coordinate. `effective_ref` = explicit query
	// ref, else the source's RECORDED ref, else None (the repo default branch).
	let meta =
		resolve_source_meta(&source, &input.scopes, input.git_ref.as_deref());
	if let Some(origins) = meta.ambiguous_origins() {
		return SourceDiffOutcome::AmbiguousSource {
			origins: origins.to_vec(),
		};
	}
	let git_ref = meta.effective_ref;
	// Recover the recorded clone URL so a caller who passed the host-stripped
	// lock `source` (owner/repo) still fetches a non-github host correctly.
	let fetch_source = meta.effective_source.unwrap_or_else(|| source.clone());

	// Skip sources we cannot fetch (local/ssh/unsupported) up front.
	if let Some(reason) = precheck_source(&meta.source_type, &fetch_source) {
		return SourceDiffOutcome::UncheckableSource { git_ref, reason };
	}

	// One cohort per recorded ref, so no entry is judged against a tree it was
	// never installed from. An explicit `?ref=` is the caller asking "what would
	// this ref give me?", so it collapses every entry into one cohort by design.
	//
	// The PRIMARY cohort (the row's `effective_ref`) also contributes the
	// not-installed offers: those describe what is available to install, and an
	// install goes to the row's ref. A secondary cohort contributes verdicts for
	// its own entries only.
	let cohorts: Vec<(Option<String>, Baseline)> = if input.git_ref.is_some() {
		vec![(git_ref.clone(), baseline)]
	} else {
		let mut by_ref = baseline_by_ref(baseline);
		let primary = by_ref.remove(&git_ref).unwrap_or_default();
		std::iter::once((git_ref.clone(), primary))
			.chain(by_ref)
			.collect()
	};

	// Verdicts about installed entries, from every cohort; and what the primary
	// tree offers to install. The offers are filtered LAST, against the paths the
	// verdicts actually claim — not against the baseline's paths, because a
	// Renamed verdict carries the NEW path, which no baseline ever had. Filtering
	// on the baseline let the primary offer `new/SKILL.md` while a secondary
	// reported `old → new` for the same path: two contradictory rows.
	let mut verdicts = Vec::new();
	let mut primary_offers = Vec::new();
	for (index, (cohort_ref, cohort)) in cohorts.into_iter().enumerate() {
		let is_primary = index == 0;
		if !is_primary && cohort.is_empty() {
			continue;
		}
		let source_ref = SourceRef {
			source: fetch_source.clone(),
			ref_: cohort_ref,
		};
		let fetch_started = std::time::Instant::now();
		let fetched = fetch_source_with_resolver(
			&source_ref,
			deps.fetcher,
			deps.resolver,
			FetchSelection::CatalogSnapshot,
		);
		log::info!(
			"source diff [{source_log}]: cohort {} ref={:?} fetch {} took={:?}",
			index,
			source_ref.ref_,
			match &fetched {
				Ok(_) => "ok",
				Err(FetchError::Auth) => "auth-failed",
				Err(FetchError::BackendUnavailable) => "backend-unavailable",
				Err(FetchError::Network) => "network-failed",
			},
			fetch_started.elapsed()
		);
		let repo = match fetched {
			Ok(repo) => repo,
			// A cohort that cannot be fetched fails the WHOLE diff rather than
			// dropping its entries or judging them against another ref's tree —
			// the same answer the row already gives when its only fetch fails.
			// A silently partial view is what "clean up removed" acts on.
			Err(FetchError::BackendUnavailable) => {
				return SourceDiffOutcome::UncheckableSource {
					git_ref,
					reason: UncheckableReason::Network,
				};
			}
			Err(FetchError::Auth) => {
				return SourceDiffOutcome::NeedsCredential { git_ref };
			}
			Err(FetchError::Network) => return SourceDiffOutcome::FetchFailed,
		};
		let classify_started = std::time::Instant::now();
		let classified = classify_repo_skills(
			repo.root.as_path(),
			&cohort,
			repo.upstream_commit_time(),
		);
		log::info!(
			"source diff [{source_log}]: cohort {} classify skills={} took={:?}",
			index,
			classified.len(),
			classify_started.elapsed()
		);
		for diff in classified {
			// A cohort speaks about ITS OWN installed entries — `installed_paths`
			// is what marks one, and it survives a rename's path change.
			if !diff.installed_paths.is_empty() {
				verdicts.push(diff);
			} else if is_primary {
				primary_offers.push(diff);
			}
		}
	}

	let claimed: std::collections::BTreeSet<&str> = verdicts
		.iter()
		.map(|diff| diff.skill_path.as_str())
		.collect();
	let mut skills = Vec::with_capacity(verdicts.len() + primary_offers.len());
	primary_offers.retain(|diff| !claimed.contains(diff.skill_path.as_str()));
	skills.extend(verdicts);
	skills.extend(primary_offers);

	log::info!(
		"source diff [{source_log}]: done skills={} total={:?}",
		skills.len(),
		diff_started.elapsed()
	);
	SourceDiffOutcome::Ok { git_ref, skills }
}

#[cfg(test)]
mod origin_tests {
	use super::*;

	/// Every spelling of one repository must collapse to one origin, and every
	/// distinct forge must stay separate — this is what a Sources row IS.
	#[test]
	fn one_repo_many_spellings_is_one_origin() {
		let canonical = "github.com/owner/repo";
		for (source, source_url) in [
			("owner/repo", Some("https://github.com/owner/repo.git")),
			("owner/repo", Some("https://github.com/owner/repo")),
			("owner/repo", Some("git@github.com:owner/repo.git")),
			("owner/repo", Some("ssh://git@github.com/owner/repo.git")),
			(
				"owner/repo",
				Some("https://token@github.com/owner/repo.git"),
			),
			// npx-written project lock: no `sourceUrl`, the shorthand IS the
			// GitHub coordinate.
			("owner/repo", None),
			// An origin echoed back from a previous response.
			("github.com/owner/repo", None),
		] {
			assert_eq!(
				source_origin(source, source_url, "github"),
				canonical,
				"source={source} url={source_url:?}"
			);
		}
	}

	#[test]
	fn a_different_forge_or_port_is_a_different_origin() {
		let github = source_origin(
			"owner/repo",
			Some("https://github.com/owner/repo.git"),
			"github",
		);
		for other in [
			"https://gitlab.com/owner/repo.git",
			"https://github.example.com/owner/repo.git",
			// Port matters: two forges on one host are two repositories, which
			// is why a host-only comparison is too loose (same reason
			// credential origin pinning includes the port).
			"https://github.com:8443/owner/repo.git",
		] {
			assert_ne!(
				source_origin("owner/repo", Some(other), "git"),
				github,
				"{other} must not collapse into {github}"
			);
		}
	}

	/// The value `source list` PRINTS must select the row it came from. For every
	/// non-GitHub HTTPS source that value is a bare `owner/repo`, because
	/// `resolve_remote_source` strips the host when it records `source` — so
	/// reading a want-side shorthand as GitHub silently pointed the caller at
	/// github.com, reported every skill as not-installed, and offered to install
	/// from that unrelated repository.
	#[test]
	fn a_host_blind_want_selects_its_own_entry_on_any_forge() {
		for url in [
			"https://gitlab.com/owner/repo.git",
			"https://git.example.com:8443/owner/repo.git",
			"https://github.com/owner/repo.git",
		] {
			assert!(
				source_matches("owner/repo", "owner/repo", Some(url), "git",),
				"the printed identifier must still select {url}"
			);
		}
		// A want that DOES carry a host stays exact — that is what keeps one row
		// from admitting another forge serving the same path.
		assert!(!source_matches(
			"https://github.com/owner/repo.git",
			"owner/repo",
			Some("https://gitlab.com/owner/repo.git"),
			"gitlab",
		));
	}

	/// A local/unresolvable source keeps its own spelling, so two distinct local
	/// directories never merge into one row.
	#[test]
	fn unresolvable_sources_keep_their_spelling() {
		assert_eq!(
			source_origin("/opt/skills/a", None, "local"),
			"unresolved:local:/opt/skills/a"
		);
		assert_ne!(
			source_origin("/opt/skills/a", None, "local"),
			source_origin("/opt/skills/b", None, "local")
		);
	}

	/// Reconstruction may only produce a coordinate that names the SAME
	/// repository the entry records. A shape it cannot place must keep its own
	/// spelling — inventing one advertises, and then fetches from, a repository
	/// nobody installed from.
	#[test]
	fn reconstruction_never_invents_a_repository() {
		for (source, source_type, expected) in [
			// A provider names its host; the path is whatever remains.
			("owner/repo", "github", "https://github.com/owner/repo.git"),
			("group/repo", "gitlab", "https://gitlab.com/group/repo.git"),
			(
				"group/sub/repo",
				"gitlab",
				"https://gitlab.com/group/sub/repo.git",
			),
			// An origin echoed back already carries the host — in any case, and
			// with or without `.git`.
			(
				"github.com/owner/repo",
				"github",
				"https://github.com/owner/repo.git",
			),
			(
				"GitHub.com/owner/repo.git",
				"github",
				"https://github.com/owner/repo.git",
			),
			// A host with nothing after it names no repository.
			("gitlab.com", "gitlab", "gitlab.com"),
			("github.com/", "github", "github.com/"),
			// An owner whose name merely STARTS with the host is not an echoed
			// origin. Stripping the prefix here names a different repository —
			// one that may exist.
			(
				"gitlab.comtools/sub/repo",
				"gitlab",
				"https://gitlab.com/gitlab.comtools/sub/repo.git",
			),
			// …but a trailing dot IS the same host (the DNS root label), so the
			// boundary check must not treat it as part of the owner name.
			(
				"gitlab.com./group/repo",
				"gitlab",
				"https://gitlab.com/group/repo.git",
			),
			("gitlab.com.", "gitlab", "gitlab.com."),
			// No provider ⇒ no host to be had. `segments >= 3` once turned this
			// into `https://group/subgroup/repo`, a host that does not exist.
			("group/subgroup/repo", "git", "group/subgroup/repo"),
			("owner/repo", "git", "owner/repo"),
			("Coll/_git/repo", "git", "Coll/_git/repo"),
			// A local path keeps its spelling whatever it looks like; a Windows
			// drive letter would otherwise read as an SCP-like host.
			("/opt/skills/a", "local", "/opt/skills/a"),
			("C:\\skills\\a", "local", "C:\\skills\\a"),
			("./owner/repo", "local", "./owner/repo"),
		] {
			assert_eq!(
				reconstruct_source_url(source, source_type),
				expected,
				"source={source} type={source_type}"
			);
		}
	}

	/// Membership follows the same origin, in both directions.
	#[test]
	fn membership_admits_only_the_same_origin() {
		assert!(source_matches(
			"https://github.com/owner/repo.git",
			"owner/repo",
			Some("https://github.com/owner/repo"),
			"github",
		));
		// An ORIGIN is an internal grouping key, deliberately NOT an accepted
		// input: it is not a coordinate anything can fetch from, and echoing one
		// back used to yield a different origin than the row it came from (the
		// port case parsed as SCP-like). Callers pass the row's `source` or its
		// `sourceUrl`.
		assert!(!source_matches(
			"github.com/owner/repo",
			"owner/repo",
			Some("https://github.com/owner/repo.git"),
			"github",
		));
		// A host-blind identifier the lock records but we cannot resolve (TFS
		// collection path) still matches itself — nothing finer is available.
		assert!(source_matches(
			"Coll/_git/repo",
			"Coll/_git/repo",
			Some("https://tfs.example.com/tfs/Coll/_git/repo"),
			"git",
		));
		assert!(
			!source_matches(
				"https://github.com/owner/repo.git",
				"owner/repo",
				Some("https://gitlab.com/owner/repo.git"),
				"gitlab",
			),
			"a same-path entry on another forge is NOT part of this row"
		);
	}
}

#[cfg(test)]
mod list_tests {
	use super::*;
	#[test]
	fn list_sources_global_only_all_global_scope() {
		let out = list_sources(SourceListInput {
			scopes: vec![SourceScope::Global],
		});
		assert!(out.iter().all(|s| s.scope == SourceScopeKind::Global));
	}
}

#[cfg(test)]
mod classify_tests {
	use super::*;
	use std::fs;
	use tempfile::tempdir;

	fn write_skill(root: &Path, relative_dir: &str, name: &str) {
		let dir = root.join(relative_dir);
		fs::create_dir_all(&dir).unwrap();
		fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {name} skill\n---\n"),
		)
		.unwrap();
	}

	#[test]
	fn classify_prefers_local_hash_over_stale_stored_hash() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let fresh = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let entry = BaselineEntry {
			installed_name: "skill".to_string(),
			stored_hash: "stale-lock-hash".to_string(),
			local_hashes: vec![fresh],
			scope_label: "project".to_string(),
			ref_name: None,
		};

		assert_eq!(
			classify_installed(&entry, dir.path()),
			(SourceSkillState::InstalledCurrent, None)
		);
	}

	#[test]
	fn classify_falls_back_to_stored_hash_without_local_hash() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let entry = BaselineEntry {
			installed_name: "skill".to_string(),
			stored_hash: "stale-lock-hash".to_string(),
			local_hashes: Vec::new(),
			scope_label: "project".to_string(),
			ref_name: None,
		};

		assert_eq!(
			classify_installed(&entry, dir.path()),
			(SourceSkillState::InstalledOutdated, None)
		);
	}

	#[test]
	fn classify_unknown_lock_hash_as_current() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let entry = BaselineEntry {
			installed_name: "skill".to_string(),
			stored_hash: skill::EMPTY_SKILLS_LOCK_DIGEST.to_string(),
			local_hashes: Vec::new(),
			scope_label: "project".to_string(),
			ref_name: None,
		};

		assert_eq!(
			classify_installed(&entry, dir.path()),
			(SourceSkillState::InstalledCurrent, None)
		);
	}

	#[test]
	fn classify_outdated_when_any_installed_hash_differs() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let fresh = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let entry = BaselineEntry {
			installed_name: "skill".to_string(),
			stored_hash: fresh.clone(),
			local_hashes: vec![fresh, "older-install".to_string()],
			scope_label: "project".to_string(),
			ref_name: None,
		};

		assert_eq!(
			classify_installed(&entry, dir.path()),
			(SourceSkillState::InstalledOutdated, None)
		);
	}

	#[test]
	fn classify_source_diff_reports_renamed_before_hash_compare() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let entry = BaselineEntry {
			installed_name: "old-skill".to_string(),
			stored_hash: "stale-lock-hash".to_string(),
			local_hashes: Vec::new(),
			scope_label: "project".to_string(),
			ref_name: None,
		};

		assert_eq!(
			classify_source_skill_diff(&entry, "new-skill", dir.path()),
			(
				SourceSkillState::Renamed,
				Some("old-skill".to_string()),
				None
			)
		);
	}

	#[test]
	fn source_diff_reports_locked_skill_removed_when_upstream_path_disappears()
	{
		let dir = tempdir().unwrap();
		let skill_dir = dir.path().join("skills/engineering/diagnosing-bugs");
		fs::create_dir_all(&skill_dir).unwrap();
		let skill_file = skill_dir.join("SKILL.md");
		fs::write(
			&skill_file,
			b"---\nname: diagnosing-bugs\ndescription: new name\n---\n",
		)
		.unwrap();

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/engineering/diagnose/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "diagnose".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
				ref_name: None,
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "diagnosing-bugs".to_string(),
				full_path: skill_file,
				relative_dir: "skills/engineering/diagnosing-bugs".to_string(),
			}],
			&baseline,
			None,
		);

		let removed = diffs
			.iter()
			.find(|diff| {
				diff.skill_path == "skills/engineering/diagnose/SKILL.md"
			})
			.expect("removed lock entry should be present");
		assert_eq!(removed.name, "diagnose");
		assert_eq!(removed.state, SourceSkillState::Removed);
		assert_eq!(removed.reason.as_deref(), Some("noPath"));
		assert_eq!(removed.installed_paths, vec!["global".to_string()]);

		assert!(diffs.iter().any(|diff| {
			diff.skill_path == "skills/engineering/diagnosing-bugs/SKILL.md"
				&& diff.state == SourceSkillState::NotInstalled
		}));
	}

	/// Deleting a source's LAST skill upstream must still surface as `Removed`.
	/// This goes through `classify_repo_skills` (not the lower
	/// `build_source_skill_diffs`) because the empty-discovery short-circuit
	/// lives there: it used to return an empty diff, so the deletion of the
	/// only skill in a source was invisible in the UI.
	#[test]
	fn empty_upstream_still_reports_the_last_skill_as_removed() {
		let dir = tempdir().unwrap();

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/only/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "only".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
				ref_name: None,
			},
		);

		let diffs = classify_repo_skills(dir.path(), &baseline, None);

		assert_eq!(diffs.len(), 1, "expected the removed row, got {diffs:?}");
		assert_eq!(diffs[0].name, "only");
		assert_eq!(diffs[0].state, SourceSkillState::Removed);
		assert_eq!(diffs[0].reason.as_deref(), Some("noPath"));
	}

	#[test]
	fn source_diff_marks_deprecated_repo_skills_separately() {
		let dir = tempdir().unwrap();
		let skill_dir = dir.path().join("skills/deprecated/qa");
		fs::create_dir_all(&skill_dir).unwrap();
		let skill_file = skill_dir.join("SKILL.md");
		fs::write(&skill_file, b"---\nname: qa\ndescription: old\n---\n")
			.unwrap();

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "qa".to_string(),
				full_path: skill_file,
				relative_dir: "skills/deprecated/qa".to_string(),
			}],
			&Baseline::new(),
			None,
		);

		assert_eq!(diffs.len(), 1);
		assert_eq!(diffs[0].name, "qa");
		assert_eq!(diffs[0].state, SourceSkillState::Deprecated);
		assert_eq!(diffs[0].skill_path, "skills/deprecated/qa/SKILL.md");
	}

	#[test]
	fn source_diff_uses_changelog_to_report_moved_skill_as_renamed() {
		let dir = tempdir().unwrap();
		fs::write(
				dir.path().join("CHANGELOG.md"),
				"- [`47bde84`](https://github.com/mattpocock/skills/commit/47bde84) \
				 Thanks - Rename the **`diagnose`** skill to \
				 **`diagnosing-bugs`**.",
			)
			.unwrap();
		let skill_dir = dir.path().join("skills/engineering/diagnosing-bugs");
		fs::create_dir_all(&skill_dir).unwrap();
		let skill_file = skill_dir.join("SKILL.md");
		fs::write(
			&skill_file,
			b"---\nname: diagnosing-bugs\ndescription: new name\n---\n",
		)
		.unwrap();

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/engineering/diagnose/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "diagnose".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
				ref_name: None,
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "diagnosing-bugs".to_string(),
				full_path: skill_file,
				relative_dir: "skills/engineering/diagnosing-bugs".to_string(),
			}],
			&baseline,
			None,
		);

		let renamed = diffs
			.iter()
			.find(|diff| diff.name == "diagnosing-bugs")
			.expect("new skill should be present");
		assert_eq!(renamed.state, SourceSkillState::Renamed);
		assert_eq!(renamed.previous_name.as_deref(), Some("diagnose"));
		assert_eq!(renamed.installed_paths, vec!["global".to_string()]);
		assert!(!diffs.iter().any(|diff| {
			diff.skill_path == "skills/engineering/diagnose/SKILL.md"
		}));
	}

	#[test]
	fn source_diff_uses_changelog_to_report_replaced_skill_as_renamed() {
		let dir = tempdir().unwrap();
		fs::write(
				dir.path().join("CHANGELOG.md"),
				"- [`47bde84`](https://github.com/mattpocock/skills/commit/47bde84) \
				 Thanks - Replace **`write-a-skill`** with \
				 **`writing-great-skills`**.",
			)
			.unwrap();
		let skill_dir =
			dir.path().join("skills/productivity/writing-great-skills");
		fs::create_dir_all(&skill_dir).unwrap();
		let skill_file = skill_dir.join("SKILL.md");
		fs::write(
			&skill_file,
			b"---\nname: writing-great-skills\ndescription: new skill\n---\n",
		)
		.unwrap();

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/productivity/write-a-skill/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "write-a-skill".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
				ref_name: None,
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "writing-great-skills".to_string(),
				full_path: skill_file,
				relative_dir: "skills/productivity/writing-great-skills"
					.to_string(),
			}],
			&baseline,
			None,
		);

		let renamed = diffs
			.iter()
			.find(|diff| diff.name == "writing-great-skills")
			.expect("replacement skill should be present");
		assert_eq!(renamed.state, SourceSkillState::Renamed);
		assert_eq!(renamed.previous_name.as_deref(), Some("write-a-skill"));
		assert!(!diffs.iter().any(|diff| {
			diff.skill_path == "skills/productivity/write-a-skill/SKILL.md"
		}));
	}

	#[test]
	fn changelog_rename_ignores_trailing_backtick_tokens() {
		// A trailing PR/commit ref in backticks must NOT be mistaken for the
		// new name. Old/new are anchored on the connective, not the last two
		// backtick terms.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename the `diagnose` skill to `diagnosing-bugs` (see `#123`).",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert_eq!(
			successors.get("diagnose").map(String::as_str),
			Some("diagnosing-bugs"),
			"connective-anchored parse should map diagnose -> diagnosing-bugs"
		);
		assert!(
			!successors.contains_key("diagnosing-bugs"),
			"trailing `#123` must not become a successor target"
		);
	}

	#[test]
	fn changelog_rename_handles_multiple_pairs_on_one_line() {
		// Two renames on one line: both must be captured, not just the last.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename `alpha` to `alpha-two`; rename `beta` to `beta-two`.",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert_eq!(
			successors.get("alpha").map(String::as_str),
			Some("alpha-two")
		);
		assert_eq!(
			successors.get("beta").map(String::as_str),
			Some("beta-two")
		);
	}

	#[test]
	fn build_diffs_skips_rename_when_target_name_is_ambiguous() {
		// Two discovered skills share a frontmatter name; a CHANGELOG rename
		// pointing at that name must NOT silently redirect onto an arbitrary
		// one of them. The removed lock entry is reported honestly instead.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename `legacy` to `shared`.",
		)
		.unwrap();
		write_skill(dir.path(), "skills/a/shared", "shared");
		write_skill(dir.path(), "skills/b/shared", "shared");

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/legacy/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "legacy".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
				ref_name: None,
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![
				skill::RepoDiscoveredSkill {
					name: "shared".to_string(),
					full_path: dir.path().join("skills/a/shared/SKILL.md"),
					relative_dir: "skills/a/shared".to_string(),
				},
				skill::RepoDiscoveredSkill {
					name: "shared".to_string(),
					full_path: dir.path().join("skills/b/shared/SKILL.md"),
					relative_dir: "skills/b/shared".to_string(),
				},
			],
			&baseline,
			None,
		);

		assert!(
			!diffs
				.iter()
				.any(|diff| diff.state == SourceSkillState::Renamed),
			"ambiguous successor name must not produce a rename redirect"
		);
		let removed = diffs
			.iter()
			.find(|diff| diff.skill_path == "skills/legacy/SKILL.md")
			.expect("removed lock entry should be present");
		assert_eq!(removed.state, SourceSkillState::Removed);
		assert_eq!(removed.name, "legacy");
	}

	#[test]
	fn build_diffs_does_not_overwrite_deprecated_with_rename() {
		// A CHANGELOG successor that lands on a skill living under a
		// `deprecated/` path must keep the deprecated signal; the old lock
		// entry is then reported as removed rather than renamed.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename `old-qa` to `qa`.",
		)
		.unwrap();
		write_skill(dir.path(), "skills/deprecated/qa", "qa");

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/engineering/old-qa/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "old-qa".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
				ref_name: None,
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "qa".to_string(),
				full_path: dir.path().join("skills/deprecated/qa/SKILL.md"),
				relative_dir: "skills/deprecated/qa".to_string(),
			}],
			&baseline,
			None,
		);

		let qa = diffs
			.iter()
			.find(|diff| diff.skill_path == "skills/deprecated/qa/SKILL.md")
			.expect("deprecated skill should be present");
		assert_eq!(
			qa.state,
			SourceSkillState::Deprecated,
			"rename must not overwrite the deprecated state"
		);
		let removed = diffs
			.iter()
			.find(|diff| {
				diff.skill_path == "skills/engineering/old-qa/SKILL.md"
			})
			.expect("old lock entry should be present");
		assert_eq!(removed.state, SourceSkillState::Removed);
	}

	#[test]
	fn changelog_replace_with_ignores_trailing_tokens_and_handles_multiple() {
		// The `replace … with …` connective gets the same connective-anchored
		// treatment as `rename … to …`: trailing refs do not hijack the new
		// name, and multiple pairs on one line are all captured.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Replace `write-a-skill` with `writing-great-skills` (see `#42`); \
			 replace `old-x` with `new-x`.",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert_eq!(
			successors.get("write-a-skill").map(String::as_str),
			Some("writing-great-skills"),
		);
		assert_eq!(successors.get("old-x").map(String::as_str), Some("new-x"));
		assert!(
			!successors.contains_key("#42"),
			"trailing `#42` must not become a successor key"
		);
		assert!(!successors.values().any(|v| v == "#42"));
	}

	#[test]
	fn changelog_rename_yields_nothing_when_backticks_do_not_bracket_connective(
	) {
		// Verb + connective present, but no backtick term AFTER the connective:
		// the parser must yield no successor rather than guessing.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename `foo` `bar` to improve naming consistency.",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert!(
			successors.is_empty(),
			"no backtick after the connective => no successor, got {successors:?}"
		);
	}

	#[test]
	fn changelog_rename_ignores_connective_inside_a_backtick_term() {
		// A connective (" to "/" with ") that occurs INSIDE a backtick term, or
		// in a different clause from the rename verb, must not bind the
		// surrounding terms into a successor. Here the only "rename" clause does
		// not contain a connective, so the line yields nothing.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename docs for `alpha`; note `x to y` maps to `beta`.",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert!(
			successors.is_empty(),
			"connective inside a backtick term / foreign clause must not map, got {successors:?}"
		);
	}
}

#[cfg(test)]
mod diff_tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	/// A [`Fetcher`] that serves a fixed local dir as the fetched repo, ignoring
	/// the source/token.
	struct DirFetcher {
		root: std::path::PathBuf,
	}
	impl Fetcher for DirFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
			_selection: FetchSelection<'_>,
		) -> Result<crate::FetchedRepo, FetchError> {
			Ok(crate::FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: "test-oid".to_string(),
					tree_oid: "test-tree-oid".to_string(),
					commit_time: None,
				},
				_guard: None,
			})
		}
	}

	/// A [`TokenResolver`] that never has a token.
	struct NoToken;
	impl TokenResolver for NoToken {
		fn resolve(&self, _source: &str) -> TokenResolution {
			TokenResolution::NoToken
		}
	}

	/// A [`Fetcher`] that records the `ref_` it was asked to fetch so a test
	/// can assert the resolved ref handed to the fetch (recorded-ref fallback).
	struct RefCapturingFetcher {
		root: std::path::PathBuf,
		seen_ref: std::sync::Mutex<Option<Option<String>>>,
	}
	impl Fetcher for RefCapturingFetcher {
		fn fetch(
			&self,
			sr: &SourceRef,
			_token: Option<&str>,
			_selection: FetchSelection<'_>,
		) -> Result<crate::FetchedRepo, FetchError> {
			*self.seen_ref.lock().unwrap() = Some(sr.ref_.clone());
			Ok(crate::FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: "test-oid".to_string(),
					tree_oid: "test-tree-oid".to_string(),
					commit_time: None,
				},
				_guard: None,
			})
		}
	}

	/// A [`Fetcher`] serving a DIFFERENT tree per requested ref, so a test can
	/// tell which ref's tree a verdict was computed from.
	struct PerRefFetcher {
		roots: BTreeMap<Option<String>, std::path::PathBuf>,
		fetched: std::sync::Mutex<Vec<Option<String>>>,
	}
	impl Fetcher for PerRefFetcher {
		fn fetch(
			&self,
			sr: &SourceRef,
			_token: Option<&str>,
			_selection: FetchSelection<'_>,
		) -> Result<crate::FetchedRepo, FetchError> {
			self.fetched.lock().unwrap().push(sr.ref_.clone());
			let root = self
				.roots
				.get(&sr.ref_)
				.unwrap_or_else(|| panic!("unexpected ref {:?}", sr.ref_));
			Ok(crate::FetchedRepo {
				root: root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: "test-oid".to_string(),
					tree_oid: "test-tree-oid".to_string(),
					commit_time: None,
				},
				_guard: None,
			})
		}
	}

	/// A Sources row is ONE repository, but its entries need not share a branch.
	/// Every entry must be judged against the tree it was installed from: judging
	/// a `v1` entry against `main` reports it outdated forever (its apply fetches
	/// `v1`, so the hash never converges) or — when the folder does not exist on
	/// `main` — `Removed { noPath }`, which the desktop deletes in one click.
	///
	/// The mutation seam already fetches once per `(source, ref)`; this is the
	/// same grouping on the read side.
	#[test]
	fn each_ref_cohort_is_judged_against_its_own_tree() {
		let project = TempDir::new().unwrap();
		let trees = TempDir::new().unwrap();
		// `main` carries only `shared`; `v1` carries `shared` AND `pinned`.
		let main_root = trees.path().join("main");
		let v1_root = trees.path().join("v1");
		for (root, dirs) in [
			(&main_root, vec!["alpha"]),
			(&v1_root, vec!["alpha", "zeta"]),
		] {
			for dir in dirs {
				write_skill(root, dir, dir);
			}
		}

		let mut lock = skill::LocalSkillLockFile::new();
		for (name, dir, ref_name) in
			[("alpha", "alpha", "main"), ("zeta", "zeta", "v1")]
		{
			lock.skills.insert(
				name.to_string(),
				skill::LocalSkillLockEntry {
					source: "owner/repo".to_string(),
					ref_name: Some(ref_name.to_string()),
					source_type: "github".to_string(),
					skill_path: Some(format!("{dir}/SKILL.md")),
					computed_hash: "stale".to_string(),
					ref_commit: None,
					source_url: None,
				},
			);
		}
		skill::write_local_lock(&lock, Some(project.path())).unwrap();

		let fetcher = PerRefFetcher {
			roots: BTreeMap::from([
				(Some("main".to_string()), main_root.clone()),
				(Some("v1".to_string()), v1_root.clone()),
			]),
			fetched: std::sync::Mutex::new(Vec::new()),
		};
		let outcome = diff_source(
			SourceDiffInput {
				source: "owner/repo".to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Project {
					root: project.path().to_path_buf(),
				}],
			},
			SourceDiffDeps {
				fetcher: &fetcher,
				resolver: &NoToken,
			},
		);

		let SourceDiffOutcome::Ok { skills, .. } = outcome else {
			panic!("expected a diff, got {outcome:?}");
		};
		let state_of = |name: &str| {
			skills
				.iter()
				.find(|diff| diff.name == name)
				.unwrap_or_else(|| panic!("{name} missing from {skills:?}"))
				.state
				.clone()
		};
		// `zeta` exists on v1 — the ref it is installed from. The row's ref is
		// `main` (first-wins, and `alpha` sorts first), where zeta's folder is
		// absent: judged against THAT tree it is Removed{noPath}, which the
		// desktop offers to delete in one click.
		assert_eq!(state_of("zeta"), SourceSkillState::InstalledOutdated);
		assert_eq!(state_of("alpha"), SourceSkillState::InstalledOutdated);
		// One fetch per distinct ref, no more.
		let mut fetched = fetcher.fetched.lock().unwrap().clone();
		fetched.sort();
		assert_eq!(
			fetched,
			vec![Some("main".to_string()), Some("v1".to_string())]
		);
		// The v1 tree also carries `alpha`, but that is another cohort's entry,
		// not something to offer for installation — it must appear once.
		assert_eq!(
			skills.iter().filter(|d| d.name == "alpha").count(),
			1,
			"{skills:?}"
		);
	}

	/// A skill renamed upstream lives at a NEW path, which no baseline ever had.
	/// The primary tree carries that new path too, so filtering its offers on the
	/// BASELINE's paths let it advertise `new` as installable while the cohort
	/// that owns the entry reported `old → new` — two contradictory rows for one
	/// path, one offering an install and one offering a rename.
	#[test]
	fn a_cross_cohort_rename_is_not_also_offered_as_installable() {
		let project = TempDir::new().unwrap();
		let trees = TempDir::new().unwrap();
		// Both refs carry the renamed skill at its NEW path; the changelog
		// records the rename. `alpha` pins the row to `main`.
		let main_root = trees.path().join("main");
		let v1_root = trees.path().join("v1");
		for root in [&main_root, &v1_root] {
			write_skill(root, "alpha", "alpha");
			write_skill(root, "new", "new");
			fs::write(
				root.join("CHANGELOG.md"),
				"- Rename the `old` skill to `new`\n",
			)
			.unwrap();
		}

		let mut lock = skill::LocalSkillLockFile::new();
		for (name, path, ref_name) in [
			("alpha", "alpha/SKILL.md", "main"),
			// Installed from v1 at the OLD path, which neither tree has now.
			("old", "old/SKILL.md", "v1"),
		] {
			lock.skills.insert(
				name.to_string(),
				skill::LocalSkillLockEntry {
					source: "owner/repo".to_string(),
					ref_name: Some(ref_name.to_string()),
					source_type: "github".to_string(),
					skill_path: Some(path.to_string()),
					computed_hash: "stale".to_string(),
					ref_commit: None,
					source_url: None,
				},
			);
		}
		skill::write_local_lock(&lock, Some(project.path())).unwrap();

		let outcome = diff_source(
			SourceDiffInput {
				source: "owner/repo".to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Project {
					root: project.path().to_path_buf(),
				}],
			},
			SourceDiffDeps {
				fetcher: &PerRefFetcher {
					roots: BTreeMap::from([
						(Some("main".to_string()), main_root.clone()),
						(Some("v1".to_string()), v1_root.clone()),
					]),
					fetched: std::sync::Mutex::new(Vec::new()),
				},
				resolver: &NoToken,
			},
		);
		let SourceDiffOutcome::Ok { skills, .. } = outcome else {
			panic!("expected a diff, got {outcome:?}");
		};

		let rows: Vec<_> = skills
			.iter()
			.filter(|diff| diff.skill_path == "new/SKILL.md")
			.collect();
		assert_eq!(rows.len(), 1, "one row per path: {skills:?}");
		assert_eq!(rows[0].state, SourceSkillState::Renamed);
		assert_eq!(rows[0].previous_name.as_deref(), Some("old"));
	}

	/// Write a project lock entry recording a non-default ref for `source`.
	fn write_project_lock_entry(root: &Path, source: &str, ref_name: &str) {
		write_project_lock_entry_typed(root, source, Some(ref_name), "github");
	}

	/// Write a project lock entry with an explicit `source_type` and an optional
	/// recorded ref.
	fn write_project_lock_entry_typed(
		root: &Path,
		source: &str,
		ref_name: Option<&str>,
		source_type: &str,
	) {
		let mut lock = skill::LocalSkillLockFile::new();
		lock.skills.insert(
			"s".to_string(),
			skill::LocalSkillLockEntry {
				source_url: None,
				source: source.to_string(),
				ref_name: ref_name.map(str::to_string),
				source_type: source_type.to_string(),
				skill_path: Some("s/SKILL.md".to_string()),
				computed_hash: "h".to_string(),
				ref_commit: None,
			},
		);
		skill::write_local_lock(&lock, Some(root)).unwrap();
	}

	fn write_skill(root: &Path, relative_dir: &str, name: &str) {
		let dir = root.join(relative_dir);
		fs::create_dir_all(&dir).unwrap();
		fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {name} skill\n---\n"),
		)
		.unwrap();
	}

	#[test]
	fn diff_source_reports_not_installed() {
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");

		let fetcher = DirFetcher {
			root: upstream.path().to_path_buf(),
		};
		let resolver = NoToken;
		let outcome = diff_source(
			SourceDiffInput {
				// A unique test source not present in any installed lock, so the
				// baseline is empty and `alpha` resolves to NotInstalled.
				source: "test-owner/diff-source-not-installed".to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Global],
			},
			SourceDiffDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		match outcome {
			SourceDiffOutcome::Ok { skills, .. } => {
				let alpha = skills
					.iter()
					.find(|s| s.name == "alpha")
					.expect("alpha should be discovered");
				assert_eq!(alpha.state, SourceSkillState::NotInstalled);
			}
			other => panic!("expected Ok, got {other:?}"),
		}
	}

	#[test]
	fn diff_source_reports_unavailable_credentials_without_fetching() {
		struct Unavailable;
		impl TokenResolver for Unavailable {
			fn resolve(&self, _source: &str) -> TokenResolution {
				TokenResolution::BackendUnavailable
			}
		}

		struct CountingFetcher(std::sync::Mutex<usize>);
		impl Fetcher for CountingFetcher {
			fn fetch(
				&self,
				_source_ref: &SourceRef,
				_token: Option<&str>,
				_selection: FetchSelection<'_>,
			) -> Result<crate::FetchedRepo, FetchError> {
				*self.0.lock().unwrap() += 1;
				Err(FetchError::Network)
			}
		}

		let fetcher = CountingFetcher(std::sync::Mutex::new(0));
		let outcome = diff_source(
			SourceDiffInput {
				source: "owner/private-source".to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Global],
			},
			SourceDiffDeps {
				fetcher: &fetcher,
				resolver: &Unavailable,
			},
		);

		assert!(matches!(
			outcome,
			SourceDiffOutcome::UncheckableSource {
				reason: UncheckableReason::Network,
				..
			}
		));
		assert_eq!(
			*fetcher.0.lock().unwrap(),
			0,
			"an indeterminate credential decision must not fetch anonymously",
		);
	}

	/// The agent scan re-reads every registered agent's config from disk and
	/// does not vary by skill name, so the baseline builder must run it ONCE per
	/// scope. Doing it per entry is what made a single source diff cost
	/// `O(entries × agents)` — 264 full scans for an 11-entry source.
	///
	/// Counted through the shared `mutation::scan_agents` seam, so a future
	/// rewrite that reaches for `load_all_agents` directly fails here rather
	/// than silently reintroducing the sweep.
	#[test]
	fn baseline_scans_agents_once_per_scope_not_once_per_entry() {
		let project = TempDir::new().unwrap();
		let source = "owner/multi-entry-baseline";
		let mut lock = skill::LocalSkillLockFile::new();
		for name in ["alpha", "beta", "gamma"] {
			lock.skills.insert(
				name.to_string(),
				skill::LocalSkillLockEntry {
					source_url: None,
					source: source.to_string(),
					ref_name: None,
					source_type: "github".to_string(),
					skill_path: Some(format!("{name}/SKILL.md")),
					computed_hash: "h".to_string(),
					ref_commit: None,
				},
			);
		}
		skill::write_local_lock(&lock, Some(project.path())).unwrap();

		crate::mutation::AGENT_SCANS.with(|scans| scans.set(0));
		let (baseline, _source_type, _recorded_ref) =
			merged_baseline_for_source(
				&[SourceScope::Project {
					root: project.path().to_path_buf(),
				}],
				source,
			);

		assert_eq!(
			baseline.len(),
			3,
			"all three entries must reach the baseline, or the scan count \
			 below proves nothing"
		);
		assert_eq!(
			crate::mutation::AGENT_SCANS.with(|scans| scans.get()),
			1,
			"3 entries in one scope must scan the agents once"
		);
	}

	#[test]
	fn merged_baseline_surfaces_recorded_ref_for_fallback() {
		// A skill installed from a tag/feature-branch records its ref; the
		// baseline builder must surface it so `diff_source` can fall back to
		// it when no explicit ref is requested.
		let project = TempDir::new().unwrap();
		let source = "owner/recorded-ref-baseline";
		write_project_lock_entry(project.path(), source, "v1.2.3");

		let (_baseline, _source_type, recorded_ref) =
			merged_baseline_for_source(
				&[SourceScope::Project {
					root: project.path().to_path_buf(),
				}],
				source,
			);

		assert_eq!(recorded_ref.as_deref(), Some("v1.2.3"));
	}

	#[test]
	fn diff_source_falls_back_to_recorded_ref_when_input_ref_none() {
		// input git_ref None + a lock entry with a recorded ref => the resolved
		// git_ref (surfaced on the outcome AND handed to the fetch) must be the
		// recorded ref, not None / the default branch. Without the fallback the
		// fetch would receive None and the outcome would carry None.
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");
		let project = TempDir::new().unwrap();
		let source = "owner/recorded-ref-diff";
		write_project_lock_entry(project.path(), source, "v9.9.9");

		let fetcher = RefCapturingFetcher {
			root: upstream.path().to_path_buf(),
			seen_ref: std::sync::Mutex::new(None),
		};
		let resolver = NoToken;
		let outcome = diff_source(
			SourceDiffInput {
				source: source.to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Project {
					root: project.path().to_path_buf(),
				}],
			},
			SourceDiffDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		match outcome {
			SourceDiffOutcome::Ok { git_ref, .. } => {
				assert_eq!(
					git_ref.as_deref(),
					Some("v9.9.9"),
					"outcome must carry the recorded ref as the resolved ref"
				);
			}
			other => panic!("expected Ok, got {other:?}"),
		}
		assert_eq!(
			fetcher.seen_ref.lock().unwrap().clone(),
			Some(Some("v9.9.9".to_string())),
			"fetch must be issued at the recorded ref, not the default branch"
		);
	}

	#[test]
	fn resolve_source_meta_explicit_ref_wins_over_recorded() {
		// explicit > recorded: a passed `--ref` overrides the lock's recorded
		// ref entirely.
		let project = TempDir::new().unwrap();
		let source = "owner/meta-explicit";
		write_project_lock_entry(project.path(), source, "v1");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			Some("feature-x"),
		);

		assert_eq!(meta.effective_ref.as_deref(), Some("feature-x"));
		assert_eq!(meta.source_type, "github");
	}

	#[test]
	fn resolve_source_meta_recorded_ref_used_when_no_explicit() {
		// recorded > None: with no `--ref`, the recorded lock ref is used.
		let project = TempDir::new().unwrap();
		let source = "owner/meta-recorded";
		write_project_lock_entry(project.path(), source, "v2");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		);

		assert_eq!(meta.effective_ref.as_deref(), Some("v2"));
	}

	#[test]
	fn resolve_source_meta_none_when_neither_explicit_nor_recorded() {
		// No explicit ref AND no recorded ref => None (default branch). This is
		// the case where the CLI records None on install while the API records
		// the scan session's resolved default-branch name (documented residual
		// in `crates/cli/src/commands/source.rs`).
		let project = TempDir::new().unwrap();
		let source = "owner/meta-none";
		write_project_lock_entry_typed(project.path(), source, None, "github");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		);

		assert_eq!(meta.effective_ref, None);
	}

	#[test]
	fn resolve_source_meta_passes_through_recorded_source_type() {
		// A lock recording source_type "git" (not "github") must surface "git",
		// not the "github" default.
		let project = TempDir::new().unwrap();
		let source = "git@example.com:owner/meta-git.git";
		write_project_lock_entry_typed(project.path(), source, None, "git");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		);

		assert_eq!(meta.source_type, "git");
	}

	/// `diff_source` and CLI `source diff`/`sync` fetch `effective_source`. It
	/// must be the SAME coordinate `list_sources` advertises and apply uses:
	/// falling back to the caller's raw `group/repo` sent the diff to GitHub
	/// while the row and the apply went to GitLab, so the badge compared against
	/// one repository and the update installed from another.
	#[test]
	fn resolve_source_meta_reconstructs_the_coordinate_apply_uses() {
		let project = TempDir::new().unwrap();
		let mut lock = skill::LocalSkillLockFile::new();
		lock.skills.insert(
			"legacy".to_string(),
			skill::LocalSkillLockEntry {
				source: "group/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "gitlab".to_string(),
				skill_path: Some("legacy/SKILL.md".to_string()),
				computed_hash: "h".to_string(),
				ref_commit: None,
				source_url: None,
			},
		);
		skill::write_local_lock(&lock, Some(project.path())).unwrap();
		let scopes = [SourceScope::Project {
			root: project.path().to_path_buf(),
		}];

		// The host-blind identifier `source list` prints, and the row URL the
		// desktop sends, must both land on the entry's own forge.
		for want in ["group/repo", "https://gitlab.com/group/repo.git"] {
			let meta = resolve_source_meta(want, &scopes, None);
			assert_eq!(
				meta.effective_source.as_deref(),
				Some("https://gitlab.com/group/repo.git"),
				"want={want} must not be diffed against GitHub"
			);
			assert_eq!(meta.source_type, "gitlab");
		}
	}

	/// `source list` prints the lock's host-blind `SOURCE`, and pasting that
	/// column back selects EVERY forge serving the same path. One fetched tree
	/// then judges all of them — `source sync --update` would overwrite the
	/// GitLab skill with GitHub's bytes, and the diff's "clean up removed" would
	/// offer to delete it. Naming one forge's clone URL stays unambiguous.
	#[test]
	fn a_host_blind_source_spanning_two_forges_is_refused() {
		let project = TempDir::new().unwrap();
		let mut lock = skill::LocalSkillLockFile::new();
		for (name, source_url, source_type) in [
			("gh", "https://github.com/owner/repo.git", "github"),
			("gl", "https://gitlab.com/owner/repo.git", "gitlab"),
		] {
			lock.skills.insert(
				name.to_string(),
				skill::LocalSkillLockEntry {
					source: "owner/repo".to_string(),
					ref_name: Some("main".to_string()),
					source_type: source_type.to_string(),
					skill_path: Some(format!("{name}/SKILL.md")),
					computed_hash: "h".to_string(),
					ref_commit: None,
					source_url: Some(source_url.to_string()),
				},
			);
		}
		skill::write_local_lock(&lock, Some(project.path())).unwrap();
		let scopes = [SourceScope::Project {
			root: project.path().to_path_buf(),
		}];

		let blind = resolve_source_meta("owner/repo", &scopes, None);
		assert_eq!(
			blind.ambiguous_origins(),
			Some(
				&[
					"github.com/owner/repo".to_string(),
					"gitlab.com/owner/repo".to_string(),
				][..]
			),
			"the printed SOURCE column selects both forges"
		);
		// Refused BEFORE any fetch: the whole point is that no single tree is
		// authoritative here, so fetching one and judging both is the bug.
		struct PanicFetcher;
		impl Fetcher for PanicFetcher {
			fn fetch(
				&self,
				_sr: &SourceRef,
				_token: Option<&str>,
				_selection: FetchSelection<'_>,
			) -> Result<crate::FetchedRepo, FetchError> {
				panic!("an ambiguous source must not be fetched");
			}
		}
		assert!(matches!(
			diff_source(
				SourceDiffInput {
					source: "owner/repo".to_string(),
					git_ref: None,
					scopes: scopes.to_vec(),
				},
				SourceDiffDeps {
					fetcher: &PanicFetcher,
					resolver: &NoToken,
				},
			),
			SourceDiffOutcome::AmbiguousSource { .. }
		));

		// Naming ONE forge is unambiguous, and still resolves to that forge.
		for url in [
			"https://github.com/owner/repo.git",
			"https://gitlab.com/owner/repo.git",
		] {
			let exact = resolve_source_meta(url, &scopes, None);
			assert_eq!(exact.ambiguous_origins(), None, "{url}");
			assert_eq!(exact.effective_source.as_deref(), Some(url));
		}
	}

	#[test]
	fn resolve_source_meta_defaults_source_type_to_github_when_absent() {
		// A source not present in any lock => empty recorded type, defaulted to
		// "github" (matching the default `diff_source` applies before its
		// `precheck_source`).
		let meta = resolve_source_meta(
			"owner/meta-absent",
			&[SourceScope::Global],
			None,
		);

		assert_eq!(meta.source_type, "github");
		assert_eq!(meta.effective_ref, None);
	}

	#[test]
	fn resolve_source_meta_local_source_prechecks_uncheckable() {
		// A lock recording source_type "local" must surface "local", which
		// `precheck_source` then rejects as uncheckable (no fetch).
		use aghub_core::skills::update::{precheck_source, UncheckableReason};
		let project = TempDir::new().unwrap();
		let source = "/some/local/path";
		write_project_lock_entry_typed(project.path(), source, None, "local");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		);

		assert_eq!(meta.source_type, "local");
		assert!(matches!(
			precheck_source(&meta.source_type, source),
			Some(UncheckableReason::Local)
		));
	}
}
