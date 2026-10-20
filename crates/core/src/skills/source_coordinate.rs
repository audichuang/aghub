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
	let carries_authority =
		source.contains("://") || source.contains('@') || source.contains(':');
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
