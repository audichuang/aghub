//! Security gate over freshly-FETCHED skill content.
//!
//! Two flows bring bytes we did not write onto disk, and both must pass here
//! before their first write: the install
//! ([`crate::skills::install_fetched::install_fetched_skill_and_lock`]) and the
//! update ([`crate::skills::resync::resync_installed_skill`]). Gating only the
//! install would leave the classic supply-chain shape open — publish a benign
//! skill, wait for installs, then push a malicious update, which `apply-update`
//! would stage and swap without a second look.
//!
//! Deliberately NOT gated: `add --from <local dir>` and `transfer`/`reconcile`.
//! The first is a directory the user pointed at themselves, the second copies
//! content already granted to another agent. Both would also make every
//! agent-to-agent copy re-audit bytes that passed on the way in.
//!
//! The refusal decision lives HERE and nowhere else. Surfaces (CLI, API) pass a
//! `force` bool and render the error; they do not re-derive the policy.

use skill_audit::{Action, AuditInput, AuditReport, ResourceFile, Verdict};
use std::path::Path;

/// SKILL.md as the audit input names it, and as it is spelled on disk.
const SKILL_MD: &str = "SKILL.md";

/// Read a fetched skill tree into an [`AuditInput`].
///
/// Reuses `skill::collect_skill_files`, so the traversal bounds and the
/// skip-symlinks rule are the folder-hash's, not a second opinion.
///
/// ponytail: the whole tree is read into memory (bounded by the hash
/// traversal's 256 MiB total), rather than streamed per rule. Streaming only
/// matters if real skills ever approach that ceiling.
fn read_input(name: &str, dir: &Path) -> std::io::Result<AuditInput> {
	let files = skill::collect_skill_files(dir).map_err(|e| match e {
		skill::HashError::Io(io) => io,
		other => std::io::Error::new(
			std::io::ErrorKind::InvalidData,
			other.to_string(),
		),
	})?;

	let mut skill_md = String::new();
	let mut resources = Vec::new();
	for (rel, abs) in files {
		let bytes = std::fs::read(&abs)?;
		// The audit wants SKILL.md RAW, frontmatter included — injection hides
		// in the frontmatter as readily as in the body. Anything that is not
		// valid UTF-8 is not the instruction file; keep it as a resource so the
		// binary-detection rules still see it.
		if rel.eq_ignore_ascii_case(SKILL_MD) {
			if let Ok(text) = String::from_utf8(bytes.clone()) {
				skill_md = text;
				continue;
			}
		}
		resources.push(ResourceFile {
			path: rel,
			content: bytes,
		});
	}

	Ok(AuditInput {
		name: name.to_string(),
		skill_md,
		resources,
	})
}

/// Audit a fetched source tree and refuse a malicious one.
///
/// - `Malicious` → `Err(ValidationFailed)` naming the findings, unless `force`.
/// - `Suspicious` → installs, with every finding logged at warn level (the CLI
///   routes `log` to stderr, so the user sees them; the API records them).
/// - `Benign` → silent.
///
/// An audit that cannot RUN (unreadable tree, rule-compilation failure) is
/// logged and treated as "not audited" rather than as a refusal: failing closed
/// here would make a corrupt rule set break every install, and the tree is
/// about to be read by the install itself, which reports its own IO errors with
/// far better context.
pub fn guard_fetched_source(
	name: &str,
	dir: &Path,
	force: bool,
) -> Result<Option<AuditReport>, crate::ConfigError> {
	let input = match read_input(name, dir) {
		Ok(input) => input,
		Err(error) => {
			log::warn!("skill '{name}' was not audited: {error}");
			return Ok(None);
		}
	};
	let report = match skill_audit::audit(&input) {
		Ok(report) => report,
		Err(error) => {
			log::warn!("skill '{name}' was not audited: {error}");
			return Ok(None);
		}
	};

	if report.verdict != Verdict::Benign {
		for finding in &report.findings {
			log::warn!(
				"skill '{name}': {} [{}] in {} — {}",
				finding.rule_id,
				severity_label(finding.severity),
				finding.file,
				finding.evidence,
			);
		}
	}

	if skill_audit::decide(&report) == Action::Block {
		if !force {
			return Err(crate::ConfigError::ValidationFailed(refusal_message(
				name, &report,
			)));
		}
		log::warn!(
			"skill '{name}' audited as malicious; installing anyway because \
			 the caller forced it"
		);
	}

	Ok(Some(report))
}

fn severity_label(severity: skill_audit::Severity) -> &'static str {
	match severity {
		skill_audit::Severity::Critical => "critical",
		skill_audit::Severity::High => "high",
		skill_audit::Severity::Medium => "medium",
		skill_audit::Severity::Low => "low",
		skill_audit::Severity::Info => "info",
	}
}

/// The refusal a blocked install reports.
///
/// Names the rules that fired and where, because "this skill is malicious" with
/// no evidence is unactionable — the user cannot tell a real finding from a
/// false positive, and a false positive is the only reason to override.
fn refusal_message(name: &str, report: &AuditReport) -> String {
	let mut detail = report
		.findings
		.iter()
		.filter(|f| {
			matches!(
				f.severity,
				skill_audit::Severity::Critical | skill_audit::Severity::High
			)
		})
		.map(|f| format!("{} in {}", f.rule_id, f.file))
		.collect::<Vec<_>>();
	detail.sort();
	detail.dedup();
	format!(
		"Skill '{name}' was refused by the security audit: {}. Findings: {}. \
		 Re-run with --force-unsafe if you have reviewed the source and \
		 believe this is a false positive.",
		report.summary,
		detail.join(", "),
	)
}
