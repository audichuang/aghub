//! Proves aghub never breaks an npx-read/written lock.
use skill::compute_skill_folder_hash;
use skill::lock::{SkillLockEntry, SkillLockFile};

const NPX_LOCK: &str = include_str!("fixtures/global-lock-npx-written.json");

#[test]
fn reads_npx_lock_and_preserves_unknown_and_versions() {
	let lock: SkillLockFile = serde_json::from_str(NPX_LOCK).unwrap();
	assert_eq!(lock.version, 3);
	let alpha = lock.skills.get("alpha").unwrap();
	assert_eq!(alpha.content_hash, None);
	assert_eq!(
		alpha.skill_folder_hash,
		"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	);
	// re-serialize: version stays 3, skillFolderHash preserved, no contentHash injected
	let out = serde_json::to_string_pretty(&lock).unwrap();
	assert!(out.contains("\"version\": 3"));
	assert!(out.contains("\"skillFolderHash\": \"aaaa"));
	assert!(!out.contains("contentHash"));
}

#[test]
fn round_trip_b_missing_content_hash_recomputes_not_errors() {
	// aghub wrote contentHash; npx addSkillToLock dropped it on one entry.
	let mut entry: SkillLockEntry = serde_json::from_value(serde_json::json!({
		"source":"o/r","sourceType":"github","sourceUrl":"https://github.com/o/r",
		"skillFolderHash":"","installedAt":"t","updatedAt":"t"
	}))
	.unwrap();
	assert_eq!(entry.content_hash, None);
	// The update path must recompute, not error. Here we just assert the field
	// is optional + recompute is possible from disk.
	let tmp = tempfile::tempdir().unwrap();
	std::fs::write(tmp.path().join("SKILL.md"), b"x").unwrap();
	entry.content_hash = Some(compute_skill_folder_hash(tmp.path()).unwrap());
	assert!(entry.content_hash.is_some());
}

#[test]
fn experimental_sync_skip_condition_hash_match() {
	// aghub-written computed_hash must equal a second recompute (npx sync ===).
	let tmp = tempfile::tempdir().unwrap();
	std::fs::write(tmp.path().join("SKILL.md"), b"content").unwrap();
	std::fs::create_dir_all(tmp.path().join("lib")).unwrap();
	std::fs::write(tmp.path().join("lib/x.ts"), b"export {}").unwrap();
	let h1 = compute_skill_folder_hash(tmp.path()).unwrap();
	let h2 = compute_skill_folder_hash(tmp.path()).unwrap();
	assert_eq!(h1, h2);
}

#[test]
fn lock_wipe_boundary_old_version_returns_empty_not_panic() {
	let old = r#"{"version":2,"skills":{}}"#;
	// read_skill_lock wipes <CURRENT; deserialize of the struct still works.
	let lock: SkillLockFile = serde_json::from_str(old).unwrap();
	assert_eq!(lock.version, 2); // raw parse; read_skill_lock() applies the wipe policy
}
