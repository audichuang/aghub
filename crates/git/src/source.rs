use gix::bstr::ByteSlice;
use std::path::{Component, Path};
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSourceType {
	Github,
	Gitlab,
	Git,
}

impl RemoteSourceType {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Github => "github",
			Self::Gitlab => "gitlab",
			Self::Git => "git",
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRemoteSource {
	pub source: String,
	pub source_type: RemoteSourceType,
	pub source_url: String,
	pub clone_url: String,
	/// Lowercase host extracted from the source URL (e.g. `github.com`).
	/// `None` for inputs where no host can be derived (e.g. `owner/repo`
	/// shorthand that the caller already knows is GitHub-only — those resolve
	/// to `Some("github.com")`; this is `None` only for unresolvable inputs).
	pub host: Option<String>,
}

impl ResolvedRemoteSource {
	pub fn lock_source(&self) -> String {
		if self.source_url.starts_with("git@") {
			self.source_url.clone()
		} else {
			self.source.clone()
		}
	}
}

#[derive(Debug, Error)]
pub enum SourceError {
	#[error("Unsupported remote source '{0}'")]
	Unsupported(String),
	#[error("Invalid GitHub shorthand '{0}'")]
	InvalidGithubShorthand(String),
}

impl SourceError {
	/// Build an `InvalidGithubShorthand` error. The source is run through
	/// [`crate::redact::redact_url_userinfo`] first, so a `user:token@` a
	/// caller embedded in the URL can never survive into the error string —
	/// the same contract `GitError`'s constructors already hold. Construct
	/// this variant through here, never directly.
	///
	/// `Unsupported` has no constructor because nothing in the workspace
	/// builds it; add one (redacting, like this) if that changes.
	pub fn invalid_github_shorthand(source: impl Into<String>) -> Self {
		Self::InvalidGithubShorthand(crate::redact::redact_url_userinfo(
			&source.into(),
		))
	}
}

fn normalize_repo_path(path: &Path) -> Option<String> {
	let mut segments = path
		.components()
		.filter_map(|component| match component {
			Component::Normal(value) => value.to_str().map(str::to_string),
			_ => None,
		})
		.collect::<Vec<_>>();

	if segments.len() < 2 {
		return None;
	}

	if let Some(last) = segments.last_mut() {
		*last = last.trim_end_matches(".git").to_string();
		if last.is_empty() {
			return None;
		}
	}

	Some(segments.join("/"))
}

fn parse_github_repo_shorthand(
	source: &str,
) -> Result<(String, String), SourceError> {
	let trimmed = source.trim();
	let mut segments = if let Ok(parsed) = Url::parse(trimmed) {
		if parsed.scheme() != "github" {
			return Err(SourceError::invalid_github_shorthand(source));
		}

		let path = if let Some(host) = parsed.host_str() {
			let suffix = parsed.path().trim_matches('/');
			if suffix.is_empty() {
				host.to_string()
			} else {
				format!("{host}/{suffix}")
			}
		} else {
			parsed.path().trim_matches('/').to_string()
		};

		Path::new(&path)
			.components()
			.filter_map(|component| match component {
				Component::Normal(value) => value.to_str().map(str::to_string),
				_ => None,
			})
			.collect::<Vec<_>>()
	} else {
		let path = Path::new(trimmed);
		if path.has_root() {
			return Err(SourceError::invalid_github_shorthand(source));
		}
		path.components()
			.map(|component| match component {
				Component::Normal(value) => value.to_str().map(str::to_string),
				_ => None,
			})
			.collect::<Option<Vec<_>>>()
			.ok_or_else(|| SourceError::invalid_github_shorthand(source))?
	};

	if segments.len() != 2 {
		return Err(SourceError::invalid_github_shorthand(source));
	}

	if let Some(last) = segments.last_mut() {
		*last = last.trim_end_matches(".git").to_string();
		if last.is_empty() {
			return Err(SourceError::invalid_github_shorthand(source));
		}
	}

	Ok((segments[0].clone(), segments[1].clone()))
}

fn build_github_clone_url(owner: &str, repo: &str) -> Url {
	let mut url = Url::parse("https://github.com/")
		.expect("static GitHub base URL is valid");
	{
		let mut segments = url
			.path_segments_mut()
			.expect("GitHub base URL supports path segments");
		segments.push(owner);
		segments.push(&format!("{repo}.git"));
	}
	url
}

fn source_type_from_host(host: Option<&str>) -> RemoteSourceType {
	match host {
		Some("github.com") => RemoteSourceType::Github,
		Some("gitlab.com") => RemoteSourceType::Gitlab,
		_ => RemoteSourceType::Git,
	}
}

fn strip_scp_like_password(source: &str) -> Option<String> {
	if source.contains("://") {
		return None;
	}
	let (userinfo, remote) = source.split_once('@')?;
	let (user, password) = userinfo.split_once(':')?;
	if user.is_empty() || password.is_empty() || !remote.contains(':') {
		return None;
	}
	Some(format!("{user}@{remote}"))
}

pub fn normalize_repo_source_from_url(source_url: &str) -> Option<String> {
	let trimmed = source_url.trim();

	if let Ok(parsed) = Url::parse(trimmed) {
		let path = Path::new(parsed.path());
		return normalize_repo_path(path);
	}

	let parsed = gix::url::parse(trimmed.as_bytes().as_bstr()).ok()?;
	if matches!(parsed.scheme, gix::url::Scheme::File) {
		return None;
	}

	let repo_path = String::from_utf8_lossy(parsed.path.as_ref()).into_owned();
	normalize_repo_path(Path::new(&repo_path))
}

/// Normalize an Azure DevOps Server / TFS clone URL.
///
/// TFS serves repos at `.../_git/<repo>` and rejects the conventional trailing
/// `.git` suffix — it treats `ops-tools.git` as a repo literally named that and
/// returns TF401019. Strip a trailing `.git` for `/_git/`-style URLs only;
/// GitHub/GitLab (which accept or require `.git`) are returned unchanged.
pub fn normalize_tfs_clone_url(url: &str) -> String {
	if !url.contains("/_git/") {
		return url.to_string();
	}
	let trimmed = url.trim_end_matches('/');
	trimmed
		.strip_suffix(".git")
		.map(str::to_string)
		.unwrap_or_else(|| url.to_string())
}

pub fn resolve_remote_source(
	source: &str,
) -> Result<ResolvedRemoteSource, SourceError> {
	let trimmed = source.trim();
	let stripped_scp_password = strip_scp_like_password(trimmed);
	let parse_input = stripped_scp_password.as_deref().unwrap_or(trimmed);

	if let Ok(parsed) = Url::parse(parse_input) {
		match parsed.scheme() {
			"http" | "https" => {
				let source = normalize_repo_source_from_url(parsed.as_str())
					.unwrap_or_else(|| parsed.to_string());
				// Strip URL userinfo (user:token@) so credentials are never
				// persisted into source_url / clone_url.
				let mut clean = parsed.clone();
				let _ = clean.set_username("");
				let _ = clean.set_password(None);
				let clean_str = clean.to_string();
				return Ok(ResolvedRemoteSource {
					source,
					source_type: source_type_from_host(parsed.host_str()),
					source_url: clean_str.clone(),
					clone_url: clean_str,
					host: parsed.host_str().map(|h| h.to_lowercase()),
				});
			}
			"github" => {
				let (owner, repo) = parse_github_repo_shorthand(parse_input)?;
				let clone_url = build_github_clone_url(&owner, &repo);
				return Ok(ResolvedRemoteSource {
					source: format!("{owner}/{repo}"),
					source_type: RemoteSourceType::Github,
					source_url: clone_url.to_string(),
					clone_url: clone_url.to_string(),
					host: Some("github.com".to_string()),
				});
			}
			_ => {}
		}
	}

	if let Ok(mut parsed) = gix::url::parse(parse_input.as_bytes().as_bstr()) {
		if !matches!(parsed.scheme, gix::url::Scheme::File) {
			let normalized = normalize_repo_source_from_url(parse_input)
				.unwrap_or_else(|| parse_input.into());
			parsed.password = None;
			if !matches!(parsed.scheme, gix::url::Scheme::Ssh) {
				parsed.user = None;
			}
			let source_url =
				String::from_utf8_lossy(parsed.to_bstring().as_ref())
					.into_owned();

			return Ok(ResolvedRemoteSource {
				source: normalized,
				source_type: source_type_from_host(parsed.host()),
				source_url: source_url.clone(),
				clone_url: source_url,
				host: parsed.host().map(|h| h.to_lowercase()),
			});
		}
	}

	let (owner, repo) = parse_github_repo_shorthand(parse_input)?;
	let clone_url = build_github_clone_url(&owner, &repo);
	Ok(ResolvedRemoteSource {
		source: format!("{owner}/{repo}"),
		source_type: RemoteSourceType::Github,
		source_url: clone_url.to_string(),
		clone_url: clone_url.to_string(),
		host: Some("github.com".to_string()),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn normalizes_https_repo_sources() {
		let source = normalize_repo_source_from_url(
			"https://github.com/vercel-labs/agent-skills.git",
		);
		assert_eq!(source.as_deref(), Some("vercel-labs/agent-skills"));
	}

	#[test]
	fn normalizes_ssh_repo_sources() {
		let source = normalize_repo_source_from_url(
			"git@github.com:vercel-labs/agent-skills.git",
		);
		assert_eq!(source.as_deref(), Some("vercel-labs/agent-skills"));
	}

	#[test]
	fn resolves_github_shorthand() {
		let source = resolve_remote_source("vercel-labs/agent-skills").unwrap();
		assert_eq!(source.source, "vercel-labs/agent-skills");
		assert_eq!(source.source_type, RemoteSourceType::Github);
		assert_eq!(
			source.clone_url,
			"https://github.com/vercel-labs/agent-skills.git"
		);
	}

	#[test]
	fn rejects_explicit_relative_path_as_github_shorthand() {
		assert!(matches!(
			resolve_remote_source("./vercel-labs/agent-skills"),
			Err(SourceError::InvalidGithubShorthand(_))
		));
	}

	#[test]
	fn resolves_github_scheme_shorthand() {
		let source =
			resolve_remote_source("github:vercel-labs/agent-skills").unwrap();
		assert_eq!(source.source, "vercel-labs/agent-skills");
		assert_eq!(source.source_type, RemoteSourceType::Github);
	}

	#[test]
	fn resolve_remote_source_strips_userinfo() {
		let r =
			resolve_remote_source("https://user:ghp_SECRET@github.com/o/r.git")
				.unwrap();
		assert!(
			!r.source_url.contains("ghp_SECRET")
				&& !r.source_url.contains("user:")
		);
		assert!(
			!r.clone_url.contains("ghp_SECRET")
				&& !r.clone_url.contains("user:")
		);
	}

	#[test]
	fn resolve_remote_source_strips_non_ssh_gix_userinfo() {
		let r = resolve_remote_source("git://user:SECRET@github.com/o/r.git")
			.unwrap();
		assert_eq!(r.source_url, "git://github.com/o/r.git");
		assert_eq!(r.clone_url, "git://github.com/o/r.git");
	}

	#[test]
	fn resolve_remote_source_strips_password_from_ssh_but_keeps_user() {
		let r = resolve_remote_source(
			"ssh://git:SECRET@github.com/vercel-labs/agent-skills.git",
		)
		.unwrap();
		assert_eq!(
			r.source_url,
			"ssh://git@github.com/vercel-labs/agent-skills.git"
		);
		assert_eq!(r.clone_url, r.source_url);
		assert!(!r.source_url.contains("SECRET"));
	}

	#[test]
	fn resolve_remote_source_strips_scp_like_password() {
		let r = resolve_remote_source(
			"git:SECRET@github.com:vercel-labs/agent-skills.git",
		)
		.unwrap();
		assert_eq!(r.source_url, "git@github.com:vercel-labs/agent-skills.git");
		assert_eq!(r.clone_url, r.source_url);
		assert!(!r.source_url.contains("SECRET"));
	}

	#[test]
	fn strips_dotgit_from_tfs_urls() {
		assert_eq!(
			normalize_tfs_clone_url(
				"https://pialm01/tfs/DefaultCollection/IVTLXITP01-ITP/_git/ops-tools.git"
			),
			"https://pialm01/tfs/DefaultCollection/IVTLXITP01-ITP/_git/ops-tools"
		);
		assert_eq!(
			normalize_tfs_clone_url("https://host/col/_git/repo.git/"),
			"https://host/col/_git/repo"
		);
		assert_eq!(
			normalize_tfs_clone_url("https://host/col/_git/repo.git//"),
			"https://host/col/_git/repo"
		);
	}

	#[test]
	fn leaves_non_tfs_urls_untouched() {
		assert_eq!(
			normalize_tfs_clone_url("https://host/col/_git/repo"),
			"https://host/col/_git/repo"
		);
		assert_eq!(
			normalize_tfs_clone_url("https://github.com/owner/repo.git"),
			"https://github.com/owner/repo.git"
		);
		assert_eq!(normalize_tfs_clone_url(""), "");
	}

	#[test]
	fn resolves_git_protocol_sources() {
		let source = resolve_remote_source(
			"git://github.com/vercel-labs/agent-skills.git",
		)
		.unwrap();
		assert_eq!(source.source, "vercel-labs/agent-skills");
		assert_eq!(source.source_type, RemoteSourceType::Github);
	}
}
