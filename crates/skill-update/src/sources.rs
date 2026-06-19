//! Sources domain service. Extracted from `crates/api/src/routes/sources.rs`
//! so the API and the CLI share one implementation. Fetch + credentials are
//! injected via [`crate::Fetcher`] / [`crate::TokenResolver`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{FetchError, Fetcher, SourceRef, TokenResolver};
use aghub_core::skills::update::UncheckableReason;

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
}

/// skill_path -> installed baseline metadata.
pub(crate) struct BaselineEntry {
	pub installed_name: String,
	pub stored_hash: String,
	pub local_hashes: Vec<String>,
	pub scope_label: String,
}
pub(crate) type Baseline = BTreeMap<String, BaselineEntry>;

#[derive(Debug)]
pub enum SourceDiffOutcome {
	/// Flat skill list (API-compatible: merged baseline, classified once).
	/// Carries the resolved `git_ref` (query override → recorded ref → None)
	/// so the API response keeps the old recorded-ref fallback.
	Ok {
		git_ref: Option<String>,
		skills: Vec<SourceSkillDiff>,
	},
	NeedsCredential,
	FetchFailed,
	/// Local/ssh/unsupported scheme — known before any fetch. Carries the
	/// resolved git_ref too (the old route returned it on the early-out).
	UncheckableSource {
		git_ref: Option<String>,
		reason: UncheckableReason,
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

pub fn list_sources(_input: SourceListInput) -> Vec<SourceSummary> {
	todo!("Task 1.2")
}

pub fn fetch_source_with_resolver(
	_source_ref: &SourceRef,
	_fetcher: &dyn Fetcher,
	_resolver: &dyn TokenResolver,
) -> Result<crate::FetchedRepo, FetchError> {
	todo!("Task 1.4")
}

/// Internal: classify discovered repo skills against a prebuilt baseline.
/// `Baseline`/`BaselineEntry` stay `pub(crate)` so they never leak across the
/// crate boundary; cross-crate callers use [`classify_scope`] / [`diff_source`].
pub(crate) fn classify_repo_skills(
	_root: &Path,
	_baseline: &Baseline,
) -> Vec<SourceSkillDiff> {
	todo!("Task 1.3")
}

/// PUBLIC CLI entry: build the baseline for one scope and classify the fetched
/// repo against it. Does NOT fetch (caller passes the already-fetched `root`),
/// so the CLI reuses one `FetchedRepo` for every scope and for install.
pub fn classify_scope(
	_root: &Path,
	_scope: &SourceScope,
	_source: &str,
) -> Vec<SourceSkillDiff> {
	todo!("Task 1.4")
}

/// PUBLIC API entry: merged-baseline, single-classification, flat output —
/// byte-identical to the old route. Fetches internally via `deps`.
pub fn diff_source(
	_input: SourceDiffInput,
	_deps: SourceDiffDeps<'_>,
) -> SourceDiffOutcome {
	todo!("Task 1.4")
}
