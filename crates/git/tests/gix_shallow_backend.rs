//! Ticket 05: the `RepoFetchBackend` trait + `GixShallow` backend.
//!
//! - `resolve` returns a `RepoSnapshot` whose `commit_oid` is the branch tip and
//!   whose `tree_oid` is DISTINCT from the commit oid (OID separation, ticket 01).
//! - `read_tree` lists the tip's file entries (repo-relative, mode-tagged).
//! - `read_blobs` returns the exact stored bytes for requested blob oids.
//! - `materialize` writes selected sub-trees through the ticket-04
//!   `stage_tree_entries` materializer, producing a folder BYTE-IDENTICAL to a
//!   real gix clone — including recreating an in-folder symlink as a symlink
//!   (which is the observable proof it routed through `stage_tree_entries` and
//!   not a naive blob-dump that would write the symlink as a regular file).
//!
//! Unix-gated: the exec bit + symlink are part of the byte-identity claim.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use aghub_git::{
	GixShallow, RepoFetchBackend, RepoSnapshot, RepoTree, SourceRef,
	StagedEntryMode,
};

fn git(args: &[&str], cwd: &Path) -> String {
	let out = Command::new("git")
		.args(args)
		.current_dir(cwd)
		// C locale: `verify-pack` output below is parsed positionally.
		.env("LC_ALL", "C")
		.env("GIT_CONFIG_GLOBAL", "/dev/null")
		.env("GIT_CONFIG_SYSTEM", "/dev/null")
		.env("GIT_AUTHOR_NAME", "t")
		.env("GIT_AUTHOR_EMAIL", "t@t")
		.env("GIT_COMMITTER_NAME", "t")
		.env("GIT_COMMITTER_EMAIL", "t@t")
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"git {args:?} failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Origin repo with a sub-folder skill `skills/music/` containing a SKILL.md, a
/// nested executable, and an in-folder symlink. Returns the origin path.
fn build_origin(root: &Path) -> std::path::PathBuf {
	let origin = root.join("origin");
	let skill = origin.join("skills/music");
	std::fs::create_dir_all(skill.join("scripts")).unwrap();
	std::fs::write(
		skill.join("SKILL.md"),
		b"---\nname: music\ndescription: a sub-folder skill fixture\n---\n# body\n",
	)
	.unwrap();
	let sh = skill.join("scripts/run.sh");
	std::fs::write(&sh, b"#!/bin/sh\necho hi\n").unwrap();
	std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755))
		.unwrap();
	// In-folder symlink → must be recreated AS a symlink (clone parity).
	std::os::unix::fs::symlink("SKILL.md", skill.join("link.md")).unwrap();
	// An unrelated top-level file, to confirm materialize takes ONLY the folder.
	std::fs::write(origin.join("UNRELATED.txt"), b"noise\n").unwrap();

	git(&["init", "-q", "-b", "main"], &origin);
	git(&["add", "-A"], &origin);
	git(&["commit", "-q", "-m", "init"], &origin);
	origin
}

/// A real gix clone + worktree checkout → ground truth for byte-identity.
fn gix_checkout(origin: &Path, dest: &Path) {
	let url = format!("file://{}", origin.display());
	let (mut checkout, _) = gix::clone::PrepareFetch::new(
		url.as_str(),
		dest,
		gix::create::Kind::WithWorktree,
		Default::default(),
		Default::default(),
	)
	.unwrap()
	.fetch_then_checkout(
		gix::progress::Discard,
		&gix::interrupt::IS_INTERRUPTED,
	)
	.unwrap();
	checkout
		.main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
		.unwrap();
}

/// (relative-path, kind:payload) for every entry under `root`, excluding `.git`.
/// A symlink records its target; a file records exec-bit + content hex — so a
/// symlink written as a regular file would NOT match a symlink target entry.
fn snapshot(root: &Path) -> BTreeSet<(String, String)> {
	let mut set = BTreeSet::new();
	collect(root, root, &mut set);
	set
}

fn collect(root: &Path, dir: &Path, set: &mut BTreeSet<(String, String)>) {
	for e in std::fs::read_dir(dir).unwrap() {
		let e = e.unwrap();
		let p = e.path();
		let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
		let ft = e.file_type().unwrap();
		if ft.is_symlink() {
			let target = std::fs::read_link(&p).unwrap();
			set.insert((rel, format!("symlink:{}", target.display())));
		} else if ft.is_dir() {
			if e.file_name() == ".git" {
				continue;
			}
			collect(root, &p, set);
		} else {
			let bytes = std::fs::read(&p).unwrap();
			let exec = std::fs::metadata(&p).unwrap().permissions().mode()
				& 0o111 != 0;
			set.insert((rel, format!("file:exec={exec}:{}", hex(&bytes))));
		}
	}
}

fn hex(bytes: &[u8]) -> String {
	use std::fmt::Write;
	bytes.iter().fold(String::new(), |mut s, b| {
		let _ = write!(s, "{b:02x}");
		s
	})
}

fn source_ref(origin: &Path) -> SourceRef {
	SourceRef {
		url: format!("file://{}", origin.display()),
		ref_: Some("main".to_string()),
	}
}

#[test]
fn resolve_returns_tip_commit_with_distinct_tree_oid() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());

	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();

	// commit_oid must be the branch tip; tree_oid is a DIFFERENT object id (the
	// tip's root tree) — never conflated (ticket 01 OID separation).
	let repo = gix::open(&origin).unwrap();
	let tip = repo.head_id().unwrap().detach().to_string();
	let tip_tree = repo.head_tree().unwrap().id.to_string();
	assert_eq!(snap.commit_oid, tip, "snapshot commit_oid must be the tip");
	assert_eq!(
		snap.tree_oid, tip_tree,
		"snapshot tree_oid must be the root tree"
	);
	assert_ne!(
		snap.commit_oid, snap.tree_oid,
		"commit and tree oids must stay distinct"
	);
}

#[test]
fn read_tree_lists_file_entries_with_modes() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();

	let tree = backend.read_tree(&snap).unwrap();
	let find = |path: &str| tree.entries.iter().find(|e| e.path == path);

	assert!(
		matches!(find("skills/music/SKILL.md"), Some(e) if e.mode == StagedEntryMode::Regular),
		"SKILL.md must be listed as a regular file"
	);
	assert!(
		matches!(find("skills/music/scripts/run.sh"), Some(e) if e.mode == StagedEntryMode::Executable),
		"run.sh must be listed as executable (mode 100755)"
	);
	assert!(
		matches!(find("skills/music/link.md"), Some(e) if e.mode == StagedEntryMode::Symlink),
		"link.md must be listed as a symlink (mode 120000)"
	);
	assert!(
		find("UNRELATED.txt").is_some(),
		"read_tree lists the whole tip"
	);
}

#[test]
fn read_blobs_returns_exact_stored_bytes() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();

	let tree = backend.read_tree(&snap).unwrap();
	let entry = tree
		.entries
		.iter()
		.find(|e| e.path == "skills/music/SKILL.md")
		.unwrap();

	let blobs = backend
		.read_blobs(&snap, std::slice::from_ref(&entry.oid))
		.unwrap();
	assert_eq!(blobs.len(), 1);
	assert_eq!(blobs[0].oid, entry.oid);
	assert_eq!(
		blobs[0].bytes,
		b"---\nname: music\ndescription: a sub-folder skill fixture\n---\n# body\n"
	);
}

#[test]
fn materialize_selected_folder_is_byte_identical_to_clone() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());

	// Ground truth: a real gix clone + checkout of the whole repo.
	let ground = tmp.path().join("ground");
	gix_checkout(&origin, &ground);

	// Under test: resolve + materialize ONLY the skills/music sub-tree.
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();
	let dest = tmp.path().join("staged");
	backend
		.materialize(&snap, &["skills/music"], &dest)
		.unwrap();

	// The materialized folder must be byte-identical to the clone's folder —
	// exec bit preserved and the symlink recreated AS a symlink (proof it went
	// through stage_tree_entries, not a naive blob dump).
	assert_eq!(
		snapshot(&dest.join("skills/music")),
		snapshot(&ground.join("skills/music")),
		"materialized skill folder must be byte-identical to the gix clone"
	);

	// And it must hash identically under the Source-hash — the round-trip anchor.
	let h_staged =
		skill::compute_skill_folder_hash(&dest.join("skills/music")).unwrap();
	let h_ground =
		skill::compute_skill_folder_hash(&ground.join("skills/music")).unwrap();
	assert_eq!(h_staged, h_ground, "Source hash must equal the clone's");

	// ONLY the selected folder was written — the unrelated top-level file is not.
	assert!(
		!dest.join("UNRELATED.txt").exists(),
		"materialize must write only the selected sub-tree"
	);
}

/// `TreeEntry.size` must stay the DECOMPRESSED byte length whichever way the
/// walk obtains it (full object vs object header) — `root_size_preflight` sums
/// these to refuse oversized root skills.
#[test]
fn read_tree_sizes_match_decompressed_blob_lengths() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();

	let tree = backend.read_tree(&snap).unwrap();
	assert!(tree.entries.len() >= 4, "fixture must have several entries");

	for entry in &tree.entries {
		match entry.mode {
			StagedEntryMode::Symlink | StagedEntryMode::Gitlink => assert_eq!(
				entry.size, None,
				"{} must not declare a size",
				entry.path
			),
			StagedEntryMode::Regular | StagedEntryMode::Executable => {
				let blobs = backend
					.read_blobs(&snap, std::slice::from_ref(&entry.oid))
					.unwrap();
				assert_eq!(
					entry.size,
					Some(blobs[0].bytes.len() as u64),
					"declared size for {} must equal its decompressed length",
					entry.path
				);
			}
		}
	}
}

/// Walking a pinned tree decompresses every blob just to read its length, and
/// `materialize` walks it again. One walk per snapshot: drop the cache and the
/// second read of an unresolved-but-same-tree snapshot goes back to Err.
#[test]
fn read_tree_is_served_from_cache_after_the_first_walk() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();

	// A snapshot whose commit was never resolved can only be answered from a
	// tree cache — before the first walk there is none.
	let probe = RepoSnapshot {
		commit_oid: "0".repeat(40),
		tree_oid: snap.tree_oid.clone(),
		commit_time: None,
	};
	assert!(
		backend.read_tree(&probe).is_err(),
		"nothing walked yet -> no cache to answer from"
	);

	let walked = backend.read_tree(&snap).unwrap();
	let cached = backend
		.read_tree(&probe)
		.expect("the walked tree must be served from cache");

	// Vec, not a set: a cache hit must reproduce the walk order too, because
	// `SkillRepository::list` feeds that order to skill discovery.
	let listing = |t: &RepoTree| {
		t.entries
			.iter()
			.map(|e| {
				(
					e.path.clone(),
					format!("{:?}", e.mode),
					e.oid.clone(),
					e.size,
				)
			})
			.collect::<Vec<_>>()
	};
	assert_eq!(
		listing(&walked),
		listing(&cached),
		"the cached listing must be identical to the walked one"
	);
}

/// Origin whose tip holds several same-sized, progressively-mutated blobs: each
/// `vN` differs from `v(N-1)` by ONE block and from `v0` by N blocks, so the
/// cheapest base for `vN` is `v(N-1)` and pack-objects builds a real delta
/// CHAIN rather than a flat set of depth-1 deltas.
fn build_delta_origin(root: &Path) -> std::path::PathBuf {
	let origin = root.join("delta-origin");
	std::fs::create_dir_all(origin.join("skills/big")).unwrap();

	let mut blocks: Vec<String> = (0..8)
		.map(|b| {
			(0..1000)
				.map(|i| format!("block {b} line {i} lorem ipsum dolor sit\n"))
				.collect()
		})
		.collect();
	for v in 0..8 {
		if v > 0 {
			blocks[v - 1] = (0..1000)
				.map(|i| format!("MUTATED {v} line {i} lorem ipsum dolor si\n"))
				.collect();
		}
		std::fs::write(
			origin.join(format!("skills/big/v{v}.txt")),
			blocks.concat(),
		)
		.unwrap();
	}
	std::fs::write(
		origin.join("skills/big/SKILL.md"),
		b"---\nname: big\ndescription: packed delta fixture\n---\n",
	)
	.unwrap();

	git(&["init", "-q", "-b", "main"], &origin);
	git(&["add", "-A"], &origin);
	git(&["commit", "-q", "-m", "init"], &origin);
	origin
}

/// `walk_tree` reads `TreeEntry.size` from the object HEADER instead of
/// decompressing the blob. For a packed DELTA that size comes out of the delta
/// header, not the object itself — so this pins the premise the whole
/// optimization rests on: a delta's declared size is still the RESOLVED
/// (decompressed) length, at every depth of a delta chain.
///
/// `root_size_preflight` sums these to refuse an oversized root skill, so a
/// header size that reported the delta stream's length instead (as
/// `git verify-pack -v` does — 61 bytes for a 463 KB blob here) would silently
/// wave a huge repo past the guard.
#[test]
fn read_tree_sizes_are_correct_for_packed_delta_blobs() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_delta_origin(tmp.path());
	let url = format!("file://{}", origin.display());

	// Fetch the way GixShallow does, but keep the temp dir so the pack the
	// backend will actually read can be inspected.
	let (temp, _tip) =
		aghub_git::fetch::fetch_ref_to_temp(&url, Some("main"), None, None)
			.unwrap();
	let idx = std::fs::read_dir(temp.path().join("objects/pack"))
		.expect("a fetch must produce a pack")
		.filter_map(|e| e.ok())
		.map(|e| e.file_name().to_string_lossy().into_owned())
		.find(|n| n.ends_with(".idx"))
		.expect("a fetch must produce a pack index");

	// `verify-pack -v`: `oid type size size-in-pack offset [depth base-oid]` —
	// the two trailing columns appear only on delta objects.
	let verify = git(
		&["verify-pack", "-v", &format!("objects/pack/{idx}")],
		temp.path(),
	);
	let depths: Vec<u32> = verify
		.lines()
		.filter_map(|line| {
			let c: Vec<_> = line.split_whitespace().collect();
			(c.len() == 7 && c[1] == "blob").then(|| c[5].parse().ok())?
		})
		.collect();
	// Guard the FIXTURE, not the code: without packed blob deltas — and without
	// a chain deeper than one level — this test proves nothing about headers.
	assert!(
		depths.len() >= 4 && depths.iter().any(|d| *d >= 2),
		"fixture stopped producing a packed blob delta chain (depths {depths:?}); \
		 the header-size claim would go untested"
	);

	// Same claim as `read_tree_sizes_match_decompressed_blob_lengths`, now over
	// blobs that only exist in the pack as deltas.
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();
	let tree = backend.read_tree(&snap).unwrap();
	let mut regular = 0;
	for entry in &tree.entries {
		if !matches!(
			entry.mode,
			StagedEntryMode::Regular | StagedEntryMode::Executable
		) {
			continue;
		}
		let blobs = backend
			.read_blobs(&snap, std::slice::from_ref(&entry.oid))
			.unwrap();
		assert_eq!(
			entry.size,
			Some(blobs[0].bytes.len() as u64),
			"declared size for {} must equal its decompressed length",
			entry.path
		);
		regular += 1;
	}
	assert!(regular >= 8, "expected the fixture's blobs, got {regular}");
}
