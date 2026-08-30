//! Shared removal-outcome wire DTO.
//!
//! [`RemovalView`] is the single source of truth for turning a
//! [`RemovalOutcome`] into the success/dry_run/executed/needs_confirm/paths/
//! skipped/deleted_path shape both the CLI `delete` command and the API
//! `DeleteSkillByPathResponse` serialize. The `PathBuf -> String`
//! stringification and `deleted_path` derivation live here and nowhere else.
//!
//! Serde defaults to snake_case, matching the field names the desktop and CLI
//! already consume. No ts-rs here — core carries no ts-rs dependency; the
//! ts-rs struct stays in `crates/api` as a thin wrapper over this view.

use crate::skills::removal::RemovalOutcome;
use serde::Serialize;

/// What a removal request actually resolved to. The one field that answers "did
/// the thing I asked for happen?" without cross-referencing three booleans.
///
/// `executed` + `dry_run` could not express `Absent`: an "already gone" outcome
/// and a refused preview both had `executed: false`, and `dry_run` was derived
/// from `!executed`, so they serialized IDENTICALLY — the same md5, byte for
/// byte, for `delete skills nope -y` and `delete skills nope`. The human
/// renderer told those two apart perfectly ("nothing to remove" vs "would
/// remove … re-run with --yes"); only the machine shape could not. Worse, the
/// comment on that renderer explains that telling a caller to "re-run with
/// --yes" when the thing is already gone is a loop that never terminates — and
/// `dry_run: true` on a `--yes` run is exactly that hint in machine form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalKind {
	/// Nothing was touched because the caller did not confirm. Re-running with
	/// `--yes` / `confirm` WILL change something.
	Preview,
	/// Deletion ran.
	Removed,
	/// Nothing to do: the resource was already gone. Re-running changes
	/// nothing, so a caller must not retry on this.
	Absent,
	/// Deletion ran and at least one path could NOT be deleted. `paths` holds
	/// what went, `skipped` holds what stayed. Do not read this as success:
	/// when `paths` is empty, nothing was removed at all.
	///
	/// Without this variant, such a run reported `removed` — `executed` is
	/// hard-coded `true` for the whole execute branch and the failures are
	/// folded into `skipped`, so the only honest signal was a caller manually
	/// comparing two lists.
	Partial,
	/// Nothing was or will be removed because the agent goes on reading the
	/// skill from the SHARED universal Master either way — whether the plan
	/// found nothing of its own to take (the Master IS the only copy) or found
	/// a path that changes nothing (an npx-era Referrer beside the Master it
	/// points at).
	///
	/// A single-agent removal cannot express "stop only this agent seeing it",
	/// so an EXECUTING call refuses outright (`manager/skill.rs`'s
	/// `unsupported_operation`). Reporting `preview` for the dry-run of that
	/// state was the same non-terminating hint `Absent` was added to kill:
	/// "re-run with --yes" pointed straight at a guaranteed error. And the API
	/// reported it as a plain `success: true`, which is why the desktop's
	/// delete dialog closed on a skill that is still installed.
	Kept,
}

/// Wire view of a [`RemovalOutcome`]: the post-execution plan flattened to
/// strings plus the derived `deleted_path`/`outcome` fields.
#[derive(Debug, Clone, Serialize)]
pub struct RemovalView {
	pub success: bool,
	/// Whether the CALLER asked for a preview — their intent, not an inference
	/// from the result. See [`RemovalView::from_outcome`].
	pub dry_run: bool,
	pub executed: bool,
	pub needs_confirm: bool,
	pub paths: Vec<String>,
	pub skipped: Vec<String>,
	pub deleted_path: Option<String>,
	/// The three-way answer. Prefer this over reading the booleans.
	pub outcome: RemovalKind,
}

impl RemovalView {
	/// Build the wire view.
	///
	/// `requested_dry_run` is the CALLER's intent — no `--yes`, or an explicit
	/// `--dry-run` (the API always passes `false`; it has no preview mode).
	/// There is deliberately no `From<&RemovalOutcome>`: `dry_run` used to be
	/// `!outcome.executed`, which reported `dry_run: true` to a caller who HAD
	/// passed `--yes` and whose target simply no longer existed. That caller's
	/// only reasonable reading is "my confirmation was ignored", so it retries —
	/// forever, because the world is already in the requested state and nothing
	/// else ever contradicts it.
	pub fn from_outcome(
		outcome: &RemovalOutcome,
		requested_dry_run: bool,
	) -> Self {
		let stringify = |paths: &[std::path::PathBuf]| -> Vec<String> {
			paths.iter().map(|p| p.display().to_string()).collect()
		};
		let kind = if outcome.plan.shared_master_kept
			&& (outcome.plan.paths.is_empty() || !outcome.executed)
		{
			// Nothing was or will be removed BECAUSE it is shared. Outranks
			// every other answer: an executing call refuses (the manager
			// guard), so `preview` would tell a caller to retry into a
			// guaranteed error, and an `executed` call that took nothing is not
			// a removal.
			//
			// `plan.shared_master_kept` has existed since the plan was written
			// and is read by `manager/skill.rs` and `transfer.rs` — it was just
			// never read HERE, so the fact never reached the wire at all.
			//
			// `|| !outcome.executed` covers the shape where the plan HAS paths
			// of its own and unlinking them changes NOTHING about what the
			// agent reads (an npx-era Referrer beside the Master it points at
			// — both resolve to the same place). The manager folds that fact
			// into `shared_master_kept`, then REFUSES the confirmed call — so a
			// preview reporting `preview` promised "re-run with --yes will
			// change something" and `--yes` answered `unsupported_operation`.
			// That is exactly the never-terminating hint this variant exists to
			// kill. An EXECUTED run keeps falling through to `Removed`/
			// `Partial`: it got past the refusal, so its paths really went.
			RemovalKind::Kept
		} else if outcome.executed && !outcome.failed_paths.is_empty() {
			// Ran, but not everything went. Checked BEFORE `Removed`: the
			// execute branch sets `executed: true` unconditionally, so a run
			// where every single delete failed is indistinguishable from a
			// clean one on that flag alone.
			RemovalKind::Partial
		} else if outcome.executed {
			RemovalKind::Removed
		} else if outcome.absent {
			// Already gone. This OUTRANKS the caller's intent: an unconfirmed
			// delete of a resource that does not exist is not a preview of
			// anything — re-running it with `--yes` changes nothing, and
			// reporting `preview` invites exactly that pointless retry.
			RemovalKind::Absent
		} else if requested_dry_run {
			RemovalKind::Preview
		} else {
			// Confirmed, nothing ran, and the plan did not say "absent" —
			// treated as absent because there is nothing else it can be.
			RemovalKind::Absent
		};
		Self {
			// NOT hard-coded true. `Partial` means the removal RAN and at least
			// one path could not be deleted — with every path failing, `paths` is
			// empty and the resource is entirely still there. This type's own doc
			// for the variant says "Do not read this as success", and then said
			// `success: true` two screens later; a `delete --yes` that deleted
			// nothing because of EACCES exited 0. Every other variant IS a
			// success: a preview succeeded at previewing, `absent` at finding
			// nothing to do, and `kept` at deliberately keeping a shared master.
			success: kind != RemovalKind::Partial,
			dry_run: requested_dry_run,
			executed: outcome.executed,
			needs_confirm: outcome.plan.needs_confirm,
			paths: stringify(&outcome.plan.paths),
			skipped: stringify(&outcome.plan.skipped),
			deleted_path: outcome
				.executed
				.then(|| {
					outcome.plan.paths.first().map(|p| p.display().to_string())
				})
				.flatten(),
			outcome: kind,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::skills::removal::{
		Layout, PruneStatus, RemovalOutcome, RemovalPlan,
	};
	use std::path::PathBuf;

	fn outcome(executed: bool, paths: Vec<PathBuf>) -> RemovalOutcome {
		RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths,
				skipped: vec![],
				needs_confirm: false,
				shared_master_kept: false,
				incomplete: false,
			},
			executed,
			prune: PruneStatus::NotRun,
			failed_paths: vec![],
			absent: false,
		}
	}

	#[test]
	fn removal_view_dry_run_sets_flags_and_no_deleted_path() {
		let view = RemovalView::from_outcome(
			&outcome(false, vec![PathBuf::from("/a/foo")]),
			true,
		);
		let json = serde_json::to_value(&view).unwrap();
		assert_eq!(json["dry_run"], serde_json::json!(true));
		assert_eq!(json["executed"], serde_json::json!(false));
		assert_eq!(json["deleted_path"], serde_json::Value::Null);
	}

	#[test]
	fn executed_outcome_sets_deleted_path_to_first() {
		let p = PathBuf::from("/a/foo");
		let view =
			RemovalView::from_outcome(&outcome(true, vec![p.clone()]), false);
		assert_eq!(view.deleted_path.as_deref(), Some("/a/foo"));
		let json = serde_json::to_value(&view).unwrap();
		assert_eq!(json["executed"], serde_json::json!(true));
		assert_eq!(json["dry_run"], serde_json::json!(false));
		assert_eq!(json["deleted_path"], serde_json::json!("/a/foo"));
	}

	#[test]
	fn executed_outcome_with_no_paths_has_null_deleted_path() {
		let view = RemovalView::from_outcome(&outcome(true, vec![]), false);
		assert!(view.deleted_path.is_none());
	}

	#[test]
	fn preview_and_absent_are_machine_distinguishable() {
		// The regression this pins: `dry_run` was `!executed`, so a refused
		// preview and an already-gone resource serialized IDENTICALLY — the
		// same bytes for `delete skills nope -y` and `delete skills nope`. A
		// caller that passed --yes read `dry_run: true` as "my confirmation was
		// ignored" and retried forever, because the world was already in the
		// requested state and nothing ever contradicted it.
		let nothing_ran = outcome(false, vec![]);

		let preview = RemovalView::from_outcome(&nothing_ran, true);
		let absent = RemovalView::from_outcome(&nothing_ran, false);

		assert_eq!(preview.outcome, RemovalKind::Preview);
		assert_eq!(absent.outcome, RemovalKind::Absent);
		assert!(
			preview.dry_run,
			"no --yes means the caller wanted a preview"
		);
		assert!(
			!absent.dry_run,
			"a confirmed request is NOT a dry-run just because there was \
			 nothing to do"
		);

		// The whole point: the two serialized documents must differ.
		let a = serde_json::to_string(&preview).unwrap();
		let b = serde_json::to_string(&absent).unwrap();
		assert_ne!(
			a, b,
			"a preview and an already-absent resource must not serialize \
			 identically"
		);

		// And an executed removal is a third, distinct answer.
		let removed = RemovalView::from_outcome(
			&outcome(true, vec![PathBuf::from("/a/foo")]),
			false,
		);
		assert_eq!(removed.outcome, RemovalKind::Removed);
		assert!(!removed.dry_run);
	}

	#[test]
	fn a_run_whose_deletes_all_failed_is_not_reported_as_removed() {
		// The execute branch sets `executed: true` unconditionally and folds
		// failed paths into `plan.skipped`, so a run where EVERY delete failed
		// looked identical to a clean one on that flag. Once a three-way
		// outcome existed it said `removed` — for files that are all still on
		// disk. `failed_paths` is what makes `partial` expressible.
		let all_failed = RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				// Nothing actually went.
				paths: vec![],
				skipped: vec![PathBuf::from("/a/locked")],
				needs_confirm: false,
				shared_master_kept: false,
				incomplete: false,
			},
			executed: true,
			prune: PruneStatus::NotRun,
			failed_paths: vec![PathBuf::from("/a/locked")],
			absent: false,
		};
		let view = RemovalView::from_outcome(&all_failed, false);
		assert_eq!(
			view.outcome,
			RemovalKind::Partial,
			"a removal that deleted nothing must not report `removed`"
		);

		// A clean executed run is still `removed`.
		let clean = RemovalOutcome {
			failed_paths: vec![],
			..outcome(true, vec![PathBuf::from("/a/foo")])
		};
		assert_eq!(
			RemovalView::from_outcome(&clean, false).outcome,
			RemovalKind::Removed
		);
	}

	#[test]
	fn a_kept_shared_master_is_not_a_removal_or_a_preview() {
		// `plan.shared_master_kept` existed all along and was read by the
		// manager and by transfer — but never by this mapper, so the fact never
		// reached the wire. The dry-run of that state reported `preview`, whose
		// contract promises `--yes` will change something; `--yes` actually
		// hits `unsupported_operation`, so the machine answer pointed straight
		// at a guaranteed error. The API's own branch reported a bare
		// `success: true`, and the desktop closed its delete dialog on a skill
		// that is still installed.
		let kept = RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				// Nothing to take: the only path is the shared Master.
				paths: vec![],
				skipped: vec![PathBuf::from("/a/.agents/skills/shared")],
				needs_confirm: false,
				shared_master_kept: true,
				incomplete: false,
			},
			executed: false,
			prune: PruneStatus::NotRun,
			failed_paths: vec![],
			absent: false,
		};

		// Kept regardless of what the caller asked for — the answer is about
		// the world, not the request.
		for requested_dry_run in [true, false] {
			assert_eq!(
				RemovalView::from_outcome(&kept, requested_dry_run).outcome,
				RemovalKind::Kept,
				"shared-master-kept outranks the caller's intent \
				 (dry_run={requested_dry_run})"
			);
		}

		// A plan that HAS a path of its own whose removal changes nothing about
		// what the agent reads (an npx-era Referrer beside the Master it points
		// at) previews as `kept` too — the manager folds that into
		// `shared_master_kept` and then REFUSES the confirmed call, so
		// `preview` would promise a `--yes` that only ever answers
		// `unsupported_operation`. A path whose removal DOES take something
		// away (a private copy shadowing the Master) never reaches here: the
		// manager does not set the flag for it.
		let refused_but_has_paths = RemovalOutcome {
			plan: RemovalPlan {
				paths: vec![PathBuf::from("/a/.opencode/skills/mover")],
				..kept.plan.clone()
			},
			..kept.clone()
		};
		for requested_dry_run in [true, false] {
			assert_eq!(
				RemovalView::from_outcome(
					&refused_but_has_paths,
					requested_dry_run
				)
				.outcome,
				RemovalKind::Kept,
				"a non-executed removal the commit will refuse must not be \
				 previewed as one that changes something \
				 (dry_run={requested_dry_run})"
			);
		}

		// Guard against over-triggering: a plan that kept the Master but ALSO
		// removed this agent's own path is a real removal.
		let removed_own_path = RemovalOutcome {
			plan: RemovalPlan {
				paths: vec![PathBuf::from("/a/.claude/skills/shared")],
				..kept.plan.clone()
			},
			executed: true,
			..kept.clone()
		};
		assert_eq!(
			RemovalView::from_outcome(&removed_own_path, false).outcome,
			RemovalKind::Removed,
			"keeping the shared Master while removing this agent's own link \
			 IS a removal"
		);
	}

	#[test]
	fn needs_confirm_and_skipped_paths_map_through() {
		// Guards the single mapper against dropping/hard-coding needs_confirm
		// or skipped: a not-yet-executed all-agents delete carries
		// needs_confirm=true and a kept (skipped) shared master.
		let outcome = RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths: vec![PathBuf::from("/a/foo")],
				skipped: vec![PathBuf::from("/a/master")],
				needs_confirm: true,
				shared_master_kept: false,
				incomplete: false,
			},
			executed: false,
			prune: PruneStatus::NotRun,
			failed_paths: vec![],
			absent: false,
		};
		let view = RemovalView::from_outcome(&outcome, true);
		assert!(view.needs_confirm, "needs_confirm must propagate");
		assert_eq!(view.skipped, vec!["/a/master".to_string()]);
		assert!(view.deleted_path.is_none(), "nothing executed");
		let json = serde_json::to_value(&view).unwrap();
		assert_eq!(json["needs_confirm"], serde_json::json!(true));
		assert_eq!(json["skipped"], serde_json::json!(["/a/master"]));
	}
}
