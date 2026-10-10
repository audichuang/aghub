//! `aghub-cli doctor` — read-only skill health across scopes.
//!
//! Rows come from `aghub_core::skills::health::report` (one scope's lock
//! reconciled against its Master store, plus the optional referrer audit).
//! This module only selects scopes and roster, renders the table and JSON, and
//! gates `--fail-on-issues`. Never writes.

use aghub_core::{
	models::{AgentSelection, AgentType},
	skills::health::{report, DoctorRow, LinkAudit, Remedy},
};
use anyhow::{anyhow, Result};
use tabled::builder::Builder;
use tabled::settings::Style;

fn resolve_roster(agent: &str) -> Result<Vec<AgentType>> {
	match AgentSelection::parse(agent).map_err(|error| {
		anyhow!("invalid --agent for doctor link audit: {error}")
	})? {
		AgentSelection::All => Ok(AgentType::ALL.to_vec()),
		AgentSelection::List(agents) => Ok(agents),
	}
}

/// Dispatch `doctor`, optionally auditing the selected roster's referrers.
/// Scope resolution is shared with `source` (`-g` global only, `-p` project
/// only, default = global plus the current project when a root is detected).
pub fn execute_with_options(
	scope: &crate::Scope,
	json: bool,
	verify_links: bool,
	agent: &str,
	fail_on_issues: bool,
) -> Result<()> {
	let scopes = crate::commands::source::read_scopes(scope);
	let roster = verify_links.then(|| resolve_roster(agent)).transpose()?;

	let mut rows: Vec<DoctorRow> = Vec::new();
	for scope in &scopes {
		rows.extend(report(scope, roster.as_deref())?);
	}

	// Counted before the JSON early-return, so `--fail-on-issues` means the
	// same in both output modes, and over BOTH axes (`DoctorRow::is_issue`).
	let issue_count = rows.iter().filter(|row| row.is_issue()).count();
	// Name the axis that actually failed, not always the link audit (which
	// may not have run).
	let axes: std::collections::BTreeSet<&'static str> =
		rows.iter().filter_map(DoctorRow::issue_axis).collect();
	let gate = |issues: usize| -> Result<()> {
		if fail_on_issues && issues > 0 {
			let what = if axes.contains("both")
				|| (axes.contains("health") && axes.contains("links"))
			{
				"skill health and agent referrer issues"
			} else if axes.contains("links") {
				"agent referrer issues"
			} else {
				"skill health issues"
			};
			// Name the UNIT: this counts skills (rows); the notes count
			// per-agent records, so the two numbers legitimately differ.
			anyhow::bail!(
				"{issues} skill(s) with {what} — see the report above"
			);
		}
		Ok(())
	};

	if json {
		println!("{}", serde_json::to_string_pretty(&rows)?);
		// The report above IS the answer; without this the failure renderer
		// would append a second JSON document and every parse of stdout fails.
		crate::note_answer_on_stdout();
		return gate(issue_count);
	}

	if rows.is_empty() {
		println!("No installed skills.");
		return Ok(());
	}

	let mut builder = Builder::default();
	builder
		.push_record(["SCOPE", "SKILL", "SOURCE", "MASTER", "HEALTH", "LINKS"]);
	for r in &rows {
		builder.push_record([
			r.scope.to_string(),
			r.skill.clone(),
			r.source.clone(),
			r.master.label().to_string(),
			r.health.to_string(),
			r.link_audit.label(),
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");

	// A one-line hint only when something is off, so the healthy case stays quiet.
	let orphans = rows.iter().filter(|r| r.health == "orphan-lock").count();
	let untracked = rows.iter().filter(|r| r.health == "untracked").count();
	if orphans > 0 {
		eprintln!(
			"note: {orphans} orphan lock ent(y/ies) — run `aghub-cli prune-lock` \
			 to clear"
		);
	}
	if untracked > 0 {
		eprintln!(
			"note: {untracked} untracked skill(s) on disk with no lock — compare or \
			 back up local content, then delete before reinstalling via source sync; \
			 sync never overwrites an existing Master"
		);
	}
	// An unusable master is ONE fault on ONE row; the ReplaceMaster note is
	// its only remedy. Read on both axes: without `--verify-links` only
	// `health` names it, and it still fails `--fail-on-issues`.
	let master_unusable = |row: &DoctorRow| {
		row.health == "master-is-symlink"
			|| matches!(&row.link_audit, LinkAudit::Issues { agents }
			if agents.iter().any(|audit| {
				audit.state.remedy() == Some(Remedy::ReplaceMaster)
			}))
	};
	// Such a row's per-agent states are SYMPTOMS (as in `classify_shape`,
	// which collapses the master first), so the row is excluded from every
	// other bucket — filtered per ROW, since `audits()` flattens rows away.
	// Carries the row's scope for `scope_flag_for`.
	let audits = || {
		rows.iter()
			.filter(|row| !master_unusable(row))
			.filter_map(|row| match &row.link_audit {
				LinkAudit::NotRequested => None,
				LinkAudit::Verified { agents }
				| LinkAudit::Issues { agents } => Some((row.scope, agents)),
			})
			.flat_map(|(scope, agents)| {
				agents.iter().map(move |audit| (scope, audit))
			})
	};

	/// The scope flag for the rows in one remedy bucket.
	///
	/// Derived from the ROWS, never from the command's scope (which defaults
	/// to both): a wrong flag fixes nothing, or the same-named skill in the
	/// other scope. `<-g|-p>` when a bucket spans both — read the SCOPE column.
	fn flag_of(global: bool, project: bool) -> &'static str {
		match (global, project) {
			(false, true) => "-p",
			(true, true) => "<-g|-p>",
			_ => "-g",
		}
	}
	let scope_flag_for = |remedy: Remedy| {
		let (mut global, mut project) = (false, false);
		for (scope, audit) in audits() {
			if audit.state.remedy() == Some(remedy) {
				if scope == "project" {
					project = true;
				} else {
					global = true;
				}
			}
		}
		flag_of(global, project)
	};
	// One note per `Remedy`, so every state that IS an issue gets advice that
	// fits it. Anything else lets a new state inherit a command that cannot fix
	// it — see `AgentLinkState::remedy`.
	let in_bucket = |remedy: Remedy| {
		audits()
			.filter(move |(_, audit)| audit.state.remedy() == Some(remedy))
			.count()
	};

	// Orphan masters get their OWN note: reinstall advice would put back what
	// `delete --yes` just (partly) removed.
	let orphan_masters = in_bucket(Remedy::LeftoverMaster);
	if orphan_masters > 0 {
		eprintln!(
			"note: {orphan_masters} orphan master(s) — a master with no lock \
			 entry and no slot for this agent. There is no source to relink \
			 from; remove the master directory if nothing reads it. Check the \
			 other agents' rows first: an untracked master can still have a \
			 live `linked` referrer, and deleting it would dangle that link."
		);
	}

	let broken_links = in_bucket(Remedy::Reinstall);
	if broken_links > 0 {
		let scope_flag = scope_flag_for(Remedy::Reinstall);
		// Must stay a runnable command: `<source>` positional and `--yes`.
		eprintln!(
			"note: {broken_links} agent referrer issue(s) — repair a missing or \
			 dangling link with:\n  aghub-cli {scope_flag} -a <agent> source \
			 sync <source> --skill <name> --install-missing --yes\n\
			 (<source> is the SOURCE column of `aghub-cli source list`.) \
			 Foreign links and real-path conflicts are not repaired by sync — \
			 inspect those."
		);
	}

	// `chain` gets its OWN note, which must not say `source sync` (linking
	// sees the Master endpoint and writes nothing); `repair` plans a `Relink`.
	let chained = in_bucket(Remedy::Relink);
	if chained > 0 {
		let scope_flag = scope_flag_for(Remedy::Relink);
		eprintln!(
			"note: {chained} agent referrer(s) reaching the master through \
			 ANOTHER link (`chain`) — re-point them with:\n  aghub-cli \
			 {scope_flag} repair <name> --yes\n\
			 (`source sync --install-missing` cannot: the chain already \
			 resolves to the master, so linking reports it as already linked \
			 and writes nothing.)"
		);
	}

	// Counted per SKILL. Claims only what is pinned: `plan_repair` returns
	// `Refuse { MasterIsLink }`, rendered with this same advice. `source sync`
	// is NOT named (its behaviour on a linked master is unpinned), nor what
	// `--verify-links` prints for it (it varies by slot and lock).
	// See docs/history/cli.md#doctor-fail-on-issues-claims
	let unusable_masters =
		rows.iter().filter(|row| master_unusable(row)).count();
	if unusable_masters > 0 {
		eprintln!(
			"note: {unusable_masters} skill(s) whose master is not a real \
			 directory — the store holds a link (or a file) where the skill's \
			 own bytes must live. `aghub-cli repair` refuses it, naming this \
			 layout: replace the entry under the `.aghub` store with a real \
			 directory yourself, then re-run."
		);
	}
	gate(issue_count)
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::registry;

	/// `doctor -a all` expands through `AgentType::ALL` while every other
	/// `-a all` surface (source sync, the API, the desktop list) expands
	/// through `registry::ALL_AGENTS`. Those were two hand-written rosters in
	/// DIFFERENT orders, so `doctor` printed cursor-first while `source sync`
	/// printed claude-first for the same `-a all`. `agent_roster!` now emits
	/// both from one declaration; this pins that they stay one order, since
	/// nothing else would notice a second roster growing back.
	#[test]
	fn all_expands_in_the_same_order_as_the_registry() {
		let doctor: Vec<&str> = resolve_roster("all")
			.expect("'all' is a valid selection")
			.iter()
			.map(|agent| agent.as_str())
			.collect();
		let registry: Vec<&str> = registry::iter_all().map(|d| d.id).collect();
		assert_eq!(
			doctor, registry,
			"doctor's -a all roster must match registry order exactly"
		);
	}
}
