//! `git_scan_skills` builds a `SkillRepository` straight from its async body,
//! before the `spawn_blocking` under it. reqwest's blocking client panics when
//! constructed on a Tokio worker ("Cannot drop a runtime in a context where
//! blocking is not allowed"), so the shared client's initializer has to hop to
//! its own OS thread.
//!
//! This lives in its OWN test binary on purpose: the shared client is a
//! process-wide `OnceLock`, so any sibling test that builds one off-runtime
//! first would leave nothing for this test to construct — and it would pass
//! even with the hop removed.

#[tokio::test]
async fn a_skill_repository_can_be_built_inside_an_async_handler() {
	let _repo = skill_update::SkillRepository::new();
}
