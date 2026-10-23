//! Tree-first proof, end to end through the real `SkillRepository` over a fake
//! GitHub REST transport. Every assertion about cost counts REQUESTS through
//! the `HttpTransport` seam — never wall-clock (crates/git AGENTS.md).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use aghub_core::skills::update::SkillUpdateStatus;
use aghub_core::WriteScope;
use aghub_git::{
	BlobHasher, Credentials, GitError, GithubRest, HttpRequest, HttpResponse,
	HttpTransport, RepoFetchBackend, RepoSnapshot, RepoTree, StagedEntryMode,
};

use crate::sources::{
	diff_source, SourceDiffDeps, SourceDiffInput, SourceDiffOutcome,
	SourceSkillState,
};
use crate::{
	run_update_check, EntryInput, FetchSelection, FetchedRepo, GitFetcher,
	InstalledFolder, LocalManifest, SkillRepository, SourceRef,
	TokenResolution, TokenResolver,
};

const COMMIT: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";

fn oid(bytes: &[u8]) -> String {
	let mut hasher = BlobHasher::new(bytes.len() as u64);
	hasher.update(bytes);
	hasher.finish().unwrap()
}

/// A GitHub repo served from a path → (mode, bytes) map, recording every URL.
struct FakeGithub {
	files: BTreeMap<String, (StagedEntryMode, Vec<u8>)>,
	recorded: Mutex<Vec<String>>,
}

impl FakeGithub {
	fn new(files: BTreeMap<String, (StagedEntryMode, Vec<u8>)>) -> Arc<Self> {
		Arc::new(Self {
			files,
			recorded: Mutex::new(Vec::new()),
		})
	}

	fn urls(&self) -> Vec<String> {
		self.recorded.lock().unwrap().clone()
	}

	/// Blob oids downloaded, as a set.
	fn blobs(&self) -> BTreeSet<String> {
		self.urls()
			.iter()
			.filter_map(|u| u.split("/git/blobs/").nth(1).map(str::to_string))
			.collect()
	}

	/// The oids of every upstream file whose path satisfies `keep`.
	fn oids_where(&self, keep: impl Fn(&str) -> bool) -> BTreeSet<String> {
		self.files
			.iter()
			.filter(|(path, _)| keep(path))
			.map(|(_, (_, bytes))| oid(bytes))
			.collect()
	}
}

fn response(content_type: &str, body: Vec<u8>) -> HttpResponse {
	HttpResponse {
		status: 200,
		headers: vec![("content-type".into(), content_type.into())],
		body,
	}
}

impl HttpTransport for FakeGithub {
	fn execute(&self, request: HttpRequest) -> aghub_git::Result<HttpResponse> {
		let url = request.url;
		self.recorded.lock().unwrap().push(url.clone());
		if let Some(wanted) = url.split("/git/blobs/").nth(1) {
			let bytes = self
				.files
				.values()
				.find(|(_, bytes)| oid(bytes) == wanted)
				.map(|(_, bytes)| bytes.clone());
			return Ok(match bytes {
				Some(bytes) => response("application/vnd.github.raw", bytes),
				None => HttpResponse {
					status: 404,
					headers: Vec::new(),
					body: Vec::new(),
				},
			});
		}
		if url.contains("/git/trees/") {
			let entries: Vec<String> = self
				.files
				.iter()
				.map(|(path, (mode, bytes))| {
					let mode = match mode {
						StagedEntryMode::Regular => "100644",
						StagedEntryMode::Executable => "100755",
						StagedEntryMode::Symlink => "120000",
						StagedEntryMode::Gitlink => "160000",
					};
					format!(
						r#"{{"path":"{path}","mode":"{mode}","type":"blob","sha":"{}","size":{}}}"#,
						oid(bytes),
						bytes.len()
					)
				})
				.collect();
			let body = format!(
				r#"{{"sha":"{TREE}","truncated":false,"tree":[{}]}}"#,
				entries.join(",")
			);
			return Ok(response("application/json", body.into_bytes()));
		}
		if url.contains("/commits/") {
			let body = format!(
				r#"{{"sha":"{COMMIT}","commit":{{"tree":{{"sha":"{TREE}"}},"committer":{{"date":"2026-07-17T00:00:00Z"}}}}}}"#
			);
			return Ok(response("application/json", body.into_bytes()));
		}
		Ok(HttpResponse {
			status: 404,
			headers: Vec::new(),
			body: Vec::new(),
		})
	}
}

/// The gix slot must never be reached: REST serves every request here.
struct NeverGix;

impl RepoFetchBackend for NeverGix {
	fn resolve(
		&self,
		_s: &aghub_git::SourceRef,
		_a: Option<&Credentials>,
	) -> aghub_git::Result<RepoSnapshot> {
		panic!("gix slot reached")
	}
	fn read_tree(&self, _s: &RepoSnapshot) -> aghub_git::Result<RepoTree> {
		panic!("gix slot reached")
	}
	fn read_blobs(
		&self,
		_s: &RepoSnapshot,
		_o: &[String],
	) -> aghub_git::Result<Vec<aghub_git::Blob>> {
		panic!("gix slot reached")
	}
	fn materialize(
		&self,
		_s: &RepoSnapshot,
		_p: &[&str],
		_d: &Path,
	) -> aghub_git::Result<()> {
		Err(GitError::clone_failed("gix slot reached"))
	}
}

struct NoToken;
impl TokenResolver for NoToken {
	fn resolve(&self, _source: &str) -> TokenResolution {
		TokenResolution::NoToken
	}
}

fn fetcher(fake: &Arc<FakeGithub>) -> GitFetcher {
	let rest: Arc<dyn RepoFetchBackend> =
		Arc::new(GithubRest::new(Arc::clone(fake) as Arc<dyn HttpTransport>));
	GitFetcher::with_repository(SkillRepository::with_backends(
		Some(rest),
		Arc::new(NeverGix),
	))
}

fn skill_md(name: &str) -> Vec<u8> {
	format!(
		"---\nname: {name}\ndescription: {name} does things\nversion: 1.2.0\n\
		 author: acme\n---\n# {name}\n"
	)
	.into_bytes()
}

/// A skill's files, relative to its folder.
fn skill_files(name: &str) -> Vec<(String, Vec<u8>)> {
	vec![
		("SKILL.md".to_string(), skill_md(name)),
		(
			"scripts/run.sh".to_string(),
			format!("echo {name}\n").into_bytes(),
		),
		(
			"docs/guide.md".to_string(),
			format!("guide for {name}\n").into_bytes(),
		),
	]
}

fn write_local(dir: &Path, files: &[(String, Vec<u8>)]) {
	for (rel, bytes) in files {
		let path = dir.join(rel);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(path, bytes).unwrap();
	}
}

fn upstream_add(
	map: &mut BTreeMap<String, (StagedEntryMode, Vec<u8>)>,
	folder: &str,
	files: &[(String, Vec<u8>)],
) {
	for (rel, bytes) in files {
		map.insert(
			format!("{folder}/{rel}"),
			(StagedEntryMode::Regular, bytes.clone()),
		);
	}
}

/// A global lock entry whose installed copy is `dir`, recording no usable
/// refCommit — the npx / partial-update case the preflight can never skip.
fn entry(name: &str, dir: &Path, ref_commit: Option<&str>) -> EntryInput {
	let manifest = LocalManifest::read(dir).unwrap();
	EntryInput {
		name: name.to_string(),
		scope: "global".to_string(),
		source_ref: SourceRef {
			source: "https://github.com/acme/skills.git".to_string(),
			ref_: Some("main".to_string()),
		},
		source_type: "github".to_string(),
		skill_path: Some(format!("skills/{name}/SKILL.md")),
		stored_hash: Some(manifest.raw.clone()),
		local_hash: Some(manifest.raw.clone()),
		local_comparison_hash: Some(manifest.comparison.clone()),
		local_ambiguous: false,
		local_manifest: Some(Arc::new(manifest)),
		ref_commit: ref_commit.map(str::to_string),
		ambiguous_host: false,
	}
}

fn check(
	fake: &Arc<FakeGithub>,
	entries: Vec<EntryInput>,
) -> HashMap<String, (SkillUpdateStatus, Option<String>)> {
	let fetcher = fetcher(fake);
	let ref_resolver = Arc::new(fetcher.ref_resolver());
	let rt = tokio::runtime::Runtime::new().unwrap();
	rt.block_on(run_update_check(
		entries,
		Arc::new(fetcher),
		ref_resolver,
		&NoToken,
		false,
	))
	.into_iter()
	.map(|out| (out.key.name, (out.status, out.heal_oid)))
	.collect()
}

/// Upstream repo content: path -> (mode, bytes).
type Upstream = BTreeMap<String, (StagedEntryMode, Vec<u8>)>;

const NAMES: [&str; 6] = ["s0", "s1", "s2", "s3", "s4", "s5"];

/// Six installed skills, each copy byte-identical to upstream; refCommits that
/// disagree (or are absent), so the group must fetch.
fn unchanged_group() -> (tempfile::TempDir, Upstream, Vec<EntryInput>) {
	let local = tempfile::tempdir().unwrap();
	let mut upstream = BTreeMap::new();
	let mut entries = Vec::new();
	for (i, name) in NAMES.iter().enumerate() {
		let files = skill_files(name);
		let dir = local.path().join(name);
		write_local(&dir, &files);
		upstream_add(&mut upstream, &format!("skills/{name}"), &files);
		let recorded = match i % 3 {
			0 => None,
			1 => Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
			_ => Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
		};
		entries.push(entry(name, &dir, recorded));
	}
	(local, upstream, entries)
}

/// Acceptance 1: N unchanged folders that cannot preflight-skip cost the tip
/// resolve and ONE tree read — zero blob downloads — and all read UpToDate
/// (healing refCommit, so the next check can preflight).
#[test]
fn unchanged_group_costs_resolve_and_tree_only() {
	let (_local, upstream, entries) = unchanged_group();
	let fake = FakeGithub::new(upstream);

	let out = check(&fake, entries);

	for name in NAMES {
		assert_eq!(
			out[name],
			(SkillUpdateStatus::UpToDate, Some(COMMIT.to_string())),
			"{name}"
		);
	}
	let urls = fake.urls();
	assert_eq!(fake.blobs(), BTreeSet::new(), "no blob may be downloaded");
	assert_eq!(urls.len(), 2, "resolve + tree only, got {urls:#?}");
	assert!(urls[0].contains("/commits/") && urls[1].contains("/git/trees/"));
}

/// Acceptance 2: one changed file downloads only its folder; that member is
/// UpdateAvailable and every other member still UpToDate.
#[test]
fn one_changed_file_downloads_only_its_folder() {
	let (_local, mut upstream, entries) = unchanged_group();
	upstream.insert(
		"skills/s3/docs/guide.md".to_string(),
		(StagedEntryMode::Regular, b"guide v2\n".to_vec()),
	);
	let fake = FakeGithub::new(upstream);

	let out = check(&fake, entries);

	assert!(
		matches!(out["s3"].0, SkillUpdateStatus::UpdateAvailable { .. }),
		"{:?}",
		out["s3"]
	);
	for name in NAMES.iter().filter(|n| **n != "s3") {
		assert_eq!(out[*name].0, SkillUpdateStatus::UpToDate, "{name}");
	}
	assert_eq!(
		fake.blobs(),
		fake.oids_where(|p| p.starts_with("skills/s3/")),
		"exactly the changed folder's blobs"
	);
}

/// Acceptance 3: a symlink entry in the upstream folder (every file oid still
/// matching) is never trusted to the proof — the folder is materialized, and
/// the verdict is what materializing gives (the hash skips symlinks: current).
#[test]
fn an_upstream_symlink_falls_back_to_materializing() {
	let (_local, mut upstream, entries) = unchanged_group();
	upstream.insert(
		"skills/s1/link.md".to_string(),
		(StagedEntryMode::Symlink, b"SKILL.md".to_vec()),
	);
	let fake = FakeGithub::new(upstream);

	let out = check(&fake, entries);

	assert_eq!(out["s1"].0, SkillUpdateStatus::UpToDate);
	assert_eq!(
		fake.blobs(),
		fake.oids_where(|p| p.starts_with("skills/s1/")),
		"the symlinked folder is downloaded whole, nothing else"
	);
}

/// Acceptance 4: an extra local file (untracked notes) defeats the shortcut;
/// the folder is downloaded and judged as before — UpdateAvailable.
#[test]
fn an_extra_local_file_defeats_the_shortcut() {
	let (local, upstream, mut entries) = unchanged_group();
	let dir = local.path().join("s4");
	std::fs::write(dir.join("notes.md"), b"my notes").unwrap();
	let index = entries.iter().position(|e| e.name == "s4").unwrap();
	entries[index] = entry("s4", &dir, None);
	let fake = FakeGithub::new(upstream);

	let out = check(&fake, entries);

	assert!(
		matches!(out["s4"].0, SkillUpdateStatus::UpdateAvailable { .. }),
		"{:?}",
		out["s4"]
	);
	assert_eq!(
		fake.blobs(),
		fake.oids_where(|p| p.starts_with("skills/s4/"))
	);
}

/// Acceptance 6: the consumers judge a proven folder through the proof, not
/// through the (empty) disk — check's probe and source diff's classifier.
#[test]
fn a_proven_but_unmaterialized_folder_reads_current() {
	let local = tempfile::tempdir().unwrap();
	write_local(local.path(), &skill_files("demo"));
	let manifest = Arc::new(LocalManifest::read(local.path()).unwrap());
	let upstream = tempfile::tempdir().unwrap();
	// What a catalog verify leaves on disk for a proven skill: its SKILL.md.
	write_local(
		&upstream.path().join("skills/demo"),
		&skill_files("demo")[..1],
	);
	let repo = FetchedRepo::new(
		upstream.path().to_path_buf(),
		RepoSnapshot::default(),
		None,
	)
	.with_verified(crate::VerifiedFolders {
		proven: [("skills/demo".to_string(), Arc::clone(&manifest))].into(),
		handled: ["skills/demo".to_string()].into(),
	});

	let mut member = entry("demo", local.path(), None);
	member.local_manifest = Some(Arc::clone(&manifest));
	let probe =
		crate::probe_skill_hash_in_repo(&repo, member.skill_path.as_deref());
	assert_eq!(
		crate::classify_member_from_probe(&member, &probe, None).status,
		SkillUpdateStatus::UpToDate
	);

	let mut baseline = crate::sources::Baseline::new();
	baseline.insert(
		"skills/demo/SKILL.md".to_string(),
		crate::sources::BaselineEntry {
			installed_name: "demo".to_string(),
			stored_hash: manifest.raw.clone(),
			local_comparison_hash: Some(manifest.comparison.clone()),
			local_ambiguous: false,
			local_manifest: Some(manifest),
			scope_label: "global".to_string(),
			ref_name: None,
		},
	);
	let rows = crate::sources::classify_repo_skills(&repo, &baseline, None);
	assert_eq!(rows.len(), 1);
	assert_eq!(rows[0].state, SourceSkillState::InstalledCurrent);
}

/// The memo seam: a verified (partial) fetch serves only the folders it was
/// asked about.
#[test]
fn a_verified_fetch_covers_only_what_it_judged() {
	let repo =
		FetchedRepo::new("/nonexistent".into(), RepoSnapshot::default(), None)
			.with_verified(crate::VerifiedFolders {
				proven: HashMap::new(),
				handled: ["a".to_string()].into(),
			});
	let folder = |p: &str| InstalledFolder {
		folder: skill::SkillPath::parse(p).unwrap(),
		manifest: None,
	};
	assert!(repo.covers(&[folder("a")]));
	assert!(!repo.covers(&[folder("a"), folder("b")]));
	assert!(!repo.is_complete());
	let full =
		FetchedRepo::new("/nonexistent".into(), RepoSnapshot::default(), None);
	assert!(full.covers(&[folder("b")]) && full.is_complete());
}

fn write_project_lock(root: &Path, entries: &[(&str, &str, &str)]) {
	let mut lock = skill::LocalSkillLockFile::new();
	for (name, skill_path, hash) in entries {
		lock.skills.insert(
			name.to_string(),
			skill::LocalSkillLockEntry {
				extra: Default::default(),
				source_url: Some("https://github.com/acme/skills.git".into()),
				source: "acme/skills".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				skill_path: Some(skill_path.to_string()),
				computed_hash: hash.to_string(),
				ref_commit: None,
			},
		);
	}
	skill::write_local_lock(&lock, Some(root)).unwrap();
}

/// Acceptance 5: `source diff` over a large catalog with few installed,
/// unchanged skills downloads the catalog's SKILL.md files and CHANGELOG.md —
/// plus, here, the one installed folder whose SKILL.md really changed (an
/// in-place rename). Uninstalled rows keep their metadata; installed rows are
/// current; both rename paths (frontmatter, CHANGELOG) still report.
#[test]
fn source_diff_downloads_catalog_skill_md_and_changelog_only() {
	let project = tempfile::tempdir().unwrap();
	let mut upstream = BTreeMap::new();
	let mut lock = Vec::new();
	let mut hashes = Vec::new();
	// Installed and unchanged.
	for name in ["alpha", "beta"] {
		let files = skill_files(name);
		let dir = project.path().join(".claude/skills").join(name);
		write_local(&dir, &files);
		upstream_add(&mut upstream, &format!("skills/{name}"), &files);
		hashes.push((name, skill::compute_skill_folder_hash(&dir).unwrap()));
	}
	// Installed as `gamma-old`; upstream's frontmatter now says `gamma`.
	let gamma_dir = project.path().join(".claude/skills/gamma-old");
	write_local(&gamma_dir, &skill_files("gamma-old"));
	upstream_add(&mut upstream, "skills/gamma", &skill_files("gamma"));
	let gamma_hash = skill::compute_skill_folder_hash(&gamma_dir).unwrap();
	// Installed `old-name`, gone upstream; CHANGELOG names its successor.
	let old_dir = project.path().join(".claude/skills/old-name");
	write_local(&old_dir, &skill_files("old-name"));
	let old_hash = skill::compute_skill_folder_hash(&old_dir).unwrap();
	// Not installed.
	for name in ["d1", "d2", "d3", "d4", "new-name"] {
		upstream_add(
			&mut upstream,
			&format!("skills/{name}"),
			&skill_files(name),
		);
	}
	upstream.insert(
		"CHANGELOG.md".to_string(),
		(
			StagedEntryMode::Regular,
			b"- Rename the **`old-name`** skill to **`new-name`**.\n".to_vec(),
		),
	);
	upstream.insert(
		"README.md".to_string(),
		(StagedEntryMode::Regular, b"never needed".to_vec()),
	);
	for (name, hash) in &hashes {
		lock.push((*name, format!("skills/{name}/SKILL.md"), hash.clone()));
	}
	lock.push(("gamma-old", "skills/gamma/SKILL.md".into(), gamma_hash));
	lock.push(("old-name", "skills/old-name/SKILL.md".into(), old_hash));
	let lock: Vec<(&str, &str, &str)> = lock
		.iter()
		.map(|(n, p, h)| (*n, p.as_str(), h.as_str()))
		.collect();
	write_project_lock(project.path(), &lock);

	let fake = FakeGithub::new(upstream);
	let fetcher = fetcher(&fake);
	let outcome = diff_source(
		SourceDiffInput {
			source: "acme/skills".to_string(),
			git_ref: None,
			scopes: vec![WriteScope::project(project.path())],
		},
		SourceDiffDeps {
			fetcher: &fetcher,
			resolver: &NoToken,
		},
	);
	let SourceDiffOutcome::Ok { skills, .. } = outcome else {
		panic!("expected Ok, got {outcome:?}");
	};
	let row = |name: &str| {
		skills
			.iter()
			.find(|s| s.name == name)
			.unwrap_or_else(|| panic!("no row {name}: {skills:#?}"))
	};

	assert_eq!(
		fake.blobs(),
		fake.oids_where(|p| p.ends_with("/SKILL.md")
			|| p == "CHANGELOG.md"
			|| p.starts_with("skills/gamma/")),
		"catalog SKILL.md + CHANGELOG + the one changed installed folder"
	);
	for name in ["alpha", "beta"] {
		assert_eq!(row(name).state, SourceSkillState::InstalledCurrent);
	}
	assert_eq!(row("gamma").state, SourceSkillState::Renamed);
	assert_eq!(row("gamma").previous_name.as_deref(), Some("gamma-old"));
	assert_eq!(row("new-name").state, SourceSkillState::Renamed);
	assert_eq!(row("new-name").previous_name.as_deref(), Some("old-name"));
	let d1 = row("d1");
	assert_eq!(d1.state, SourceSkillState::NotInstalled);
	assert_eq!(d1.description.as_deref(), Some("d1 does things"));
	assert_eq!(d1.version.as_deref(), Some("1.2.0"));
	assert_eq!(d1.author.as_deref(), Some("acme"));
	assert_eq!(skills.len(), 8, "{skills:#?}");
}

/// The fetch never asks the gix slot, and an all-proven check selection does
/// not call `materialize` at all (an empty selection is not trusted).
#[test]
fn an_all_proven_selection_materializes_nothing() {
	let (_local, upstream, entries) = unchanged_group();
	let fake = FakeGithub::new(upstream);
	let repo = SkillRepository::with_backends(
		Some(Arc::new(GithubRest::new(
			Arc::clone(&fake) as Arc<dyn HttpTransport>
		))),
		Arc::new(NeverGix),
	);
	let installed: Vec<InstalledFolder> = entries
		.iter()
		.map(|e| InstalledFolder {
			folder: crate::skill_folder_from_lock_path(
				e.skill_path.as_deref().unwrap(),
			)
			.unwrap(),
			manifest: e.local_manifest.clone(),
		})
		.collect();
	let sr = SourceRef {
		source: "https://github.com/acme/skills.git".to_string(),
		ref_: Some("main".to_string()),
	};
	let pinned = repo.resolve_pinned(&sr, None).unwrap();
	let fetched = repo
		.fetch_pinned(
			&pinned,
			FetchSelection::Verify {
				installed: &installed,
				catalog: false,
			},
		)
		.unwrap();
	assert_eq!(std::fs::read_dir(&fetched.root).unwrap().count(), 0);
	for name in NAMES {
		assert!(
			fetched.proven(&format!("skills/{name}")).is_some(),
			"{name}"
		);
	}
}

/// A lock `skillPath` without the `/SKILL.md` suffix (`skill_folder_from_lock_path`
/// accepts it) must hash THAT folder, not its parent — the parent holds sibling
/// skills, so hashing it reads as a false UpdateAvailable.
#[test]
fn suffixless_lock_skill_path_hashes_the_skill_folder() {
	let upstream = tempfile::tempdir().unwrap();
	write_local(&upstream.path().join("skills/demo"), &skill_files("demo"));
	write_local(&upstream.path().join("skills/other"), &skill_files("other"));
	let repo = FetchedRepo::new(
		upstream.path().to_path_buf(),
		RepoSnapshot::default(),
		None,
	);

	let probe = crate::probe_skill_hash_in_repo(&repo, Some("skills/demo"));

	let expected = skill::compute_skill_folder_hashes(
		&upstream.path().join("skills/demo"),
	)
	.unwrap();
	match probe {
		crate::HashProbe::Fresh {
			hash,
			comparison_hash,
			name,
		} => {
			assert_eq!((hash, comparison_hash), expected);
			assert_eq!(name.as_deref(), Some("demo"));
		}
		_ => panic!("expected a fresh probe"),
	}
}
