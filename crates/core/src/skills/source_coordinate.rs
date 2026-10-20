//! The clone coordinate a lock entry names — one provider-aware rule for every
//! reader and writer of a lock entry's `sourceUrl`.

/// The clone coordinate a lock entry's content comes from — the ONE answer every
/// consumer must use (`owner/repo` alone reads as GitHub shorthand on every
/// forge). Callers that must agree: the Sources row's `source_url`, the grouping
/// origin, `diff_source`'s fetch, both update-check projections, CLI `source
/// diff`/`sync`, the bulk apply's `SourceRef`, a global entry's
/// `EntryIdentity`, accept-rename's global fetch, and a copy carried into the
/// global lock. Never hand-mirror
/// `source_url.unwrap_or(source)`. See
/// docs/history/skill-update.md#one-clone-coordinate-per-lock-entry
pub fn entry_clone_source(
	source: &str,
	source_url: Option<&str>,
	source_type: &str,
) -> String {
	source_url
		.map(str::trim)
		.filter(|recorded| !recorded.is_empty())
		.map(str::to_string)
		.unwrap_or_else(|| reconstruct_source_url(source, source_type))
}

/// Does [`reconstruct_source_url`] name the RIGHT repository for this entry,
/// rather than a guess? True for `github` (one host) and `local` (a path), and
/// for a source that spells its own host. NOT for a URL-less `gitlab`: npx
/// 1.5.11 records any self-hosted `/-/tree/` host as `gitlab` with the host
/// stripped, so `gitlab.com` is only the likeliest reading. A writer must not
/// persist that guess as if it were recorded.
pub fn reconstruction_is_unambiguous(source: &str, source_type: &str) -> bool {
	let source = source.trim();
	source_type.eq_ignore_ascii_case("github")
		|| source_type.eq_ignore_ascii_case("local")
		// A bare `@` (`group/repo@v1`) names no host; a URL or an SCP-like
		// `host:path` does.
		|| source.contains("://")
		|| source.contains(':')
}

/// The clone coordinate when the entry PROVES its host: the recorded URL, else
/// a rebuild that cannot guess ([`reconstruction_is_unambiguous`]). `None` for
/// an ambiguous legacy entry (a URL-less `gitlab:group/repo` may be
/// self-hosted) — a caller that fetches, compares or carries an owner must not
/// invent a host for it.
pub fn known_clone_source(
	source: &str,
	source_url: Option<&str>,
	source_type: &str,
) -> Option<String> {
	let recorded = source_url.map(str::trim).filter(|url| !url.is_empty());
	match recorded {
		Some(url) => Some(url.to_string()),
		None if reconstruction_is_unambiguous(source, source_type) => {
			Some(reconstruct_source_url(source, source_type))
		}
		None => None,
	}
}

/// `://`, `@` or `:` means the string carries its own transport/authority.
fn carries_authority(source: &str) -> bool {
	source.contains("://") || source.contains('@') || source.contains(':')
}

/// The [`entry_clone_source`] half that has to invent a coordinate, because the
/// entry recorded no `sourceUrl`.
///
/// Only two things can then name a host: the string itself, when it already
/// carries a transport or an authority, and the recorded provider — `github` and
/// `gitlab` are single-host services. A `git`/custom/`local` type keeps its own
/// spelling; inventing a host advertises, and then fetches from, a repository
/// nobody installed from.
pub fn reconstruct_source_url(source: &str, source_type: &str) -> String {
	let source = source.trim();
	if source_type.eq_ignore_ascii_case("local") {
		// A Windows path (`C:\skills\a`) would otherwise read as an SCP-like URL
		// whose host is the drive letter.
		return source.to_string();
	}
	// `://`, `@` or `:` means the string carries its own transport/authority, so
	// the resolver can read the host out of it and the provider is irrelevant.
	let carries_authority = carries_authority(source);
	let provider_host = match source_type.to_ascii_lowercase().as_str() {
		"github" => Some("github.com"),
		"gitlab" => Some("gitlab.com"),
		_ => None,
	};
	let candidate = match provider_host {
		Some(host) if !carries_authority => {
			let path = source.trim_matches('/');
			// An origin echoed back from a previous response already carries the
			// host; re-prefixing it would produce `host/host/path`. Hostnames are
			// case-insensitive, so the comparison is too — but it must land on a
			// SEGMENT boundary: `gitlab.comtools/sub/repo` is an owner literally
			// named `gitlab.comtools`, and stripping `gitlab.com` off it names a
			// different repository that may well exist.
			let path = path
				.get(..host.len())
				.filter(|head| head.eq_ignore_ascii_case(host))
				.map(|_| &path[host.len()..])
				// A trailing dot is the DNS root label: `gitlab.com.` names the
				// same host as `gitlab.com`.
				.map(|rest| rest.strip_prefix('.').unwrap_or(rest))
				.filter(|rest| rest.is_empty() || rest.starts_with('/'))
				.map_or(path, |rest| rest.trim_start_matches('/'));
			let path = path.trim_end_matches(".git").trim_matches('/');
			if path.is_empty() {
				return source.to_string();
			}
			format!("https://{host}/{path}.git")
		}
		_ if carries_authority => source.to_string(),
		// A self-hosted forge behind a `git`/custom type: unknowable.
		_ => return source.to_string(),
	};
	aghub_git::resolve_remote_source(&candidate)
		.map(|resolved| resolved.clone_url)
		.unwrap_or_else(|_| source.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Reconstruction may only produce a coordinate that names the SAME
	/// repository the entry records. A shape it cannot place must keep its own
	/// spelling — inventing one advertises, and then fetches from, a repository
	/// nobody installed from.
	#[test]
	fn reconstruction_never_invents_a_repository() {
		for (source, source_type, expected) in [
			// A provider names its host; the path is whatever remains.
			("owner/repo", "github", "https://github.com/owner/repo.git"),
			("group/repo", "gitlab", "https://gitlab.com/group/repo.git"),
			(
				"group/sub/repo",
				"gitlab",
				"https://gitlab.com/group/sub/repo.git",
			),
			// An origin echoed back already carries the host — in any case, and
			// with or without `.git`.
			(
				"github.com/owner/repo",
				"github",
				"https://github.com/owner/repo.git",
			),
			(
				"GitHub.com/owner/repo.git",
				"github",
				"https://github.com/owner/repo.git",
			),
			// A host with nothing after it names no repository.
			("gitlab.com", "gitlab", "gitlab.com"),
			("github.com/", "github", "github.com/"),
			// An owner whose name merely STARTS with the host is not an echoed
			// origin. Stripping the prefix here names a different repository —
			// one that may exist.
			(
				"gitlab.comtools/sub/repo",
				"gitlab",
				"https://gitlab.com/gitlab.comtools/sub/repo.git",
			),
			// …but a trailing dot IS the same host (the DNS root label), so the
			// boundary check must not treat it as part of the owner name.
			(
				"gitlab.com./group/repo",
				"gitlab",
				"https://gitlab.com/group/repo.git",
			),
			("gitlab.com.", "gitlab", "gitlab.com."),
			// No provider ⇒ no host to be had. `segments >= 3` once turned this
			// into `https://group/subgroup/repo`, a host that does not exist.
			("group/subgroup/repo", "git", "group/subgroup/repo"),
			("owner/repo", "git", "owner/repo"),
			("Coll/_git/repo", "git", "Coll/_git/repo"),
			// A local path keeps its spelling whatever it looks like; a Windows
			// drive letter would otherwise read as an SCP-like host.
			("/opt/skills/a", "local", "/opt/skills/a"),
			("C:\\skills\\a", "local", "C:\\skills\\a"),
			("./owner/repo", "local", "./owner/repo"),
		] {
			assert_eq!(
				reconstruct_source_url(source, source_type),
				expected,
				"source={source} type={source_type}"
			);
		}
	}

	/// A host is proven only by a provider that has one, a URL, or an SCP-like
	/// `host:path`; a bare `@` (a ref suffix) names none.
	#[test]
	fn only_a_spelled_host_makes_a_gitlab_source_unambiguous() {
		for (source, expected) in [
			("group/repo", false),
			("group/repo@v1", false),
			("https://gitlab.corp.example/group/repo.git", true),
			("git@gitlab.corp.example:group/repo.git", true),
		] {
			assert_eq!(
				reconstruction_is_unambiguous(source, "gitlab"),
				expected,
				"source={source}"
			);
		}
	}
}
