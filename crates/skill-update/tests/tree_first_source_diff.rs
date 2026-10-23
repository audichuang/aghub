//! `source diff`'s tree-first cost, end to end through the real
//! `SkillRepository` over a fake GitHub REST transport. Every cost assertion
//! counts REQUESTS through the `HttpTransport` seam — never wall-clock.
//!
//! Its own binary because `diff_source` reads the installed copies through the
//! MANAGED agent set, i.e. the disabled-agent selection under
//! `$AGHUB_DATA_DIR`: without pinning it the developer's own selection decides
//! which copies count. Unix-only because Windows ignores `$HOME`.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use aghub_core::WriteScope;
use aghub_git::{
	BlobHasher, Credentials, GitError, GithubRest, HttpRequest, HttpResponse,
	HttpTransport, RepoFetchBackend, RepoSnapshot, RepoTree, StagedEntryMode,
};
use skill_update::sources::{
	diff_source, SourceDiffDeps, SourceDiffInput, SourceDiffOutcome,
	SourceSkillState,
};
use skill_update::{
	GitFetcher, SkillRepository, TokenResolution, TokenResolver,
};

const COMMIT: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";

fn env_lock() -> &'static Mutex<()> {
	static LOCK: Mutex<()> = Mutex::new(());
	&LOCK
}

struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

impl EnvVarGuard {
	fn set(key: &'static str, path: &Path) -> Self {
		let previous = std::env::var_os(key);
		std::env::set_var(key, path);
		EnvVarGuard(key, previous)
	}
}

impl Drop for EnvVarGuard {
	fn drop(&mut self) {
		match self.1.take() {
			Some(value) => std::env::set_var(self.0, value),
			None => std::env::remove_var(self.0),
		}
	}
}

/// HOME, XDG dirs and AGHUB_DATA_DIR in fresh tempdirs (no agent selection
/// file, so every agent is managed). Serialized; RAII restoration.
fn with_isolated_env<T>(f: impl FnOnce() -> T) -> T {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let home = tempfile::tempdir().unwrap();
	let xdg_state = tempfile::tempdir().unwrap();
	let xdg_data = tempfile::tempdir().unwrap();
	let xdg_config = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	// Declared AFTER the tempdirs so they drop FIRST.
	let _home_guard = EnvVarGuard::set("HOME", home.path());
	let _state_guard = EnvVarGuard::set("XDG_STATE_HOME", xdg_state.path());
	let _data_guard = EnvVarGuard::set("XDG_DATA_HOME", xdg_data.path());
	let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", xdg_config.path());
	let _aghub_guard = EnvVarGuard::set("AGHUB_DATA_DIR", data.path());
	f()
}

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

	/// Blob oids downloaded, as a set.
	fn blobs(&self) -> BTreeSet<String> {
		self.recorded
			.lock()
			.unwrap()
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

fn not_found() -> HttpResponse {
	HttpResponse {
		status: 404,
		headers: Vec::new(),
		body: Vec::new(),
	}
}

impl HttpTransport for FakeGithub {
	fn execute(&self, request: HttpRequest) -> aghub_git::Result<HttpResponse> {
		let url = request.url;
		self.recorded.lock().unwrap().push(url.clone());
		if let Some(wanted) = url.split("/git/blobs/").nth(1) {
			return Ok(self
				.files
				.values()
				.find(|(_, bytes)| oid(bytes) == wanted)
				.map(|(_, bytes)| {
					response("application/vnd.github.raw", bytes.clone())
				})
				.unwrap_or_else(not_found));
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
		Ok(not_found())
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
	with_isolated_env(
		source_diff_downloads_catalog_skill_md_and_changelog_only_body,
	)
}

fn source_diff_downloads_catalog_skill_md_and_changelog_only_body() {
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
