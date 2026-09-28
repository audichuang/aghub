# skill-update crate history

Why some rules in `crates/skill-update` look the way they do. The code comment
keeps the current rule; this file keeps what happened.

## REST budget decline refetches over gix

The original spec made a REST decline AFTER a successful resolve (blob
admission, a `truncated` tree) a clean error, on the premise that it "cannot
occur for a real single-skill repo". Blob admission broke that premise: a skill
with more files than the remaining anonymous budget (135 files against 60/hr)
was refused on every attempt, so "update all" answered 200 and wrote nothing.

Rule: `SkillRepository::fetch_or_regix` re-resolves the same ref over gix and
materializes ONLY if the tip is the same commit (gix cannot fetch a commit by
OID). Public `list` keeps the clean error.

Pinned by: `fetch_refused_by_rest_budget_after_resolve_is_served_by_gix_at_the_same_commit`,
`fetch_refused_by_rest_budget_errors_when_gix_sees_a_moved_tip`,
`pinned_fetch_refused_by_rest_budget_is_served_by_gix_at_the_same_commit`
(`tests/skill_repository.rs`), and aghub-api's
`apply_skill_updates_writes_a_skill_the_rest_budget_refused`.
Commit: f9003217.

## One clone coordinate per lock entry

`owner/repo` is the shape EVERY forge's lock identifier takes, so whoever
resolves it alone reads it as GitHub shorthand. While the Sources row
reconstructed a GitLab URL and the bulk apply resolved the raw `group/repo` on
its own, applying stamped GitHub's commit into a GitLab entry — silently,
whenever a same-path repo existed on GitHub. Each consumer hand-mirroring
`source_url.unwrap_or(source)` is what let them drift.

Rule: every consumer goes through `sources::entry_clone_source`.

Pinned by: `a_provider_typed_entry_applies_from_the_forge_the_row_advertises`
(`tests/source_bulk_sync.rs`), `two_forges_serving_one_path_are_two_source_rows`,
`source_type_selects_forge_when_project_source_url_is_missing`
(`tests/source_bulk_sync_global.rs`).
Commit: df8321a3.

## Source membership has one definition

Two drifts produced `sources::source_matches` as the ONE membership predicate:

- When the Sources grouping admitted an entry the predicate's own resolution
  could not, a row's diff was judged against one repository while its apply
  installed from another, and a stricter check in `mutation.rs` rejected rows
  the caller had correctly been shown (an error no refresh could clear).
- `want_origin` once read a caller's bare `owner/repo` as GitHub, like an
  entry's. `resolve_remote_source` strips the host when it records `source`, so
  that is the shape EVERY forge's identifier takes — a caller naming a GitLab
  row selected GitHub's tree, reported every skill as not-installed, and offered
  to install from that unrelated repository.

Rule: grouping, `diff_source` and the bulk resync's `source_group` check all
call `source_matches`; a caller's `want` is host-blind unless it carries a
transport or authority.

Pinned by: `two_forges_serving_one_path_are_two_source_rows`,
`project_two_forges_serving_one_path_are_two_source_rows`
(`tests/source_bulk_sync_global.rs`).
Commit: df8321a3.
