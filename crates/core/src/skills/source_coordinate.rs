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

/// A Source's identity: the repository's ORIGIN — `host[:port]/path`, lower-cased
/// authority, no userinfo, no `.git`. Two forges serving the same path are two
/// Sources; two spellings of one repo (ssh vs https, with or without `.git`) are
/// one. The explicit-transport normalization is shared with the install-time
/// owner check and `EntryIdentity` via `aghub_core`; provider fallback and
/// unresolved-entry identity are intentionally owned here.
///
/// Accepts whatever a caller holds: the recorded `sourceUrl`, an `owner/repo`
/// shorthand, or an origin echoed back from a previous response — all through
/// [`reconstruct_source_url`], the same coordinate apply fetches.
///
/// Unresolvable entries get a `unresolved:<type>:<spelling>` key: a local
/// directory named `github.com/owner/repo` would otherwise collide with a real
/// GitHub entry's origin.
///
/// Some legacy shapes remain undecidable: `sourceType: "git"` (or another
/// custom type) plus no `sourceUrl` cannot reveal which self-hosted forge owns
/// `owner/repo`, and a TFS collection path cannot reveal its server. Two such
/// entries with the same type and spelling still cannot be separated.
pub fn source_origin(
	entry_source: &str,
	entry_source_url: Option<&str>,
	entry_source_type: &str,
) -> String {
	resolvable_origin(entry_source, entry_source_url, entry_source_type)
		.unwrap_or_else(|| {
			format!(
				"unresolved:{}:{}",
				entry_source_type.trim().to_ascii_lowercase(),
				entry_source.trim()
			)
		})
}

/// `Some` when a host can be derived, `None` for a HOST-BLIND identifier (a TFS
/// collection path, a local directory) — which cannot separate two forges, so
/// it must never be compared as an origin.
pub fn resolvable_origin(
	entry_source: &str,
	entry_source_url: Option<&str>,
	entry_source_type: &str,
) -> Option<String> {
	// Recorded URLs first — they carry the real host. The RAW `entry_source`
	// is deliberately absent: `remote_owner_from_url` reads `host:8443/o/r` as
	// SCP-like (path `8443/o/r`), which would make the key non-idempotent.
	let candidates = [
		entry_source_url.map(str::to_string),
		entry_source_url
			.map(|source| reconstruct_source_url(source, entry_source_type)),
		Some(reconstruct_source_url(entry_source, entry_source_type)),
	];
	candidates.into_iter().flatten().find_map(|candidate| {
		crate::skills::install_fetched::remote_owner_from_url(&candidate)
	})
}

/// The origin a CALLER's `want` names, or `None` when it carries no host.
///
/// Deliberately stricter than [`resolvable_origin`], which reads a bare
/// `owner/repo` as a GitHub coordinate. That reading is right for an ENTRY — an
/// npx-written lock records exactly that and it IS GitHub — but wrong for a
/// `want`: `resolve_remote_source` strips the host when it records `source`, so
/// `owner/repo` is the shape EVERY forge's lock identifier takes, and it is what
/// `aghub source list` prints. See
/// docs/history/skill-update.md#source-membership-has-one-definition
pub fn want_origin(want: &str) -> Option<String> {
	let trimmed = want.trim();
	// No transport and no authority separator ⇒ a host-blind identifier.
	if !trimmed.contains("://")
		&& !trimmed.contains('@')
		&& !trimmed.contains(':')
	{
		return None;
	}
	resolvable_origin(trimmed, None, "git")
}

/// Whether a lock entry belongs to the requested source.
///
/// The ONE definition of Source membership: the Sources list groups by it,
/// `diff_source` selects by it, and `mutation.rs` checks a bulk caller's row
/// identity with it. They MUST agree. See
/// docs/history/skill-update.md#source-membership-has-one-definition
pub fn source_matches(
	want: &str,
	entry_source: &str,
	entry_source_url: Option<&str>,
	entry_source_type: &str,
) -> bool {
	if entry_source_url == Some(want) {
		return true;
	}
	let entry =
		source_origin(entry_source, entry_source_url, entry_source_type);
	match want_origin(want) {
		// `want` names a host, so compare exactly — this is what stops a row from
		// admitting another forge serving the same path. A bare
		// `entry_source == want` would not: the lock's identifier is host-blind.
		Some(origin) => origin == entry,
		// `want` is host-blind (a TFS collection path, a local directory). It
		// cannot separate forges, and the caller has given nothing finer, so fall
		// back to the identifier the lock itself records.
		None => entry_source == want,
	}
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

#[cfg(test)]
mod origin_tests {
	use super::*;

	/// Every spelling of one repository must collapse to one origin, and every
	/// distinct forge must stay separate — this is what a Sources row IS.
	#[test]
	fn one_repo_many_spellings_is_one_origin() {
		let canonical = "github.com/owner/repo";
		for (source, source_url) in [
			("owner/repo", Some("https://github.com/owner/repo.git")),
			("owner/repo", Some("https://github.com/owner/repo")),
			("owner/repo", Some("git@github.com:owner/repo.git")),
			("owner/repo", Some("ssh://git@github.com/owner/repo.git")),
			(
				"owner/repo",
				Some("https://token@github.com/owner/repo.git"),
			),
			// npx-written project lock: no `sourceUrl`, the shorthand IS the
			// GitHub coordinate.
			("owner/repo", None),
			// An origin echoed back from a previous response.
			("github.com/owner/repo", None),
		] {
			assert_eq!(
				source_origin(source, source_url, "github"),
				canonical,
				"source={source} url={source_url:?}"
			);
		}
	}

	#[test]
	fn a_different_forge_or_port_is_a_different_origin() {
		let github = source_origin(
			"owner/repo",
			Some("https://github.com/owner/repo.git"),
			"github",
		);
		for other in [
			"https://gitlab.com/owner/repo.git",
			"https://github.example.com/owner/repo.git",
			// Port matters: two forges on one host are two repositories, which
			// is why a host-only comparison is too loose (same reason
			// credential origin pinning includes the port).
			"https://github.com:8443/owner/repo.git",
		] {
			assert_ne!(
				source_origin("owner/repo", Some(other), "git"),
				github,
				"{other} must not collapse into {github}"
			);
		}
	}

	/// The value `source list` PRINTS must select the row it came from. For every
	/// non-GitHub HTTPS source that value is a bare `owner/repo`, because
	/// `resolve_remote_source` strips the host when it records `source` — so
	/// reading a want-side shorthand as GitHub silently pointed the caller at
	/// github.com, reported every skill as not-installed, and offered to install
	/// from that unrelated repository.
	#[test]
	fn a_host_blind_want_selects_its_own_entry_on_any_forge() {
		for url in [
			"https://gitlab.com/owner/repo.git",
			"https://git.example.com:8443/owner/repo.git",
			"https://github.com/owner/repo.git",
		] {
			assert!(
				source_matches("owner/repo", "owner/repo", Some(url), "git",),
				"the printed identifier must still select {url}"
			);
		}
		// A want that DOES carry a host stays exact — that is what keeps one row
		// from admitting another forge serving the same path.
		assert!(!source_matches(
			"https://github.com/owner/repo.git",
			"owner/repo",
			Some("https://gitlab.com/owner/repo.git"),
			"gitlab",
		));
	}

	/// A local/unresolvable source keeps its own spelling, so two distinct local
	/// directories never merge into one row.
	#[test]
	fn unresolvable_sources_keep_their_spelling() {
		assert_eq!(
			source_origin("/opt/skills/a", None, "local"),
			"unresolved:local:/opt/skills/a"
		);
		assert_ne!(
			source_origin("/opt/skills/a", None, "local"),
			source_origin("/opt/skills/b", None, "local")
		);
	}

	/// Membership follows the same origin, in both directions.
	#[test]
	fn membership_admits_only_the_same_origin() {
		assert!(source_matches(
			"https://github.com/owner/repo.git",
			"owner/repo",
			Some("https://github.com/owner/repo"),
			"github",
		));
		// An ORIGIN is an internal grouping key, deliberately NOT an accepted
		// input: it is not a coordinate anything can fetch from, and echoing one
		// back used to yield a different origin than the row it came from (the
		// port case parsed as SCP-like). Callers pass the row's `source` or its
		// `sourceUrl`.
		assert!(!source_matches(
			"github.com/owner/repo",
			"owner/repo",
			Some("https://github.com/owner/repo.git"),
			"github",
		));
		// A host-blind identifier the lock records but we cannot resolve (TFS
		// collection path) still matches itself — nothing finer is available.
		assert!(source_matches(
			"Coll/_git/repo",
			"Coll/_git/repo",
			Some("https://tfs.example.com/tfs/Coll/_git/repo"),
			"git",
		));
		assert!(
			!source_matches(
				"https://github.com/owner/repo.git",
				"owner/repo",
				Some("https://gitlab.com/owner/repo.git"),
				"gitlab",
			),
			"a same-path entry on another forge is NOT part of this row"
		);
	}
}
