use aghub_core::{availability, registry};
use rocket::serde::json::Json;
use std::path::Path;

use crate::dto::agents::{
	AgentAvailabilityDto, AgentInfo, CapabilitiesDto, DisabledAgentsDto,
	McpCapabilitiesDto, ScopeSupportDto, SetDisabledAgentsRequest,
	SkillCapabilitiesDto, SkillsPathsDto, SubAgentCapabilitiesDto,
};
use crate::error::{ApiError, ApiResult};
use crate::extractors::TrustedLocalOrigin;

fn format_path(path: std::path::PathBuf) -> String {
	let s = path.to_string_lossy();
	let Some(home) = dirs::home_dir().map(|h| h.to_string_lossy().into_owned())
	else {
		return s.into_owned();
	};
	if s.starts_with(&home) {
		format!("~{}", &s[home.len()..])
	} else {
		s.into_owned()
	}
}

#[get("/agents")]
pub fn list_agents(_origin: TrustedLocalOrigin) -> Json<Vec<AgentInfo>> {
	let agents = registry::iter_all()
		.map(|d| {
			let project_root = Path::new("");
			// The derived list re-appends the universal `.agents/skills` that
			// a universal agent's own list already ends with; dedup keeps this
			// DTO field unchanged.
			let mut project_read_paths =
				d.project_skill_read_paths(project_root);
			project_read_paths.dedup();
			let project_read =
				project_read_paths.into_iter().map(format_path).collect();
			let project_write_path = d.skill_write_path(
				Some(project_root),
				aghub_core::models::ResourceScope::ProjectOnly,
			);
			let mutable_project = project_write_path.is_some();
			let project_write = project_write_path.map(format_path);
			let global_read = d
				.global_skill_read_paths()
				.into_iter()
				.map(format_path)
				.collect();
			let global_write = d
				.skill_write_path(
					None,
					aghub_core::models::ResourceScope::GlobalOnly,
				)
				.map(format_path);

			AgentInfo {
				id: d.id.to_string(),
				display_name: d.display_name.to_string(),
				capabilities: CapabilitiesDto {
					skills: SkillCapabilitiesDto {
						scopes: ScopeSupportDto {
							global: d.supports_skill_scope(
								aghub_core::models::ResourceScope::GlobalOnly,
							),
							project: d.supports_skill_scope(
								aghub_core::models::ResourceScope::ProjectOnly,
							),
						},
						universal: d.capabilities.skills.universal,
						mutable_global: d
							.skill_write_path(
								None,
								aghub_core::models::ResourceScope::GlobalOnly,
							)
							.is_some(),
						mutable_project,
					},
					mcp: McpCapabilitiesDto {
						scopes: ScopeSupportDto {
							global: d.supports_mcp_scope(
								aghub_core::models::ResourceScope::GlobalOnly,
							),
							project: d.supports_mcp_scope(
								aghub_core::models::ResourceScope::ProjectOnly,
							),
						},
						stdio: d.capabilities.mcp.stdio,
						remote: d.capabilities.mcp.remote,
						enable_disable: d.capabilities.mcp.enable_disable,
					},
					sub_agents: SubAgentCapabilitiesDto {
						scopes: ScopeSupportDto {
							global: d.supports_sub_agent_scope(
								aghub_core::models::ResourceScope::GlobalOnly,
							),
							project: d.supports_sub_agent_scope(
								aghub_core::models::ResourceScope::ProjectOnly,
							),
						},
					},
				},
				skills_paths: SkillsPathsDto {
					global_read,
					global_write,
					project_read,
					project_write,
				},
			}
		})
		.collect();
	Json(agents)
}

#[get("/agents/availability")]
pub fn check_availability(
	_origin: TrustedLocalOrigin,
) -> Json<Vec<AgentAvailabilityDto>> {
	let availability_info = availability::check_all_agents_availability();

	let dtos: Vec<AgentAvailabilityDto> = availability_info
		.into_iter()
		.map(|info| AgentAvailabilityDto {
			id: info.agent_id.to_string(),
			has_global_directory: info.has_global_directory,
			has_cli: info.has_cli,
			is_available: info.is_available,
		})
		.collect();

	Json(dtos)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_list_agents_includes_pi_without_mcp_capabilities() {
		let agents = list_agents(TrustedLocalOrigin).into_inner();
		let pi = agents
			.into_iter()
			.find(|agent| agent.id == "pi")
			.expect("pi agent should be listed");

		assert!(!pi.capabilities.mcp.stdio);
		assert!(!pi.capabilities.mcp.remote);
		assert!(pi.capabilities.skills.scopes.global);
	}
}

fn disabled_agents_dto() -> Result<DisabledAgentsDto, ApiError> {
	let stored = aghub_core::agent_settings::read_disabled_agents()
		.map_err(|e| ApiError::internal(format!("agent settings: {e}")))?;
	Ok(DisabledAgentsDto {
		configured: stored.is_some(),
		agents: stored.unwrap_or_default().into_iter().collect(),
	})
}

/// The agents aghub does not manage — the one selection every surface and
/// every server-side fan-out reads (`aghub_core::agent_settings`).
#[get("/agents/disabled")]
pub fn get_disabled_agents(
	_origin: TrustedLocalOrigin,
) -> ApiResult<DisabledAgentsDto> {
	disabled_agents_dto().map(Json)
}

/// Replace the selection. An unknown id is a 400, not silently dropped: the
/// caller would otherwise believe an agent is off that aghub still manages.
#[put("/agents/disabled", format = "json", data = "<body>")]
pub fn set_disabled_agents(
	body: Json<SetDisabledAgentsRequest>,
	_origin: TrustedLocalOrigin,
) -> ApiResult<DisabledAgentsDto> {
	let agents = body.into_inner().agents;
	if let Some(bad) = agents
		.iter()
		.find(|id| id.parse::<aghub_core::AgentType>().is_err())
	{
		return Err(ApiError::bad_request(format!("unknown agent '{bad}'")));
	}
	aghub_core::agent_settings::write_disabled_agents(
		&agents.into_iter().collect(),
	)
	.map_err(|e| ApiError::internal(format!("agent settings: {e}")))?;
	disabled_agents_dto().map(Json)
}

#[cfg(test)]
mod disabled_agents_tests {
	use super::*;

	/// The route writes where core's fan-outs read: a PUT here is what
	/// `aghub_core::agent_settings::disabled_agents` answers afterwards.
	#[test]
	fn put_persists_the_selection_core_reads() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let restore = std::env::var_os(aghub_core::paths::DATA_DIR_ENV);
		let data = tempfile::tempdir().expect("tempdir");
		std::env::set_var(aghub_core::paths::DATA_DIR_ENV, data.path());

		let before = get_disabled_agents(TrustedLocalOrigin).ok().unwrap().0;
		assert!(!before.configured && before.agents.is_empty());

		let put = |agents: &[&str]| {
			set_disabled_agents(
				Json(SetDisabledAgentsRequest {
					agents: agents.iter().map(|a| a.to_string()).collect(),
				}),
				TrustedLocalOrigin,
			)
		};
		let after = put(&["copilot"]).ok().unwrap().0;
		assert!(after.configured);
		assert_eq!(after.agents, vec!["copilot".to_string()]);
		assert!(!aghub_core::agent_settings::is_managed("copilot"));
		assert!(aghub_core::agent_settings::is_managed("claude"));

		assert!(put(&["not-an-agent"]).is_err(), "unknown id is refused");
		assert_eq!(
			get_disabled_agents(TrustedLocalOrigin)
				.ok()
				.unwrap()
				.0
				.agents,
			vec!["copilot".to_string()],
			"a refused PUT leaves the stored selection untouched"
		);

		match restore {
			Some(value) => {
				std::env::set_var(aghub_core::paths::DATA_DIR_ENV, value)
			}
			None => std::env::remove_var(aghub_core::paths::DATA_DIR_ENV),
		}
	}
}
