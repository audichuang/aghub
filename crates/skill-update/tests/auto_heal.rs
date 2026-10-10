//! Auto-heal seam: the post-check compare-and-set heal that `GET
//! /skills/check-updates` runs (`projection::write_auto_healed_hashes`). Its
//! own test binary + a module-local env lock, because the global heal needs
//! `XDG_STATE_HOME` and `HOME` isolated — process-wide state that must never
//! race another binary's env-touching tests (see crates/core/AGENTS.md Testing).
//!
//! Every test asserts observable outcomes: lock contents on disk, the lock file
//! bytes, or the error the caller receives.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use aghub_core::skills::update::SkillUpdateStatus;
use skill_update::projection::{self, Identities};
use skill_update::{CheckOutput, EntryKey};

fn env_lock() -> &'static Mutex<()> {
	static LOCK: Mutex<()> = Mutex::new(());
	&LOCK
}

/// Sets one env var and restores the previous value on drop, including during a
/// panic, so a failing test cannot leak a deleted tempdir path into the binary.
struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

impl EnvVarGuard {
	fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
		let previous = std::env::var_os(key);
		std::env::set_var(key, value);
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

/// HOME, XDG_STATE_HOME and AGHUB_DATA_DIR in fresh tempdirs, serialized against
/// this binary's other tests. RAII restoration, so a panic cannot leak a deleted
/// tempdir path into a later test.
fn with_isolated_env<T>(f: impl FnOnce(&Path) -> T) -> T {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	// Declared AFTER the tempdirs so they drop FIRST: the env is restored to the
	// real values before these directories are deleted.
	let _home_guard = EnvVarGuard::set("HOME", home.path());
	let _state_guard = EnvVarGuard::set("XDG_STATE_HOME", state.path());
	let _data_guard = EnvVarGuard::set("AGHUB_DATA_DIR", data.path());
	f(home.path())
}

fn global_entry() -> skill::SkillLockEntry {
	skill::SkillLockEntry {
		source: "owner/repo".to_string(),
		source_type: "github".to_string(),
		source_url: "https://github.com/owner/repo".to_string(),
		ref_name: Some("main".to_string()),
		skill_path: Some("SKILL.md".to_string()),
		skill_folder_hash: String::new(),
		content_hash: None,
		ref_commit: None,
		installed_at: "t".to_string(),
		updated_at: "t".to_string(),
		plugin_name: None,
	}
}

fn healed_output(name: &str, scope: &str, hash: &str) -> CheckOutput {
	CheckOutput {
		key: EntryKey {
			name: name.to_string(),
			scope: scope.to_string(),
		},
		status: SkillUpdateStatus::UpToDate,
		heal_hash: Some(hash.to_string()),
		heal_oid: None,
	}
}

/// The identities production captures from the CURRENT global lock — i.e.
/// the same read that decides what a check fetches.
fn global_identities_now() -> Identities {
	projection::global_lock_entries(true, skill::lock::global::read_skill_lock)
		.1
}

#[test]
fn auto_heal_writes_global_content_hash() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		let mut entry = global_entry();
		entry.skill_folder_hash = "tree-v1".to_string();
		lock.skills.insert("legacy".into(), entry);
		skill::lock::global::write_skill_lock(&lock).unwrap();

		assert!(projection::write_auto_healed_hashes(
			&[healed_output("legacy", "global", "abc123")],
			None,
			&global_identities_now(),
			&Identities::new(),
		)
		.is_ok());

		let lock = skill::lock::global::read_skill_lock();
		assert_eq!(
			lock.skills["legacy"].content_hash.as_deref(),
			Some("abc123")
		);
		assert_eq!(lock.skills["legacy"].skill_folder_hash, "");
	});
}

#[test]
fn auto_heal_writes_global_ref_commit() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), global_entry());
		skill::lock::global::write_skill_lock(&lock).unwrap();

		// A freshly-fetched global member carries heal_oid (and no heal_hash);
		// write_auto_healed_hashes must still persist refCommit.
		let mut output = healed_output("legacy", "global", "ignored");
		output.heal_hash = None;
		output.heal_oid = Some("deadbeefcafef00d".to_string());

		assert!(projection::write_auto_healed_hashes(
			&[output],
			None,
			&global_identities_now(),
			&Identities::new(),
		)
		.is_ok());

		let lock = skill::lock::global::read_skill_lock();
		assert_eq!(
			lock.skills["legacy"].ref_commit.as_deref(),
			Some("deadbeefcafef00d")
		);
	});
}

/// The REAL shape of a legacy/npx heal: an entry with an unknown hash and no
/// refCommit produces BOTH `heal_hash` and `heal_oid` from one check, and
/// both must land in that single write. (`auto_heal_writes_global_ref_commit`
/// above forces `heal_hash = None`, so it cannot see the two interacting —
/// applying the hash moves the entry, and a second precondition check against
/// the pre-fetch snapshot then rejects the OID.)
#[test]
fn auto_heal_lands_hash_and_ref_commit_in_one_write() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), global_entry());
		skill::lock::global::write_skill_lock(&lock).unwrap();

		let mut output = healed_output("legacy", "global", "healed-hash");
		output.heal_oid = Some("deadbeefcafef00d".to_string());
		assert!(projection::write_auto_healed_hashes(
			&[output],
			None,
			&global_identities_now(),
			&Identities::new(),
		)
		.is_ok());

		let entry = &skill::lock::global::read_skill_lock().skills["legacy"];
		assert_eq!(entry.content_hash.as_deref(), Some("healed-hash"));
		assert_eq!(
			entry.ref_commit.as_deref(),
			Some("deadbeefcafef00d"),
			"the OID must land in the same write as the hash — otherwise the \
				 next check has to fetch the whole source again to re-derive it"
		);
	});
}

/// The read ORDER inside `global_lock_entries`, pinned deterministically: the
/// npx-style write happens while the check is between its two reads. Reading
/// the lock FIRST means the snapshot predates that write, so the writer's
/// precondition sees a live lock that has moved on and refuses the heal.
/// Hash-then-lock would snapshot npx's OWN state, pair it with the disk hash
/// read before npx ran, and sail through the precondition.
#[test]
fn a_check_snapshots_the_lock_before_hashing_disk() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		let mut entry = global_entry();
		entry.skill_folder_hash = "npx-tree-a".to_string();
		lock.skills.insert("legacy".into(), entry);
		skill::lock::global::write_skill_lock(&lock).unwrap();

		let (_entries, identities) = projection::global_lock_entries_with(
			skill::lock::global::read_skill_lock,
			|_wanted| {
				// npx finishes updating to tree B right here.
				let mut updated = global_entry();
				updated.skill_folder_hash = "npx-tree-b".to_string();
				let mut lock = skill::SkillLockFile::default();
				lock.skills.insert("legacy".into(), updated);
				skill::lock::global::write_skill_lock(&lock).unwrap();
				// The disk hash the check read: still the pre-npx tree.
				projection::LocalHashes {
					hashes: HashMap::from([(
						"legacy".to_string(),
						"disk-hash-a".to_string(),
					)]),
					..Default::default()
				}
			},
		);

		assert!(projection::write_auto_healed_hashes(
			&[healed_output("legacy", "global", "disk-hash-a")],
			None,
			&identities,
			&Identities::new(),
		)
		.is_ok());

		let entry = &skill::lock::global::read_skill_lock().skills["legacy"];
		assert_eq!(
			entry.skill_folder_hash, "npx-tree-b",
			"the heal was derived from the pre-npx disk hash; it must not \
				 land on npx's newer entry or blank its baseline"
		);
		assert_eq!(entry.content_hash, None);
	});
}

/// npx writes its own baseline into `skillFolderHash`, and
/// `apply_content_hash` CLEARS that field. So a concurrent `npx skills
/// update` — same source/ref/path, and it leaves `contentHash`/`refCommit`
/// untouched — is invisible to a precondition that only compares those two:
/// the stale heal would overwrite npx's newer state and blank the field npx
/// uses to decide whether to check for updates at all.
#[test]
fn auto_heal_skips_an_entry_npx_updated_during_the_fetch() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		let mut entry = global_entry();
		entry.skill_folder_hash = "npx-tree-a".to_string();
		lock.skills.insert("legacy".into(), entry);
		skill::lock::global::write_skill_lock(&lock).unwrap();
		let identities = global_identities_now();

		// Mid-fetch: npx updates the same entry to a newer tree.
		let mut updated = global_entry();
		updated.skill_folder_hash = "npx-tree-b".to_string();
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), updated);
		skill::lock::global::write_skill_lock(&lock).unwrap();

		assert!(projection::write_auto_healed_hashes(
			&[healed_output("legacy", "global", "stale-hash")],
			None,
			&identities,
			&Identities::new(),
		)
		.is_ok());

		let entry = &skill::lock::global::read_skill_lock().skills["legacy"];
		assert_eq!(
			entry.skill_folder_hash, "npx-tree-b",
			"a stale heal must not blank the folder hash npx just wrote"
		);
		assert_eq!(entry.content_hash, None);
	});
}

/// A check reads the lock, spends SECONDS fetching, and only then takes the
/// mutation lock to write its heals. If another process repoints the same
/// NAME at a different source in that window (and writes that source's own
/// correct hash), the stale heal must not land on the new entry — otherwise
/// every later check compares against a baseline that never belonged to it
/// and reports a phantom update forever.
#[test]
fn auto_heal_skips_an_entry_repointed_during_the_fetch() {
	with_isolated_env(|_home| {
		// Pre-fetch: `legacy` points at owner/repo. This is the read the
		// check's fetch is based on.
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), global_entry());
		skill::lock::global::write_skill_lock(&lock).unwrap();
		let identities = global_identities_now();

		// Mid-fetch: another mutation repoints the name at owner/other and
		// records THAT source's hash and tip.
		let mut repointed = global_entry();
		repointed.source = "owner/other".to_string();
		repointed.source_url = "https://github.com/owner/other".to_string();
		repointed.content_hash = Some("other-hash".to_string());
		repointed.ref_commit = Some("bbbbbbbb".to_string());
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), repointed);
		skill::lock::global::write_skill_lock(&lock).unwrap();

		// The stale check finally writes: owner/repo's hash and tip.
		let mut output = healed_output("legacy", "global", "repo-hash");
		output.heal_oid = Some("aaaaaaaa".to_string());
		assert!(projection::write_auto_healed_hashes(
			&[output],
			None,
			&identities,
			&Identities::new(),
		)
		.is_ok());

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["legacy"];
		assert_eq!(
			entry.content_hash.as_deref(),
			Some("other-hash"),
			"owner/repo's hash must not overwrite owner/other's entry"
		);
		assert_eq!(entry.ref_commit.as_deref(), Some("bbbbbbbb"));
	});
}

/// The same window, but the racing mutation is an `apply-update` on THIS
/// entry: the coordinates never change, only the hash and refCommit move
/// forward. An identity-only compare-and-set sees no difference and rolls
/// both back to what the stale check saw.
#[test]
fn auto_heal_skips_an_entry_updated_during_the_fetch() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), global_entry());
		skill::lock::global::write_skill_lock(&lock).unwrap();
		let identities = global_identities_now();

		// Mid-fetch: apply-update advances this entry to the new content.
		let mut updated = global_entry();
		updated.content_hash = Some("new-hash".to_string());
		updated.ref_commit = Some("cccccccc".to_string());
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), updated);
		skill::lock::global::write_skill_lock(&lock).unwrap();

		let mut output = healed_output("legacy", "global", "stale-hash");
		output.heal_oid = Some("aaaaaaaa".to_string());
		assert!(projection::write_auto_healed_hashes(
			&[output],
			None,
			&identities,
			&Identities::new(),
		)
		.is_ok());

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["legacy"];
		assert_eq!(
			entry.content_hash.as_deref(),
			Some("new-hash"),
			"a stale check must not roll back a newer apply-update"
		);
		assert_eq!(entry.ref_commit.as_deref(), Some("cccccccc"));
	});
}

#[test]
fn auto_heal_writes_project_computed_hash_only() {
	with_isolated_env(|_home| {
		let project = tempfile::tempdir().unwrap();
		let mut local = skill::LocalSkillLockFile::default();
		local.skills.insert(
			"legacy".into(),
			skill::LocalSkillLockEntry {
				source_url: None,
				ref_commit: None,
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "github".to_string(),
				computed_hash: skill::EMPTY_SKILLS_LOCK_DIGEST.to_string(),
				skill_path: Some("SKILL.md".to_string()),
			},
		);
		skill::lock::local::write_local_lock(&local, Some(project.path()))
			.unwrap();

		let (_entries, project_identities) = projection::project_lock_entries(
			true,
			Some(project.path()),
			|| skill::lock::local::read_local_lock(Some(project.path())),
		);
		assert!(projection::write_auto_healed_hashes(
			&[healed_output("legacy", "project", "def456")],
			Some(project.path()),
			&Identities::new(),
			&project_identities,
		)
		.is_ok());

		let local = skill::lock::local::read_local_lock(Some(project.path()));
		assert_eq!(local.skills["legacy"].computed_hash, "def456");
		assert!(
			skill::lock::global::read_skill_lock().skills.is_empty(),
			"project auto-heal must not touch the global lock"
		);
	});
}

/// A stale heal must neither overwrite a newer `apply-update` nor touch the lock
/// file at all: the precondition is the only thing standing in its way.
#[test]
fn heal_skips_when_precondition_does_not_match() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), global_entry());
		skill::lock::global::write_skill_lock(&lock).unwrap();
		let identities = global_identities_now();

		// Mid-check: apply-update advances this entry, so the snapshot is stale.
		let mut updated = global_entry();
		updated.content_hash = Some("apply-update-hash".to_string());
		updated.ref_commit = Some("newer-oid".to_string());
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), updated);
		skill::lock::global::write_skill_lock(&lock).unwrap();
		let before =
			std::fs::read(skill::lock::global::get_skill_lock_path()).unwrap();

		let mut output = healed_output("legacy", "global", "stale-hash");
		output.heal_oid = Some("stale-oid".to_string());
		assert!(projection::write_auto_healed_hashes(
			&[output],
			None,
			&identities,
			&Identities::new(),
		)
		.is_ok());

		let after =
			std::fs::read(skill::lock::global::get_skill_lock_path()).unwrap();
		assert_eq!(
			before, after,
			"a stale heal must not write the lock file at all"
		);
	});
}

#[test]
fn heal_writes_hash_and_ref_commit_when_precondition_matches() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		let mut entry = global_entry();
		entry.skill_folder_hash = "npx-tree".to_string();
		lock.skills.insert("legacy".into(), entry);
		skill::lock::global::write_skill_lock(&lock).unwrap();
		let identities = global_identities_now();

		let mut output = healed_output("legacy", "global", "healed-hash");
		output.heal_oid = Some("deadbeefcafef00d".to_string());
		assert!(projection::write_auto_healed_hashes(
			&[output],
			None,
			&identities,
			&Identities::new(),
		)
		.is_ok());

		let entry = &skill::lock::global::read_skill_lock().skills["legacy"];
		assert_eq!(entry.content_hash.as_deref(), Some("healed-hash"));
		assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
		assert_eq!(entry.skill_folder_hash, "");
	});
}

#[test]
fn heal_reports_retryable_busy_and_keeps_results_when_lock_is_held() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert("legacy".into(), global_entry());
		skill::lock::global::write_skill_lock(&lock).unwrap();
		let identities = global_identities_now();

		// Another aghub process holds the global mutation lock.
		let lock_path = skill::lock::MutationScope::Global.lock_path();
		std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
		let held = std::fs::File::options()
			.read(true)
			.write(true)
			.create(true)
			.truncate(false)
			.open(&lock_path)
			.unwrap();
		held.try_lock().expect("must acquire external file lock");
		let _timeout =
			EnvVarGuard::set("AGHUB_TEST_MUTATION_LOCK_TIMEOUT_MS", "100");

		let outputs = vec![healed_output("legacy", "global", "healed-hash")];
		let result = projection::write_auto_healed_hashes(
			&outputs,
			None,
			&identities,
			&Identities::new(),
		);

		let err = result.unwrap_err();
		assert_eq!(
			aghub_core::error_codes::wire_code(&err),
			aghub_core::skills::lock::MUTATION_LOCK_BUSY_CODE
		);
		assert!(aghub_core::error_codes::retryable(&err));
		assert_eq!(
			skill::lock::global::read_skill_lock().skills["legacy"]
				.content_hash,
			None,
			"a busy heal must write nothing"
		);
		// The caller still owns its results after the failed heal.
		assert_eq!(outputs.len(), 1);
		assert_eq!(outputs[0].heal_hash.as_deref(), Some("healed-hash"));
	});
}
