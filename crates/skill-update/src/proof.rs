//! Tree-first content proof: decide from a snapshot's tree listing alone that
//! an upstream skill folder is byte-identical to an installed copy, so the
//! folder never has to be downloaded to be judged.
//!
//! A git tree lists every file's blob oid (SHA-1 over `blob <len>\0<bytes>`).
//! [`LocalManifest::read`] computes the same oids for the installed copy in the
//! SAME walk and the SAME read that produce its folder hashes. When the
//! upstream file list equals the local one, the upstream folder hashes equal
//! the local ones by construction. Anything uncertain answers "not proven",
//! which falls back to materializing the folder — never the reverse.

use std::collections::HashMap;
use std::path::Path;

use aghub_git::{BlobHasher, RepoTree, StagedEntryMode};
use skill::{FileEvent, HashError};

/// The installed copy of one skill folder, as one walk saw it: its raw and
/// comparison folder hashes plus each hashed file's git blob oid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalManifest {
	/// Raw folder hash (`skill::compute_skill_folder_hash`).
	pub raw: String,
	/// Comparison folder hash (`skill::compute_skill_folder_comparison_hash`).
	pub comparison: String,
	/// `(relative path, blob oid)` sorted by path; `None` when a file's bytes
	/// did not match the length its handle reported (changed under the read),
	/// when a hashed spelling does not name its native path, or when the copier
	/// drops an entry — this walk cannot vouch for a file list then.
	files: Option<Vec<(String, String)>>,
	/// Frontmatter `name` of the folder's `SKILL.md`, parsed from the bytes
	/// the walk read (what a fetched copy's parse would yield when proven).
	skill_name: Option<String>,
}

impl LocalManifest {
	/// Hash `dir` once, recording every hashed file's blob oid.
	pub fn read(dir: &Path) -> Result<Self, HashError> {
		let mut files = Vec::new();
		let mut intact = true;
		let mut current: Option<(String, BlobHasher)> = None;
		let mut skill_md: Option<Vec<u8>> = None;
		let mut capturing = false;
		let (raw, comparison) = skill::compute_skill_folder_hashes_with(
			dir,
			|event| match event {
				FileEvent::Begin { rel, native, len } => {
					// A listing only stands for the folder when every entry's
					// hashed spelling IS its native path, and when the copier
					// keeps it (an entry it drops hashes into the raw listing
					// but not into the comparison, so equal listings would no
					// longer imply equal comparison hashes).
					if !spelled_natively(rel, native)
						|| !skill::carried_into_master(native)
					{
						intact = false;
					}
					current = Some((rel.to_string(), BlobHasher::new(len)));
					capturing = rel == "SKILL.md";
					if capturing {
						skill_md = Some(Vec::new());
					}
				}
				FileEvent::Bytes(chunk) => {
					if let Some((_, hasher)) = current.as_mut() {
						hasher.update(chunk);
					}
					if capturing {
						if let Some(bytes) = skill_md.as_mut() {
							bytes.extend_from_slice(chunk);
						}
					}
				}
				FileEvent::End => {
					capturing = false;
					if let Some((rel, hasher)) = current.take() {
						match hasher.finish() {
							Some(oid) => files.push((rel, oid)),
							None => intact = false,
						}
					}
				}
			},
		)?;
		files.sort();
		let skill_name = skill_md
			.and_then(|bytes| String::from_utf8(bytes).ok())
			.and_then(|content| skill::parse_skill_md(&content).ok())
			.map(|skill| skill.name);
		Ok(Self {
			raw,
			comparison,
			files: intact.then_some(files),
			skill_name,
		})
	}

	/// Frontmatter `name` of this copy's `SKILL.md`, when it parses.
	pub fn skill_name(&self) -> Option<&str> {
		self.skill_name.as_deref()
	}
}

/// Whether `rel` (the hash's `/`-joined spelling) names exactly `native`: no
/// component holds a `\` (a name character on Unix, collapsed into a separator
/// by the spelling) and every component is UTF-8 (else the spelling is lossy).
fn spelled_natively(rel: &str, native: &Path) -> bool {
	let mut parts = Vec::new();
	for part in native.components() {
		match part.as_os_str().to_str() {
			Some(name) if !name.contains('\\') => parts.push(name),
			_ => return false,
		}
	}
	parts.join("/") == rel
}

/// Whether `tree`'s entries under `folder` are exactly `manifest`'s files —
/// i.e. materializing `folder` would reproduce the local copy byte for byte,
/// so the upstream folder hashes ARE `manifest.raw` / `manifest.comparison`.
///
/// `false` (materialize instead) whenever the comparison could be fooled: the
/// root skill (its size preflight must still run), a symlink entry (staged as a
/// link the hash skips), a `.git` / `node_modules` component (skipped by the
/// hash), a path staging would refuse, a non-ASCII path or two paths equal
/// under ASCII case-folding (the local filesystem may have rewritten either),
/// an entry the Master copier drops (`skill::carried_into_master`: raw-list
/// equality no longer implies comparison equality), a manifest that cannot
/// vouch for its file list (including a local name its hashed spelling
/// aliases), or no `SKILL.md` (a fetched
/// copy without one is unreadable as a skill, which a proof must not hide).
pub(crate) fn proves(
	tree: &RepoTree,
	folder: &str,
	manifest: &LocalManifest,
) -> bool {
	if folder.is_empty() {
		return false;
	}
	let Some(local) = manifest.files.as_deref() else {
		return false;
	};
	let prefix = format!("{folder}/");
	let mut upstream: Vec<(String, String)> = Vec::new();
	// Lower-cased path (and every ancestor) -> its original spelling.
	let mut folded: HashMap<String, &str> = HashMap::new();
	for entry in &tree.entries {
		let Some(rel) = entry.path.strip_prefix(&prefix) else {
			continue;
		};
		if entry.mode == StagedEntryMode::Symlink
			|| !skill::carried_into_master(Path::new(rel))
			|| !entry.path.is_ascii()
			|| !aghub_git::is_valid_entry_path(&entry.path)
			|| entry
				.path
				.split('/')
				.any(|part| part == ".git" || part == "node_modules")
		{
			return false;
		}
		let mut end = 0;
		for part in rel.split('/') {
			end += part.len();
			let spelled = &rel[..end];
			let previous = folded.insert(spelled.to_ascii_lowercase(), spelled);
			if previous.is_some_and(|previous| previous != spelled) {
				return false;
			}
			end += 1;
		}
		if matches!(
			entry.mode,
			StagedEntryMode::Regular | StagedEntryMode::Executable
		) {
			upstream.push((rel.to_string(), entry.oid.to_ascii_lowercase()));
		}
	}
	upstream.sort();
	upstream.iter().any(|(rel, _)| rel == "SKILL.md") && upstream == local
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_git::TreeEntry;

	fn oid_of(bytes: &[u8]) -> String {
		let mut hasher = BlobHasher::new(bytes.len() as u64);
		hasher.update(bytes);
		hasher.finish().unwrap()
	}

	fn entry(path: &str, mode: StagedEntryMode, bytes: &[u8]) -> TreeEntry {
		TreeEntry {
			path: path.to_string(),
			mode,
			oid: oid_of(bytes),
			size: Some(bytes.len() as u64),
		}
	}

	const SKILL: &[u8] = b"---\nname: demo\ndescription: d\n---\nbody\n";

	/// A local copy plus the tree that would reproduce it exactly.
	fn fixture() -> (tempfile::TempDir, LocalManifest, Vec<TreeEntry>) {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), SKILL).unwrap();
		std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
		std::fs::write(dir.path().join("scripts/run.sh"), b"echo hi\n")
			.unwrap();
		let manifest = LocalManifest::read(dir.path()).unwrap();
		let entries = vec![
			entry("README.md", StagedEntryMode::Regular, b"unrelated"),
			entry("skills/demo/SKILL.md", StagedEntryMode::Regular, SKILL),
			entry(
				"skills/demo/scripts/run.sh",
				StagedEntryMode::Executable,
				b"echo hi\n",
			),
		];
		(dir, manifest, entries)
	}

	fn tree(entries: Vec<TreeEntry>) -> RepoTree {
		RepoTree { entries }
	}

	#[test]
	fn manifest_matches_local_hashes_and_git_oids() {
		let (dir, manifest, _) = fixture();
		let (raw, comparison) =
			skill::compute_skill_folder_hashes(dir.path()).unwrap();
		assert_eq!(
			(manifest.raw.clone(), manifest.comparison.clone()),
			(raw, comparison)
		);
		assert_eq!(manifest.skill_name(), Some("demo"));
		// `git hash-object` of "echo hi\n".
		assert_eq!(
			manifest.files.as_deref().unwrap()[1],
			(
				"scripts/run.sh".to_string(),
				"8b2fe5434fec16870a71cd8b272c7fcf6d352536".to_string()
			)
		);
	}

	#[test]
	fn identical_listing_proves_and_exec_bit_is_irrelevant() {
		let (_dir, manifest, entries) = fixture();
		assert!(proves(&tree(entries), "skills/demo", &manifest));
	}

	#[test]
	fn a_changed_missing_or_extra_file_does_not_prove() {
		let (dir, manifest, mut entries) = fixture();
		let mut changed = entries.clone();
		changed[2] = entry(
			"skills/demo/scripts/run.sh",
			StagedEntryMode::Regular,
			b"echo bye\n",
		);
		assert!(!proves(&tree(changed), "skills/demo", &manifest));
		let missing: Vec<_> = entries[..2].to_vec();
		assert!(!proves(&tree(missing), "skills/demo", &manifest));
		entries.push(entry(
			"skills/demo/new.md",
			StagedEntryMode::Regular,
			b"",
		));
		assert!(!proves(&tree(entries.clone()), "skills/demo", &manifest));
		// Local extra file (an untracked note) is a mismatch too.
		std::fs::write(dir.path().join("notes.md"), b"mine").unwrap();
		let with_note = LocalManifest::read(dir.path()).unwrap();
		entries.pop();
		assert!(!proves(&tree(entries), "skills/demo", &with_note));
	}

	#[test]
	fn every_fallback_rule_refuses_an_otherwise_equal_listing() {
		let (_dir, manifest, entries) = fixture();
		let with = |extra: TreeEntry| {
			let mut all = entries.clone();
			all.push(extra);
			tree(all)
		};
		let cases = [
			(
				"symlink",
				with(entry(
					"skills/demo/link",
					StagedEntryMode::Symlink,
					b"SKILL.md",
				)),
			),
			(
				".git component",
				with(entry(
					"skills/demo/.git/x",
					StagedEntryMode::Regular,
					b"",
				)),
			),
			(
				"node_modules component",
				with(entry(
					"skills/demo/node_modules/x",
					StagedEntryMode::Gitlink,
					b"",
				)),
			),
			(
				"unsafe path",
				with(entry("skills/demo/a\\b", StagedEntryMode::Regular, b"")),
			),
			(
				"non-ascii",
				with(entry(
					"skills/demo/caf\u{e9}",
					StagedEntryMode::Regular,
					b"",
				)),
			),
			(
				"case-fold collision",
				with(entry(
					"skills/demo/skill.md",
					StagedEntryMode::Regular,
					b"",
				)),
			),
			(
				"file/dir case-fold collision",
				with(entry(
					"skills/demo/SCRIPTS",
					StagedEntryMode::Regular,
					b"",
				)),
			),
		];
		for (label, tree) in cases {
			assert!(!proves(&tree, "skills/demo", &manifest), "{label}");
		}
	}

	/// On Unix `\` is part of a NAME: the local file `__pycache__\payload`
	/// is carried into a Master, but its hash rel `__pycache__/payload` reads
	/// like upstream's excluded `__pycache__/payload`. Equal raw listings must
	/// not prove a folder whose comparison hashes differ.
	#[cfg(unix)]
	#[test]
	fn a_backslash_name_never_aliases_an_upstream_path() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), SKILL).unwrap();
		std::fs::write(dir.path().join("__pycache__\\payload"), b"p").unwrap();
		let manifest = LocalManifest::read(dir.path()).unwrap();
		let listing = tree(vec![
			entry("skills/demo/SKILL.md", StagedEntryMode::Regular, SKILL),
			entry(
				"skills/demo/__pycache__/payload",
				StagedEntryMode::Regular,
				b"p",
			),
		]);
		assert!(!proves(&listing, "skills/demo", &manifest));
	}

	/// Raw-list equality implies comparison equality only when nothing on
	/// either side is dropped by the Master copier's projection.
	#[test]
	fn an_entry_the_copier_drops_never_proves() {
		for rel in ["__pycache__/x.pyc", "__pypackages__/x", "metadata.json"] {
			let dir = tempfile::tempdir().unwrap();
			std::fs::write(dir.path().join("SKILL.md"), SKILL).unwrap();
			let path = dir.path().join(rel);
			std::fs::create_dir_all(path.parent().unwrap()).unwrap();
			std::fs::write(&path, b"x").unwrap();
			let manifest = LocalManifest::read(dir.path()).unwrap();
			let listing = tree(vec![
				entry("skills/demo/SKILL.md", StagedEntryMode::Regular, SKILL),
				entry(
					&format!("skills/demo/{rel}"),
					StagedEntryMode::Regular,
					b"x",
				),
			]);
			assert!(!proves(&listing, "skills/demo", &manifest), "{rel}");
		}
	}

	#[test]
	fn a_gitlink_entry_is_ignored() {
		let (_dir, manifest, mut entries) = fixture();
		entries.push(entry("skills/demo/sub", StagedEntryMode::Gitlink, b""));
		assert!(proves(&tree(entries), "skills/demo", &manifest));
	}

	#[test]
	fn root_unvouched_and_skill_md_less_folders_never_prove() {
		let (_dir, manifest, entries) = fixture();
		let root_entries: Vec<_> = entries
			.iter()
			.skip(1)
			.map(|e| TreeEntry {
				path: e.path.trim_start_matches("skills/demo/").to_string(),
				..e.clone()
			})
			.collect();
		assert!(!proves(&tree(root_entries), "", &manifest));

		let unvouched = LocalManifest {
			files: None,
			..manifest.clone()
		};
		assert!(!proves(&tree(entries), "skills/demo", &unvouched));

		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("a.md"), b"x").unwrap();
		let no_skill_md = LocalManifest::read(dir.path()).unwrap();
		let listing =
			tree(vec![entry("s/a.md", StagedEntryMode::Regular, b"x")]);
		assert!(!proves(&listing, "s", &no_skill_md));
	}
}
