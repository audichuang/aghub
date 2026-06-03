//! Read-only `check skills` subcommand.
//!
//! Reports each locked skill's update status as a `SkillUpdateResponse`-shaped
//! JSON array (camelCase, `status`-tagged). This command is intentionally
//! offline: network fetching and credential resolution live in `crates/api`,
//! NOT here. Any skill whose freshness cannot be determined without a remote
//! fetch is reported `Uncheckable` (reason `network` for remote sources,
//! `local` for local-only sources). The existing `update skills` command keeps
//! editing local metadata only; `check` never mutates anything.

use crate::{eprintln_verbose, ResourceType};
use aghub_core::models::ResourceScope;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

/// Flattened, camelCase status mirroring `aghub-api`'s `SkillUpdateStatusResponse`
/// (the CLI does not depend on the api crate, so the shape is duplicated here).
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
// `UpToDate`/`UpdateAvailable` are part of the response contract (mirroring the
// api DTO) but unreachable on the offline CLI path, which can only ever emit
// `Uncheckable`. Kept so the emitted shape stays a superset-faithful mirror.
#[allow(dead_code)]
enum StatusView {
	UpToDate,
	UpdateAvailable {
		current: String,
		available: String,
	},
	Renamed {
		#[serde(rename = "newName")]
		new_name: String,
	},
	Uncheckable {
		reason: String,
	},
}

/// One skill's name plus its flattened update status. Mirrors
/// `aghub-api`'s `SkillUpdateResponse`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillUpdateView {
	name: String,
	scope: String,
	#[serde(flatten)]
	status: StatusView,
}

/// Map a lock entry's `source_type` to the offline `Uncheckable` reason.
/// Local sources can never be checked from a committed lock without a remote;
/// everything else needs a network fetch that lives in `crates/api`.
fn offline_reason(source_type: &str) -> &'static str {
	if source_type.eq_ignore_ascii_case("local") {
		"local"
	} else {
		"network"
	}
}

pub fn execute(
	resource: ResourceType,
	scope: ResourceScope,
	project_root: Option<&Path>,
	_json: bool,
) -> Result<()> {
	match resource {
		ResourceType::Mcps => {
			anyhow::bail!("`check` only supports skills");
		}
		ResourceType::Skills => {}
	}

	let mut views: Vec<SkillUpdateView> = Vec::new();

	let want_global =
		matches!(scope, ResourceScope::GlobalOnly | ResourceScope::Both);
	let want_project =
		matches!(scope, ResourceScope::ProjectOnly | ResourceScope::Both);

	if want_global {
		let locked = skill::get_all_locked_skills();
		eprintln_verbose!("Checking {} global locked skill(s)", locked.len());
		for (name, entry) in locked {
			views.push(SkillUpdateView {
				name,
				scope: "global".to_string(),
				status: StatusView::Uncheckable {
					reason: offline_reason(&entry.source_type).to_string(),
				},
			});
		}
	}

	if want_project {
		let lock = skill::read_local_lock(project_root);
		eprintln_verbose!(
			"Checking {} project locked skill(s)",
			lock.skills.len()
		);
		for (name, entry) in lock.skills {
			views.push(SkillUpdateView {
				name,
				scope: "project".to_string(),
				status: StatusView::Uncheckable {
					reason: offline_reason(&entry.source_type).to_string(),
				},
			});
		}
	}

	println!("{}", serde_json::to_string_pretty(&views)?);
	Ok(())
}
