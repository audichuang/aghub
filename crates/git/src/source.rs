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
	// No `:` before the first `@` means no password: leave it alone, even when
	// the path holds `@` and `:` (`git@host:o/r@ref:sub`).
	let first_at = source.find('@')?;
	if !source[..first_at].contains(':') {
		return None;
	}
	// The password may itself hold an `@`, and so may the path after
	// `host:`; the userinfo ends at the LAST `@` that still has `host:path`
	// after it.
	let (userinfo, remote) = source
		.rmatch_indices('@')
		.map(|(at, _)| (&source[..at], &source[at + 1..]))
		.find(|(_, remote)| remote.contains(':'))?;
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

/// Does `source` carry a secret (password/token) in its userinfo? Never laxer
/// than the log redactor, which over-redacts on purpose (any `@` after a `:`):
/// it starts from the redactor's answer and only lets off shapes proven to
/// hold no secret.
pub fn source_has_credentials(source: &str) -> bool {
	let s = source.trim();
	crate::redact::redact_source_credentials(s) != s && !provably_secret_free(s)
}

/// Shapes the redactor flags that provably hold no secret. Ambiguous ones
/// stay flagged: scheme-less `git@host:o/r@v1` (scp + ref) reads the same as
/// `me@corp.com:TOK@host`.
/// ponytail: a clean scp source with an `@ref` still hands out `***@ref`.
/// Two shapes no parse tells apart stay let off: `git://u:12/PW@host` reads
/// like a clean `git://host:9418/repo@v1`, and `u:\PW@host\x` like
/// `C:\Users\a@b\skills`.
fn provably_secret_free(s: &str) -> bool {
	// Windows drive path (`C:\Users\a@b\skills`): the path goes on with `\`
	// after the last `@`, and no later `:` could be userinfo (`u:\PW@host/o/r`
	// is a password).
	if let [d, b':', b'\\', rest @ ..] = s.as_bytes() {
		return d.is_ascii_alphabetic()
			&& !rest.contains(&b':')
			&& s.rsplit_once('@')
				.is_some_and(|(_, tail)| tail.contains('\\'));
	}
	let Some((scheme, rest)) = s.split_once("://") else {
		return false;
	};
	// A remote-helper address (`codecommit::https://…`) is not parsed here,
	// and `Url` reads `\` as `/`, hiding `https://DOMAIN\user:PW@host`.
	let plain_scheme = !scheme.is_empty()
		&& scheme
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
	if !plain_scheme || rest.contains('\\') {
		return false;
	}
	if scheme.eq_ignore_ascii_case("ssh") {
		// An ssh user names an account. Everything before the last `@` must be
		// a bare user; `Url::parse` would reject `ssh://git@host:org/repo`.
		return rest
			.rsplit_once('@')
			.is_some_and(|(user, _)| !user.contains([':', '/', '?', '#']));
	}
	// The parser puts `host:8443/o/r@v1`'s `@` in the path. Only a single `@`
	// in the last path segment (a ref) is let off: `u:12/PW@host/o/r` reads as
	// port 12 too. An empty port (`u:/PW@host`) is a password, not a port.
	let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
	!authority.ends_with(':')
		&& rest.matches('@').count() == 1
		&& rest
			.rsplit_once('@')
			.is_none_or(|(_, tail)| !tail.contains('/'))
		&& Url::parse(s).is_ok_and(|url| {
			url.username().is_empty() && url.password().is_none()
		})
}

/// Raw text between the userinfo's `:` and the first `@` after it. The
/// shortest fragment on purpose: a short needle can only over-reject.
fn password_fragment(source: &str) -> Option<&str> {
	let s = source.trim();
	let body = s.split_once("://").map_or(s, |(_, rest)| rest);
	let userinfo = &body[..body.find('@')?];
	let pw = userinfo.split_once(':')?.1;
	(!pw.is_empty()).then_some(pw)
}

/// A recorded source safe to HAND OUT and still usable as one: the clean
/// `source_url` [`resolve_remote_source`] would persist (no https userinfo, no
/// password, an ssh user kept). A source carrying no secret (decided by
/// [`source_has_credentials`], not by comparing against the redactor) comes
/// back as is, so a lock identifier is never rewritten into a URL; an
/// unresolvable one falls back to [`crate::redact::redact_source_credentials`].
pub fn strip_source_credentials(source: &str) -> String {
	if !source_has_credentials(source) {
		return source.to_string();
	}
	// Resolution can hand the secret back: a scheme-less `:TOK@host:o/r` falls
	// through to GitHub shorthand and lands in the PATH. Accept the resolved
	// value only when it carries no userinfo secret AND not the input's
	// password.
	let secret = password_fragment(source);
	resolve_remote_source(source)
		.ok()
		.map(|resolved| resolved.source_url)
		.filter(|out| {
			!source_has_credentials(out)
				&& secret.is_none_or(|s| !out.contains(s))
		})
		.unwrap_or_else(|| crate::redact::redact_source_credentials(source))
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
	fn source_has_credentials_reads_userinfo_not_at_signs() {
		for (input, want) in [
			("ssh://git@gitlab.corp/group/repo.git", false),
			("ssh://git@host:org/repo", false),
			("git@github.com:o/r.git", false),
			("git://host:9418/o/r@v1", false),
			("https://host:8443/o/r@v1", false),
			("https://github.com/o/r.git", false),
			("o/r", false),
			("github:o/r", false),
			("gitlab:group/repo", false),
			("C:\\Users\\a@b\\skills", false),
			("/home/a@b/skills", false),
			("https://u:TOK@h/o/r", true),
			("https://TOK@github.com/o/r.git", true),
			("git://u:S@h/o/r", true),
			("ssh://git:PW@h/o/r", true),
			("u:TOK@git.example.com:o/r.git", true),
			("git:tok/part@github.com:o/r.git", true),
			("git:tok@part@github.com:o/r.git", true),
			("alice:TOK@host/o/r", true),
			("alice:tok@host", true),
			(":TOK@host:o/r", true),
			("codecommit::https://u:TOK@host/r", true),
			("https://u:p/a?b#c@host/x", true),
			("ssh://git:SEC/RET@gitlab.corp/group/repo.git", true),
			("ssh://git:p?w@h/o/r", true),
			("ssh://u:p/w@host/x", true),
			("u:/TOK@host:o/r", true),
			("me@corp.com:TOK@host:o/r", true),
			("me@corp.com:TOK@host/o/r", true),
			("me@corp.com:TOK@host", true),
			("u:tok/part@host", true),
			("u:tok\\part@host", true),
			("u:/TOK@host", true),
			("u:tok/a@b@host", true),
			// Ambiguous scheme-less shapes over-detect (safe direction).
			("git@github.com:o/r@v1", true),
			("git.example.com:o/r@v1", true),
			("C:/Users/a@b/skills", true),
			// Leaks a parse-first predicate let through.
			("https://CORP\\alice:PW@tfs.corp/tfs/coll/_git/repo", true),
			("HTTPS://SEK\\RT@host/o/r", true),
			("ssh://me@corp.com:PW/SECRET@host/o/r", true),
			("ssh://me@corp.com:PW#SECRET@host/o/r", true),
			("https://u:/PW@host/o/r", true),
			("git://u:/PW@host/o/r", true),
			("file://u:/PW@host/o/r", true),
			("a:\\PW@host:o/r.git", true),
			("C:\\x:PW@host:o/r.git", true),
			("codecommit::ssh://u:S@h/r", true),
			("git://u:12/PW@host/o/r", true),
			("u:\\PW@host/o/r", true),
		] {
			assert_eq!(source_has_credentials(input), want, "{input}");
		}
	}

	#[test]
	fn strip_keeps_clean_sources_that_hold_an_at_sign() {
		for input in [
			"git://host:9418/o/r@v1",
			"https://host:8443/o/r@v1",
			"C:\\Users\\a@b\\skills",
			"ssh://git@gitlab.corp/group/repo.git",
		] {
			assert_eq!(strip_source_credentials(input), input);
		}
	}

	#[test]
	fn strip_never_returns_a_secret_the_input_carried() {
		assert_eq!(strip_source_credentials(":TOK@host:o/r"), "***@host:o/r");
		for (input, secret) in [
			(":TOK@host:o/r", "TOK"),
			("https://u:TOK@h/o/r", "TOK"),
			("ssh://git:PW@h/o/r", "PW"),
			("u:TOK@git.example.com:o/r.git", "TOK"),
			("alice:TOK@host/o/r", "TOK"),
			("codecommit::https://u:TOK@host/r", "TOK"),
			("me@corp.com:TOK@host/o/r", "TOK"),
			("me@corp.com:TOK@host", "TOK"),
			("u:tok/part@host", "tok/part"),
			("u:/TOK@host", "TOK"),
		] {
			let out = strip_source_credentials(input);
			assert!(!out.contains(secret), "{input} -> {out}");
		}
	}

	#[test]
	fn stripped_sources_carry_no_secret_and_keep_an_ssh_user() {
		for (input, want) in [
			(
				"https://u:TOK@git.example.com/o/r.git",
				"https://git.example.com/o/r.git",
			),
			(
				"https://TOK@github.com/o/r.git",
				"https://github.com/o/r.git",
			),
			(
				"ssh://git@github.com/o/r.git",
				"ssh://git@github.com/o/r.git",
			),
			(
				"ssh://git:TOK@github.com/o/r.git",
				"ssh://git@github.com/o/r.git",
			),
			("o/r", "o/r"),
		] {
			assert_eq!(strip_source_credentials(input), want, "{input}");
		}
		let scp = strip_source_credentials("u:TOK@git.example.com:o/r.git");
		assert!(
			!scp.contains("TOK") && scp.contains("u@git.example.com"),
			"{scp}"
		);
		for leaky in [
			"alice:TOK@host/o/r",
			"git:TOK@part@github.com:o/r.git",
			"codecommit::https://u:TOK@host/r",
		] {
			let out = strip_source_credentials(leaky);
			assert!(
				!out.contains("TOK") && !out.contains("part"),
				"{leaky} -> {out}"
			);
		}
		assert_eq!(
			strip_source_credentials("https://u:TOK@host:8443/o/r@v1"),
			"https://host:8443/o/r@v1"
		);
		assert!(strip_scp_like_password("git@host:o/r@ref:sub").is_none());
		// What resolution PERSISTS into a lock: an `@` in the password or in
		// the path must not hide the userinfo from the scp strip.
		for (input, want) in [
			("git:TOK@part@github.com:o/r.git", "git@github.com:o/r.git"),
			("u:TOK@git.example.com:o/r@v1", "u@git.example.com:o/r@v1"),
		] {
			let url = resolve_remote_source(input).unwrap().source_url;
			assert!(
				!url.contains("TOK") && !url.contains("part"),
				"{input} -> {url}"
			);
			assert!(url.contains(want), "{input} -> {url}");
		}
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
