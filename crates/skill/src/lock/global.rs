use chrono::Utc;
use std::collections::{BTreeMap, BTreeSet};

use super::{io, types};

pub use io::{
	get_skill_lock_path, modify_skill_lock, modify_skill_lock_changed,
	read_skill_lock, write_skill_lock,
};
pub use types::{DismissedPrompts, SkillLockEntry, SkillLockFile};

/// Add or update a skill entry in the lock file.
///
/// Returns the entry this write REPLACED, or `None` when it created a new one —
/// the receipt a caller needs to roll its own write back: deleting an entry it
/// only replaced would destroy someone else's, and `None` is the only proof the
/// entry is genuinely ours.
pub fn add_skill_to_lock(
	skill_name: &str,
	mut entry: SkillLockEntry,
) -> std::io::Result<Option<SkillLockEntry>> {
	modify_skill_lock(|lock| {
		let now = Utc::now().to_rfc3339();

		if let Some(existing) = lock.skills.get(skill_name) {
			// Preserve the original installedAt timestamp
			entry.installed_at = existing.installed_at.clone();
			// `pluginName` belongs to the npx `skills` side of the lock — nothing
			// in aghub ever writes it, and rewriting an entry (re-install, relink,
			// coordinate heal) must not destroy interop metadata we only read. A
			// caller that genuinely knows the new owner passes it explicitly.
			if entry.plugin_name.is_none() {
				entry.plugin_name = existing.plugin_name.clone();
			}
		} else {
			entry.installed_at = now.clone();
		}
		entry.updated_at = now;

		lock.skills.insert(skill_name.to_string(), entry)
	})
}

/// Remove a skill from the lock file.
pub fn remove_skill_from_lock(skill_name: &str) -> std::io::Result<bool> {
	modify_skill_lock_changed(|lock| {
		let removed = lock.skills.remove(skill_name).is_some();
		(removed, removed)
	})
}

/// Atomically prune the global lock down to the skills present on disk.
///
/// `present_dir_names` is the set of skill *folder* names found on disk (already
/// in sanitized form). A lock entry is dropped when `sanitize_name(key)` is not
/// in that set; surviving entries are preserved byte-for-byte. Performs a single
/// read-modify-write (atomic temp+rename); the file is NOT rewritten when nothing
/// is pruned, so an unchanged lock keeps its exact bytes. Returns the pruned keys.
pub fn retain_locked_skills(
	present_dir_names: &BTreeSet<String>,
) -> std::io::Result<Vec<String>> {
	modify_skill_lock_changed(|lock| {
		let removed: Vec<String> = lock
			.skills
			.keys()
			.filter(|key| {
				!crate::sanitize::skill_present_on_disk(key, present_dir_names)
			})
			.cloned()
			.collect();
		for key in &removed {
			lock.skills.remove(key);
		}
		let changed = !removed.is_empty();
		(removed, changed)
	})
}

/// Get a skill entry from the lock file.
pub fn get_skill_from_lock(skill_name: &str) -> Option<SkillLockEntry> {
	let lock = read_skill_lock();
	lock.skills.get(skill_name).cloned()
}

/// Get all skills from the lock file.
pub fn get_all_locked_skills() -> BTreeMap<String, SkillLockEntry> {
	let lock = read_skill_lock();
	lock.skills
}

/// Get skills grouped by source for batch update operations.
pub fn get_skills_by_source() -> BTreeMap<String, Vec<String>> {
	let lock = read_skill_lock();
	let mut by_source: BTreeMap<String, Vec<String>> = BTreeMap::new();

	for (skill_name, entry) in lock.skills.iter() {
		by_source
			.entry(entry.source.clone())
			.or_default()
			.push(skill_name.clone());
	}

	by_source
}

/// Check if a prompt has been dismissed.
pub fn is_prompt_dismissed(prompt_key: &str) -> bool {
	let lock = read_skill_lock();
	lock.dismissed
		.as_ref()
		.and_then(|d| match prompt_key {
			"findSkillsPrompt" => d.find_skills_prompt,
			_ => None,
		})
		.unwrap_or(false)
}

/// Mark a prompt as dismissed.
pub fn dismiss_prompt(prompt_key: &str) -> std::io::Result<()> {
	modify_skill_lock(|lock| {
		if lock.dismissed.is_none() {
			lock.dismissed = Some(DismissedPrompts::default());
		}

		if let Some(ref mut dismissed) = lock.dismissed {
			if prompt_key == "findSkillsPrompt" {
				dismissed.find_skills_prompt = Some(true);
			}
		}
	})
}

/// Get the last selected agents.
pub fn get_last_selected_agents() -> Option<Vec<String>> {
	let lock = read_skill_lock();
	lock.last_selected_agents
}

/// Save the selected agents to the lock file.
pub fn save_selected_agents(agents: Vec<String>) -> std::io::Result<()> {
	modify_skill_lock(|lock| {
		lock.last_selected_agents = Some(agents);
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lock::test_utils::TestLockGuard;

	fn test_entry() -> SkillLockEntry {
		SkillLockEntry {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/owner/repo".to_string(),
			ref_name: None,
			skill_path: None,
			skill_folder_hash: "hash".to_string(),
			installed_at: "2024-01-01T00:00:00Z".to_string(),
			updated_at: "2024-01-01T00:00:00Z".to_string(),
			plugin_name: None,
			content_hash: None,
			ref_commit: None,
		}
	}

	#[test]
	fn test_add_skill_to_lock_new() {
		let _guard = TestLockGuard::new();
		let entry = test_entry();

		add_skill_to_lock("new-skill", entry).unwrap();

		let lock = read_skill_lock();
		assert!(lock.skills.contains_key("new-skill"));
		let stored = lock.skills.get("new-skill").unwrap();
		assert!(!stored.installed_at.is_empty());
		assert!(!stored.updated_at.is_empty());
	}

	/// The write's own receipt: `None` means it created the entry, `Some` hands
	/// back exactly what it replaced. A caller rolling its own write back can
	/// only tell "mine, delete it" from "someone else's, put it back" this way —
	/// an observation taken before the write cannot.
	#[test]
	fn add_skill_to_lock_reports_what_it_replaced() {
		let _guard = TestLockGuard::new();

		let mut first = test_entry();
		first.ref_name = Some("main".to_string());
		let replaced = add_skill_to_lock("s", first).unwrap();
		assert!(replaced.is_none(), "a new entry replaces nothing");

		let mut second = test_entry();
		second.ref_name = Some("v2".to_string());
		let replaced = add_skill_to_lock("s", second).unwrap();
		assert_eq!(
			replaced
				.expect("the rewrite replaced an entry")
				.ref_name
				.as_deref(),
			Some("main"),
			"the receipt must carry the entry as it was before the write"
		);
	}

	/// `pluginName` is written by the npx `skills` side and only ever READ here.
	/// Any aghub rewrite of the entry — re-install, relink, coordinate heal —
	/// passes `plugin_name: None`, so without preservation it silently destroys
	/// interop metadata and the skill stops looking plugin-managed.
	#[test]
	fn add_skill_to_lock_preserves_plugin_name_on_rewrite() {
		let _guard = TestLockGuard::new();

		let mut managed = test_entry();
		managed.plugin_name = Some("some-plugin".to_string());
		add_skill_to_lock("managed", managed).unwrap();

		// A later aghub write of the same entry carries no plugin name.
		let mut rewrite = test_entry();
		rewrite.ref_name = Some("v2".to_string());
		add_skill_to_lock("managed", rewrite).unwrap();

		let stored = read_skill_lock().skills.remove("managed").unwrap();
		assert_eq!(
			stored.plugin_name.as_deref(),
			Some("some-plugin"),
			"a rewrite must not drop the npx-owned pluginName"
		);
		assert_eq!(
			stored.ref_name.as_deref(),
			Some("v2"),
			"the rewrite's own fields still take effect"
		);
	}

	/// An explicit new owner still wins — preservation fills a gap, it does not
	/// pin the field forever.
	#[test]
	fn add_skill_to_lock_lets_an_explicit_plugin_name_win() {
		let _guard = TestLockGuard::new();

		let mut first = test_entry();
		first.plugin_name = Some("old-plugin".to_string());
		add_skill_to_lock("managed", first).unwrap();

		let mut second = test_entry();
		second.plugin_name = Some("new-plugin".to_string());
		add_skill_to_lock("managed", second).unwrap();

		assert_eq!(
			read_skill_lock()
				.skills
				.remove("managed")
				.unwrap()
				.plugin_name
				.as_deref(),
			Some("new-plugin"),
		);
	}

	#[test]
	fn test_add_skill_to_lock_preserves_installed_at() {
		let _guard = TestLockGuard::new();

		// Add initial skill
		let entry1 = test_entry();
		add_skill_to_lock("my-skill", entry1).unwrap();

		let lock1 = read_skill_lock();
		let original_installed_at =
			lock1.skills.get("my-skill").unwrap().installed_at.clone();

		// Update the same skill
		let mut entry2 = test_entry();
		entry2.skill_folder_hash = "hash2".to_string();
		add_skill_to_lock("my-skill", entry2).unwrap();

		let lock2 = read_skill_lock();
		let updated = lock2.skills.get("my-skill").unwrap();

		// installedAt should be preserved, updatedAt should change
		assert_eq!(updated.installed_at, original_installed_at);
		assert_ne!(updated.updated_at, original_installed_at);
		assert_eq!(updated.skill_folder_hash, "hash2");
	}

	#[test]
	fn test_remove_skill_from_lock() {
		let _guard = TestLockGuard::new();

		let entry = test_entry();
		add_skill_to_lock("my-skill", entry).unwrap();

		let removed = remove_skill_from_lock("my-skill").unwrap();
		assert!(removed);

		let lock = read_skill_lock();
		assert!(!lock.skills.contains_key("my-skill"));
	}

	#[test]
	fn test_get_skill_from_lock() {
		let _guard = TestLockGuard::new();

		let entry = test_entry();
		add_skill_to_lock("my-skill", entry.clone()).unwrap();

		let retrieved = get_skill_from_lock("my-skill");
		assert!(retrieved.is_some());
		assert_eq!(retrieved.unwrap().source, "owner/repo");

		let not_found = get_skill_from_lock("nonexistent");
		assert!(not_found.is_none());
	}

	#[test]
	fn test_get_all_locked_skills() {
		let _guard = TestLockGuard::new();

		let entry = test_entry();

		add_skill_to_lock("skill-a", entry.clone()).unwrap();
		add_skill_to_lock("skill-b", entry).unwrap();

		let all = get_all_locked_skills();
		assert_eq!(all.len(), 2);
	}

	#[test]
	fn test_get_skills_by_source() {
		let _guard = TestLockGuard::new();

		let mut entry1 = test_entry();
		entry1.source = "owner/repo".to_string();

		let mut entry2 = test_entry();
		entry2.source = "other/repo".to_string();

		add_skill_to_lock("skill-a", entry1.clone()).unwrap();
		add_skill_to_lock("skill-b", entry1).unwrap();
		add_skill_to_lock("skill-c", entry2).unwrap();

		let by_source = get_skills_by_source();
		assert_eq!(by_source.len(), 2);
		assert_eq!(by_source.get("owner/repo").unwrap().len(), 2);
		assert_eq!(by_source.get("other/repo").unwrap().len(), 1);
	}

	#[test]
	fn test_dismiss_prompt() {
		let _guard = TestLockGuard::new();

		assert!(!is_prompt_dismissed("findSkillsPrompt"));

		dismiss_prompt("findSkillsPrompt").unwrap();
		assert!(is_prompt_dismissed("findSkillsPrompt"));
	}

	#[test]
	fn test_save_and_get_last_selected_agents() {
		let _guard = TestLockGuard::new();

		assert!(get_last_selected_agents().is_none());

		save_selected_agents(vec!["claude".to_string(), "cursor".to_string()])
			.unwrap();

		let agents = get_last_selected_agents();
		assert!(agents.is_some());
		let agents = agents.unwrap();
		assert_eq!(agents.len(), 2);
		assert!(agents.contains(&"claude".to_string()));
		assert!(agents.contains(&"cursor".to_string()));
	}

	fn present(names: &[&str]) -> BTreeSet<String> {
		names.iter().map(|s| s.to_string()).collect()
	}

	#[test]
	fn retain_locked_skills_drops_absent_keeps_present() {
		let _guard = TestLockGuard::new();
		add_skill_to_lock("keep", test_entry()).unwrap();
		add_skill_to_lock("gone", test_entry()).unwrap();

		let removed = retain_locked_skills(&present(&["keep"])).unwrap();

		assert_eq!(removed, vec!["gone".to_string()]);
		let lock = read_skill_lock();
		assert!(lock.skills.contains_key("keep"));
		assert!(!lock.skills.contains_key("gone"));
	}

	#[test]
	fn retain_locked_skills_noop_when_all_present_does_not_rewrite() {
		let _guard = TestLockGuard::new();
		add_skill_to_lock("a", test_entry()).unwrap();
		let path = get_skill_lock_path();
		let before = std::fs::read(&path).unwrap();

		let removed = retain_locked_skills(&present(&["a"])).unwrap();

		assert!(removed.is_empty());
		let after = std::fs::read(&path).unwrap();
		assert_eq!(before, after, "unchanged lock must keep exact bytes");
	}

	#[test]
	fn retain_locked_skills_preserves_surviving_entry_fields_byte_identical() {
		let _guard = TestLockGuard::new();
		let mut keep = test_entry();
		keep.skill_folder_hash = "deadbeefdeadbeef".to_string();
		keep.content_hash = None; // npx-shaped: no contentHash
		add_skill_to_lock("keep", keep).unwrap();
		add_skill_to_lock("gone", test_entry()).unwrap();

		retain_locked_skills(&present(&["keep"])).unwrap();

		let raw = std::fs::read_to_string(get_skill_lock_path()).unwrap();
		assert!(!raw.contains("contentHash"), "must not inject contentHash");
		let lock = read_skill_lock();
		let k = lock.skills.get("keep").unwrap();
		assert_eq!(k.skill_folder_hash, "deadbeefdeadbeef");
		assert_eq!(k.content_hash, None);
		assert_eq!(lock.version, 3, "version must stay 3");
	}

	#[test]
	fn retain_locked_skills_matches_by_sanitized_key() {
		let _guard = TestLockGuard::new();
		// lock key "My Skill" sanitizes to "my-skill"; the on-disk folder name
		// is the sanitized form, so it must be kept.
		add_skill_to_lock("My Skill", test_entry()).unwrap();
		let removed = retain_locked_skills(&present(&["my-skill"])).unwrap();
		assert!(removed.is_empty());
		assert!(read_skill_lock().skills.contains_key("My Skill"));
	}

	#[test]
	fn retain_locked_skills_matches_legacy_sanitized_key() {
		let _guard = TestLockGuard::new();
		add_skill_to_lock("İstanbul", test_entry()).unwrap();
		let removed = retain_locked_skills(&present(&["stanbul"])).unwrap();
		assert!(removed.is_empty());
		assert!(read_skill_lock().skills.contains_key("İstanbul"));
	}

	#[test]
	fn concurrent_rmw_no_lost_update() {
		let _guard = TestLockGuard::new();
		let threads = 16;
		let barrier = std::sync::Arc::new(std::sync::Barrier::new(threads));
		std::thread::scope(|scope| {
			for i in 0..threads {
				let barrier = barrier.clone();
				scope.spawn(move || {
					barrier.wait();
					add_skill_to_lock(&format!("skill-{i}"), test_entry())
						.unwrap();
				});
			}
		});

		let lock = read_skill_lock();
		for i in 0..threads {
			assert!(lock.skills.contains_key(&format!("skill-{i}")));
		}
	}
}
