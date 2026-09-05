#![cfg(unix)]
use aghub_agents::ResourceScope;
use aghub_core::skills::shape::*;
use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::PathBuf;

fn root() -> (tempfile::TempDir, PathBuf) {
	let tmp = tempfile::tempdir().unwrap();
	let r = fs::canonicalize(tmp.path()).unwrap();
	(tmp, r)
}

fn mkskill(p: &PathBuf, body: &str) {
	fs::create_dir_all(p).unwrap();
	fs::write(p.join("SKILL.md"), body).unwrap();
}

#[test]
fn probe_00_candidate_list() {
	let (_t, r) = root();
	for c in candidate_referrers(ResourceScope::ProjectOnly, Some(&r), "foo") {
		println!(
			"{:<14} {}",
			c.agent_id,
			c.path.strip_prefix(&r).unwrap().display()
		);
	}
}

#[test]
fn probe_01_two_legacy_dirs_one_wins_other_relinked() {
	let (_t, r) = root();
	let a = r.join(".claude/skills/foo");
	let b = r.join(".agents/skills/foo");
	mkskill(&a, "PRIVATE claude copy");
	mkskill(&b, "THE REAL OLD MASTER");
	let plan =
		plan_repair(ResourceScope::ProjectOnly, Some(&r), "foo").unwrap();
	println!(
		"needs_migration={} migrate_from={:?}",
		plan.needs_migration, plan.migrate_from
	);
	for (id, p, act) in &plan.actions {
		if p == &a || p == &b {
			println!(
				"  {id:<12} {} -> {act:?}",
				p.strip_prefix(&r).unwrap().display()
			);
		}
	}
	assert_eq!(
		plan.migrate_from.as_ref(),
		Some(&a),
		"registry order picks claude, not the shared slot"
	);
	let b_actions: Vec<_> = plan
		.actions
		.iter()
		.filter(|(_, p, _)| p == &b)
		.map(|(_, _, x)| x.clone())
		.collect();
	assert!(!b_actions.is_empty());
	assert!(
		b_actions.iter().all(|x| *x == ReferrerAction::Relink),
		"the OTHER real dir holding unique bytes is planned for Relink: \
		 {b_actions:?}"
	);
	let a_actions: Vec<_> = plan
		.actions
		.iter()
		.filter(|(_, p, _)| p == &a)
		.map(|(_, _, x)| x.clone())
		.collect();
	println!("adopted-source actions = {a_actions:?}");
	assert!(a_actions.iter().all(|x| *x == ReferrerAction::Relink));
}

#[test]
fn probe_02_same_bytes_become_compare_once_a_master_exists() {
	let (_t, r) = root();
	mkskill(&r.join(".claude/skills/foo"), "PRIVATE claude copy");
	mkskill(&r.join(".agents/skills/foo"), "THE REAL OLD MASTER");
	mkskill(&r.join(".aghub/foo"), "master");
	let plan =
		plan_repair(ResourceScope::ProjectOnly, Some(&r), "foo").unwrap();
	let acts: Vec<_> = plan
		.actions
		.iter()
		.filter(|(_, p, _)| {
			*p == r.join(".claude/skills/foo")
				|| *p == r.join(".agents/skills/foo")
		})
		.map(|(_, _, a)| a.clone())
		.collect();
	assert!(
		acts.iter()
			.all(|a| *a == ReferrerAction::CompareThenQuarantine),
		"{acts:?}"
	);
	println!("with master present the SAME dirs are CompareThenQuarantine");
}

#[test]
fn probe_03_regular_file_is_legacy_and_gets_adopted() {
	let (_t, r) = root();
	fs::create_dir_all(r.join(".claude/skills")).unwrap();
	fs::write(r.join(".claude/skills/foo"), "not a directory").unwrap();
	let m = r.join(".aghub/foo");
	let shape = classify_shape(&r.join(".claude/skills/foo"), &m);
	println!("regular file, no master -> {shape:?}");
	assert_eq!(shape, SkillShape::Legacy);
	let plan =
		plan_repair(ResourceScope::ProjectOnly, Some(&r), "foo").unwrap();
	assert!(plan.needs_migration);
	assert_eq!(plan.migrate_from, Some(r.join(".claude/skills/foo")));
}

#[test]
fn probe_04_regular_file_master_is_conformant() {
	let (_t, r) = root();
	fs::create_dir_all(r.join(".aghub")).unwrap();
	fs::write(r.join(".aghub/foo"), "a FILE where the Master belongs").unwrap();
	fs::create_dir_all(r.join(".claude/skills")).unwrap();
	unix_fs::symlink(r.join(".aghub/foo"), r.join(".claude/skills/foo"))
		.unwrap();
	let shape =
		classify_shape(&r.join(".claude/skills/foo"), &r.join(".aghub/foo"));
	println!("symlink -> file master = {shape:?}");
	assert_eq!(shape, SkillShape::Conformant);
	let plan =
		plan_repair(ResourceScope::ProjectOnly, Some(&r), "foo").unwrap();
	println!("is_noop = {}", plan.is_noop());
	assert!(
		plan.is_noop(),
		"repair reports a healthy store around a file"
	);
}

#[test]
fn probe_05_relative_vs_absolute_symlink_agree() {
	let (_t, r) = root();
	mkskill(&r.join(".aghub/foo"), "m");
	fs::create_dir_all(r.join(".claude/skills")).unwrap();
	fs::create_dir_all(r.join(".cursor/skills")).unwrap();
	unix_fs::symlink(r.join(".aghub/foo"), r.join(".claude/skills/foo"))
		.unwrap();
	unix_fs::symlink("../../.aghub/foo", r.join(".cursor/skills/foo")).unwrap();
	let m = r.join(".aghub/foo");
	assert_eq!(
		classify_shape(&r.join(".claude/skills/foo"), &m),
		SkillShape::Conformant
	);
	assert_eq!(
		classify_shape(&r.join(".cursor/skills/foo"), &m),
		SkillShape::Conformant
	);
}

#[test]
fn probe_06_three_hop_chain() {
	let (_t, r) = root();
	let m = r.join(".aghub/foo");
	mkskill(&m, "m");
	let b = r.join("B");
	let a = r.join("A");
	unix_fs::symlink(&m, &b).unwrap();
	unix_fs::symlink(&b, &a).unwrap();
	fs::create_dir_all(r.join(".claude/skills")).unwrap();
	let rf = r.join(".claude/skills/foo");
	unix_fs::symlink(&a, &rf).unwrap();
	let s = classify_shape(&rf, &m);
	println!("R->A->B->M = {s:?}");
	assert!(matches!(
		s,
		SkillShape::Violation(ViolationKind::Chain { .. })
	));
}

#[test]
fn probe_07_sanitize_traversal() {
	let (_t, r) = root();
	let long = "x".repeat(300);
	for n in [
		"..",
		"../../etc/passwd",
		"/etc/passwd",
		"",
		"Foo",
		"foo bar",
		"foo/bar",
		long.as_str(),
	] {
		let p = master_path(Some(&r), n).unwrap();
		let disp = p.display().to_string();
		println!(
			"{:?} -> {}",
			if n.len() > 20 { "<300 x>" } else { n },
			&disp[r.display().to_string().len()..]
		);
		assert!(p.starts_with(&r), "escaped the root: {disp}");
		assert_eq!(
			p.components().count(),
			r.components().count() + 2,
			"extra path components for {n:?}"
		);
	}
	assert_eq!(master_path(Some(&r), "Foo"), master_path(Some(&r), "foo"));
	assert_eq!(
		master_path(Some(&r), "foo bar"),
		master_path(Some(&r), "foo/bar")
	);
}

#[test]
fn probe_08_global_scope_with_project_root_pairs_wrong_master() {
	let (_t, r) = root();
	let plan = plan_repair(ResourceScope::GlobalOnly, Some(&r), "foo").unwrap();
	println!("master  = {}", plan.master.display());
	println!("action0 = {}", plan.actions[0].1.display());
	assert!(plan.master.starts_with(&r));
	assert!(
		!plan.actions[0].1.starts_with(&r),
		"candidates are global, master is project-local"
	);
}

#[test]
fn probe_09_scope_both_is_a_vacuous_noop() {
	let (_t, r) = root();
	mkskill(&r.join(".agents/skills/foo"), "un-migrated, needs repair");
	let plan = plan_repair(ResourceScope::Both, Some(&r), "foo").unwrap();
	println!(
		"Both: {} actions, is_noop={}",
		plan.actions.len(),
		plan.is_noop()
	);
	assert!(plan.actions.is_empty());
	assert!(
		plan.is_noop(),
		"an un-migrated skill reports nothing to do under Both"
	);
}

#[test]
fn probe_10_shared_slot_duplicated_in_actions_and_refusals() {
	let (_t, r) = root();
	let store = r.join(".aghub");
	mkskill(&store.join("foo"), "m");
	fs::create_dir_all(r.join(".agents")).unwrap();
	unix_fs::symlink(&store, r.join(".agents/skills")).unwrap();
	let plan =
		plan_repair(ResourceScope::ProjectOnly, Some(&r), "foo").unwrap();
	let slot = r.join(".agents/skills/foo");
	let n = plan.actions.iter().filter(|(_, p, _)| p == &slot).count();
	println!(
		"shared slot appears {n} times; refusals={}",
		plan.refusals().len()
	);
	assert!(n > 1);
	assert!(
		!plan.refusals().is_empty(),
		"the aliased shared slot must refuse"
	);
	assert!(
		!plan.is_noop(),
		"is_noop with refusals present must be false"
	);
}

#[test]
fn probe_11_both_sides_under_a_symlinked_parent() {
	let (_t, r) = root();
	let (_t2, outer) = root();
	let link = outer.join("link");
	unix_fs::symlink(&r, &link).unwrap();
	mkskill(&r.join(".aghub/foo"), "m");
	fs::create_dir_all(r.join(".claude/skills")).unwrap();
	fs::create_dir_all(r.join(".cursor/skills")).unwrap();
	unix_fs::symlink(link.join(".aghub/foo"), r.join(".claude/skills/foo"))
		.unwrap();
	unix_fs::symlink("../../.aghub/foo", r.join(".cursor/skills/foo")).unwrap();
	let m = link.join(".aghub/foo");
	let abs = classify_shape(&link.join(".claude/skills/foo"), &m);
	let rel = classify_shape(&link.join(".cursor/skills/foo"), &m);
	println!("both under symlinked parent: abs={abs:?} rel={rel:?}");
	assert_eq!(abs, SkillShape::Conformant);
	assert_eq!(rel, SkillShape::Conformant);
}
