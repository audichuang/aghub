use anyhow::Result;

/// Dispatch a `source` subcommand action.
///
/// Stub: behavior is implemented in later tasks (list/diff/sync).
pub fn execute(
	action: &crate::SourceAction,
	global: bool,
	project: bool,
	all: bool,
	agent: &str,
) -> Result<()> {
	let _ = (action, global, project, all, agent);
	anyhow::bail!("source subcommand not yet implemented")
}
