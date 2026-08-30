pub mod global;
mod guard;
mod io;
pub mod local;
mod types;

#[cfg(test)]
pub(crate) mod test_utils;

// Re-export public API
pub use global::{
	add_skill_to_lock, dismiss_prompt, get_all_locked_skills,
	get_last_selected_agents, get_skill_from_lock, get_skills_by_source,
	is_prompt_dismissed, remove_skill_from_lock, retain_locked_skills,
	save_selected_agents,
};
pub use guard::{
	mutation_guard, mutation_guard_with_timeout, MutationGuard, MutationScope,
};
pub use io::{
	atomic_write_json, ensure_locks_writable, get_skill_lock_path,
	read_global_lock_checked, read_skill_lock, write_skill_lock,
};
pub use types::{DismissedPrompts, SkillLockEntry, SkillLockFile};
