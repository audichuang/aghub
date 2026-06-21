# Phase 3 — Accept-rename atomic backend op + ts-rs state union + human-readable version/date Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ] ) syntax for tracking.

**Goal:** Introduce a new atomic `POST /skills/accept-rename` route that installs the upstream-current skill under the new name and deletes the old-named skill in a single rollback-capable transaction, export `SourceSkillState` as a proper TypeScript union via a parallel DTO enum, and add a git commit-timestamp field to both `SkillUpdateStatusResponse::UpdateAvailable` and `SourceSkillDiff` to enable "updated N days ago" human-readable display.

**Architecture:** The accept-rename op is built from three existing primitives — `install_fetched_skill_and_lock` (install new name into agent dirs + lock), `plan_removal`/`execute_removal` (remove old-name dirs + lock entry), and `stage_and_swap_dir`/`sanitize_skill_path` (containment + atomic swap) — wired together in a new `crates/api/src/routes/skills_update.rs` inner function that rolls back the install on removal failure. The ts-rs union is a parallel enum in `crates/api/src/dto/sources.rs` that mirrors the wire strings from `skill_update::sources::SourceSkillState::as_wire()` and is serialised with `#[serde(rename_all = "camelCase")]`; the domain struct's `state: String` field is replaced by this DTO enum. The git commit-timestamp is a new optional `upstream_commit_time: Option<String>` (RFC 3339) field populated from gix commit metadata at fetch time in `crates/skill-update/src/git.rs` and threaded through `FetchedRepo` → `CheckOutput` → `SkillUpdateStatusResponse::UpdateAvailable` and through `SourceSkillDiff`.

**Tech Stack:** Rust / Rocket v0.5 / ts-rs / gix (already a dep in `crates/skill-update`) / React 19 / HeroUI v3 / Tailwind v4 / TanStack Query / bun

---

## ⚠️ Codex 審查修正（實作前必讀；覆寫下方對應步驟）

> GPT-5.5 對著真實程式碼（含 gix 0.84.0 registry 原始碼）審查後的必改項。判定：**needs-rework**。已確認 OK：§12-C4 平行 DTO enum 是對的做法、`crates/skill-update/src/git.rs` 已存在、API route 掛載點存在。

- **[P0] accept-rename 不原子（~:1112）**：現設計舊 skill 刪除 non-fatal、rollback 不還原已刪的舊 skill，違反 spec「單一交易」（spec §91）。`execute_removal` 可能部分刪除並回傳失敗（`removal.rs:419`）。改：install 前**快照舊 dirs + 舊 lock entry**；若 install／舊刪除／舊 lock 移除任一失敗 → 還原舊 dirs、只刪剛建立的新名路徑、還原 lock 快照、回傳失敗。**舊 lock 移除不可 log-and-continue**。
- **[P1] install 目標用了 `AgentType::ALL`（~:983）**：會把技能擴散到原本沒裝舊 skill 的 agent。改：`target_agents` 由 `load_all_agents(...).filter(|r| r.skills.iter().any(|s| s.name == req.old_name)).filter_map(|r| AgentType::from_str(r.agent_id).ok())` 算出，為空則 error（對齊 apply-update 只更新已裝 roots，`apply_update.rs:86`）。
- **[P1] `LinkTarget` 沒分 scope（~:996）**：全用 `Absolute`。改：`target: if matches!(resource_scope, ResourceScope::ProjectOnly) { LinkTarget::Relative } else { LinkTarget::Absolute }`（`linker/mod.rs:28`）。
- **[P1] lock 移除 closure 形狀（~:1144）**：`modify_skill_lock` 的 closure 回 `Result` 會變巢狀 `Result<Result<(),_>,_>`（`lock/io.rs:118`）對不上 `Result<(), String>`。改用非 fallible closure：`skill::lock::global::modify_skill_lock(|lock| { lock.skills.remove(name); }).map_err(|e| format!(...))`（local 同）。
- **[P1] gix commit-time API（~:395）**：`commit.author().ok()?.time.seconds` 在 gix 0.84.0 **不能編**（`SignatureRef.time` 是 `&str`，`gix-actor-0.41.1`）。改：`let time = commit.author().ok()?.time().ok()?;` 再讀 `time.seconds` / `time.offset`。
- **[P1] `classify_repo_skills` 無法塞 timestamp（~:497）**：現只收 `root, baseline`（`sources.rs:482`）。改簽名 `classify_repo_skills(root, baseline, upstream_commit_time)` + `build_source_skill_diffs(..., upstream_commit_time)`，且僅在 `state == InstalledOutdated` 時設值。
- **[P2] DTO regen 指令（~:1281）**：`just generate:dto` 不存在。用 `cd crates/desktop && bun run generate:dto && bun run format -- src/generated/dto`（script 在 `crates/desktop/package.json:15`）。
- **[note] chrono**：若用 RFC3339 格式化 `upstream_commit_time`，`chrono` **不是** `skill-update` 的 dep（`Cargo.toml:23`），需新增；或直接存 unix seconds + offset 由前端格式化。Architecture 寫「gix already a dep」正確，但別誤以為 chrono 也在。

## File Structure

| File                                     | Status              | Responsibility                                                                                                                                                          |
| ---------------------------------------- | ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- | -------------------------------------- |
| `crates/api/src/dto/skill.rs`            | Modified            | Add `upstream_commit_time` field to `SkillUpdateStatusResponse::UpdateAvailable`; add new `AcceptRenameRequest` / `AcceptRenameResponse` DTOs                           |
| `crates/api/src/dto/sources.rs`          | Modified            | Replace `pub state: String` with `pub state: SourceSkillStateDto`; add `SourceSkillStateDto` enum with `#[derive(TS)]`; add `upstream_commit_time` to `SourceSkillDiff` |
| `crates/api/src/routes/skills_update.rs` | Modified            | Add `accept_skill_rename()` route handler + `accept_rename_inner()` testable inner function                                                                             |
| `crates/api/src/routes/sources.rs`       | Modified            | Map domain `SourceSkillState` → `SourceSkillStateDto` when constructing `SourceSkillDiff` DTOs                                                                          |
| `crates/api/src/lib.rs`                  | Modified            | Mount `routes::skills_update::accept_skill_rename` in `build_rocket()` `routes![...]` (§12-GAP6)                                                                        |
| `crates/skill-update/src/lib.rs`         | Modified            | Add `upstream_commit_time: Option<String>` to `FetchedRepo`; thread it through `CheckOutput` for `UpdateAvailable`                                                      |
| `crates/skill-update/src/git.rs`         | Modified            | Populate `upstream_commit_time` from tip commit author-time via gix at fetch time                                                                                       |
| `crates/skill-update/src/sources.rs`     | Modified            | Add `upstream_commit_time: Option<String>` to domain `SourceSkillDiff` struct                                                                                           |
| `crates/desktop/src/generated/dto/`      | Regenerated         | ts-rs output after DTO changes — run `cargo test -p aghub-api --features ts-rs-export 2>/dev/null                                                                       |     | cargo test -p aghub-api` then prettier |
| `crates/cli/src/commands/source.rs`      | Modified (optional) | Add `accept-rename` subcommand to `source` for CLI/app parity (Task 6, optional)                                                                                        |
| `crates/cli/src/commands/mod.rs`         | Modified (optional) | Wire optional `accept-rename` dispatch (Task 6, optional)                                                                                                               |

---

### Task 1: New `AcceptRenameRequest`/`AcceptRenameResponse` DTOs

**Files:**

- Modify: `crates/api/src/dto/skill.rs` (after line 490, the end of `ApplySkillUpdateResponse`)
- Test path: `crates/api/src/dto/skill.rs` (in-file `#[cfg(test)]` block)

**Context:** `ApplySkillUpdateRequest` is at line 466; `ApplySkillUpdateResponse` ends at line 490. The new DTOs go immediately after. The `TS` derive already has all needed imports.

- [ ] **Step 1.1 — Write failing DTO serialisation test**

    Add inside the existing `#[cfg(test)] mod tests` block in `crates/api/src/dto/skill.rs`:

    ```rust
    #[test]
    fn accept_rename_request_deserializes() {
    	let json = r#"{"oldName":"a","newName":"b","scope":"global","confirm":true}"#;
    	let req: AcceptRenameRequest =
    		serde_json::from_str(json).expect("must deserialise");
    	assert_eq!(req.old_name, "a");
    	assert_eq!(req.new_name, "b");
    	assert_eq!(req.scope, "global");
    	assert_eq!(req.confirm, Some(true));
    }

    #[test]
    fn accept_rename_response_serializes_success() {
    	let resp = AcceptRenameResponse {
    		success: true,
    		old_name: "a".to_string(),
    		new_name: "b".to_string(),
    		scope: "global".to_string(),
    		installed_hash: Some("abc123".to_string()),
    		paths: vec!["/some/path".to_string()],
    		error: None,
    		code: None,
    	};
    	let val = serde_json::to_value(&resp).unwrap();
    	assert_eq!(val["success"], true);
    	assert_eq!(val["oldName"], "a");
    	assert_eq!(val["newName"], "b");
    	assert!(val.get("error").is_none() || val["error"].is_null());
    }
    ```

    Run (expect FAIL — types not defined yet):

    ```bash
    cargo test -p aghub-api dto::skill::tests::accept_rename_request_deserializes -- --exact 2>&1 | tail -5
    # expected: error[E0412]: cannot find type `AcceptRenameRequest`
    ```

- [ ] **Step 1.2 — Add DTOs to `skill.rs`**

    Add immediately after `ApplySkillUpdateResponse` (after the `code` field closing brace at line ~490):

    ```rust
    /// Request to atomically rename an installed skill:
    /// install upstream-current under `new_name` + delete `old_name`.
    #[derive(Debug, Clone, Deserialize, TS)]
    #[ts(export)]
    #[serde(rename_all = "camelCase")]
    pub struct AcceptRenameRequest {
    	/// Locked name of the installed skill to replace.
    	pub old_name: String,
    	/// New upstream name (from the `renamed.newName` field).
    	pub new_name: String,
    	pub scope: String,
    	pub project_root: Option<String>,
    	/// Must be `true` to execute.  Absent / false → dry-run description only.
    	pub confirm: Option<bool>,
    }

    /// Response from `POST /skills/accept-rename`.
    #[derive(Debug, Clone, Serialize, TS)]
    #[ts(export)]
    #[serde(rename_all = "camelCase")]
    pub struct AcceptRenameResponse {
    	pub success: bool,
    	pub old_name: String,
    	pub new_name: String,
    	pub scope: String,
    	#[serde(skip_serializing_if = "Option::is_none")]
    	#[ts(optional)]
    	pub installed_hash: Option<String>,
    	pub paths: Vec<String>,
    	#[serde(skip_serializing_if = "Option::is_none")]
    	#[ts(optional)]
    	pub error: Option<String>,
    	#[serde(skip_serializing_if = "Option::is_none")]
    	#[ts(optional)]
    	pub code: Option<String>,
    }
    ```

    Run (expect PASS):

    ```bash
    cargo test -p aghub-api dto::skill::tests::accept_rename_request_deserializes -- --exact
    cargo test -p aghub-api dto::skill::tests::accept_rename_response_serializes_success -- --exact
    ```

- [ ] **Step 1.3 — Commit**
    ```bash
    git add crates/api/src/dto/skill.rs
    git commit -m "feat(api/dto): add AcceptRenameRequest/Response DTOs"
    ```

---

### Task 2: `SourceSkillStateDto` enum — ts-rs union (§12-C4/GAP5)

**Files:**

- Modify: `crates/api/src/dto/sources.rs`
- Modify: `crates/api/src/routes/sources.rs` (the `DomainSkillDiff → SourceSkillDiff` mapping)

**Context:** `SourceSkillDiff.state` is currently `pub state: String` (line 77 of `sources.rs`). `skill_update::sources::SourceSkillState` in `crates/skill-update/src/sources.rs` lines 37-58 defines the domain enum with `as_wire()` returning camelCase strings: `notInstalled`, `installedCurrent`, `installedOutdated`, `renamed`, `removed`, `deprecated`, `uncheckable`. The domain crate has no ts-rs dep; recommended approach B is a parallel DTO enum in `crates/api/src/dto/sources.rs`.

- [ ] **Step 2.1 — Write failing test that asserts `state` is NOT `String` in generated TS**

    Create a compile-time / serde test at the bottom of `crates/api/src/dto/sources.rs`:

    ```rust
    #[cfg(test)]
    mod tests {
    	use super::*;

    	#[test]
    	fn source_skill_state_dto_serializes_to_camel_case_strings() {
    		assert_eq!(
    			serde_json::to_string(&SourceSkillStateDto::NotInstalled).unwrap(),
    			r#""notInstalled""#
    		);
    		assert_eq!(
    			serde_json::to_string(&SourceSkillStateDto::InstalledCurrent).unwrap(),
    			r#""installedCurrent""#
    		);
    		assert_eq!(
    			serde_json::to_string(&SourceSkillStateDto::InstalledOutdated)
    				.unwrap(),
    			r#""installedOutdated""#
    		);
    		assert_eq!(
    			serde_json::to_string(&SourceSkillStateDto::Renamed).unwrap(),
    			r#""renamed""#
    		);
    		assert_eq!(
    			serde_json::to_string(&SourceSkillStateDto::Removed).unwrap(),
    			r#""removed""#
    		);
    		assert_eq!(
    			serde_json::to_string(&SourceSkillStateDto::Deprecated).unwrap(),
    			r#""deprecated""#
    		);
    		assert_eq!(
    			serde_json::to_string(&SourceSkillStateDto::Uncheckable).unwrap(),
    			r#""uncheckable""#
    		);
    	}

    	#[test]
    	fn source_skill_diff_state_field_is_not_plain_string() {
    		// Verifies the DTO uses the enum (compile-time): if state is String,
    		// this assignment would still compile but the serde output would differ.
    		let diff = SourceSkillDiff {
    			name: "foo".to_string(),
    			skill_path: "foo/SKILL.md".to_string(),
    			description: None,
    			version: None,
    			author: None,
    			state: SourceSkillStateDto::InstalledCurrent,
    			previous_name: None,
    			reason: None,
    			installed_paths: vec![],
    			upstream_commit_time: None,
    		};
    		let val = serde_json::to_value(&diff).unwrap();
    		assert_eq!(val["state"], "installedCurrent");
    	}
    }
    ```

    Run (expect FAIL — `SourceSkillStateDto` not defined, `upstream_commit_time` not defined):

    ```bash
    cargo test -p aghub-api dto::sources::tests::source_skill_state_dto_serializes_to_camel_case_strings -- --exact 2>&1 | tail -5
    ```

- [ ] **Step 2.2 — Add `SourceSkillStateDto` enum + update `SourceSkillDiff` in `sources.rs`**

    In `crates/api/src/dto/sources.rs`, add the new enum before `SourceSummaryResponse` and update the struct:

    ```rust
    /// TypeScript-exported union mirroring
    /// `skill_update::sources::SourceSkillState::as_wire()`. Declare here
    /// (in the API DTO crate, which has ts-rs) rather than in `skill-update`
    /// (which does not) — approach B from §12-C4/GAP5.
    #[derive(Debug, Clone, Serialize, TS)]
    #[ts(export)]
    #[serde(rename_all = "camelCase")]
    pub enum SourceSkillStateDto {
    	NotInstalled,
    	InstalledCurrent,
    	InstalledOutdated,
    	Renamed,
    	Removed,
    	Deprecated,
    	Uncheckable,
    }

    impl From<&skill_update::sources::SourceSkillState> for SourceSkillStateDto {
    	fn from(s: &skill_update::sources::SourceSkillState) -> Self {
    		use skill_update::sources::SourceSkillState;
    		match s {
    			SourceSkillState::NotInstalled => Self::NotInstalled,
    			SourceSkillState::InstalledCurrent => Self::InstalledCurrent,
    			SourceSkillState::InstalledOutdated => Self::InstalledOutdated,
    			SourceSkillState::Renamed => Self::Renamed,
    			SourceSkillState::Removed => Self::Removed,
    			SourceSkillState::Deprecated => Self::Deprecated,
    			SourceSkillState::Uncheckable => Self::Uncheckable,
    		}
    	}
    }
    ```

    Then replace `pub state: String,` in `SourceSkillDiff` with:

    ```rust
    /// Typed state enum (ts-rs generates a TS union, not `string`).
    pub state: SourceSkillStateDto,
    ```

    Also add `upstream_commit_time` field (needed for Task 4 too):

    ```rust
    /// RFC 3339 timestamp of the upstream tip commit at diff time.
    /// Present only for `installedOutdated` rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub upstream_commit_time: Option<String>,
    ```

    Run (expect PASS):

    ```bash
    cargo test -p aghub-api dto::sources::tests::source_skill_state_dto_serializes_to_camel_case_strings -- --exact
    cargo test -p aghub-api dto::sources::tests::source_skill_diff_state_field_is_not_plain_string -- --exact
    ```

- [ ] **Step 2.3 — Fix `routes/sources.rs` mapping from domain `SourceSkillDiff` to DTO `SourceSkillDiff`**

    In `crates/api/src/routes/sources.rs`, the mapping from `DomainSkillDiff` to the DTO `SourceSkillDiff` currently passes `state: diff.state.as_wire().to_string()`. Find this mapping (search for `state:` inside the `From<DomainSkillDiff>` impl or the `.map()` closure) and change it to:

    ```rust
    state: SourceSkillStateDto::from(&diff.state),
    upstream_commit_time: diff.upstream_commit_time.clone(),
    ```

    ASSUMPTION: The mapping from domain `DomainSkillDiff` to DTO `SourceSkillDiff` is done inline in the `diff_source` route handler or a helper; if it is a `From` impl, update the `From` impl similarly. Grep for `as_wire` to find the exact location.

    Build check:

    ```bash
    cargo build -p aghub-api 2>&1 | grep -E "^error" | head -10
    # expected: no errors
    ```

- [ ] **Step 2.4 — Commit**
    ```bash
    git add crates/api/src/dto/sources.rs crates/api/src/routes/sources.rs
    git commit -m "feat(dto): replace stringly-typed SourceSkillDiff.state with ts-rs SourceSkillStateDto union"
    ```

---

### Task 3: Thread `upstream_commit_time` through the fetch pipeline (§12-C5)

**Files:**

- Modify: `crates/skill-update/src/lib.rs` (`FetchedRepo` struct + `CheckOutput`)
- Modify: `crates/skill-update/src/git.rs` (`GitFetcher::fetch` — populate commit time)
- Modify: `crates/skill-update/src/sources.rs` (domain `SourceSkillDiff` + `diff_source`)
- Modify: `crates/api/src/dto/skill.rs` (`SkillUpdateStatusResponse::UpdateAvailable`)
- Modify: `crates/api/src/routes/skills_update.rs` (`CheckOutput` → `SkillUpdateResponse` mapping)

**Context:** `FetchedRepo` at `lib.rs:200` has `root`, `oid`, `_guard`. `CheckOutput` at `lib.rs` is somewhere in the orchestrator output path. `SkillUpdateStatusResponse::UpdateAvailable` at `dto/skill.rs:402` currently carries only `current` and `available` (both content hashes). The gix crate is already a dependency of `skill-update` (used by `GitFetcher`).

- [ ] **Step 3.1 — Write failing test for `upstream_commit_time` in `UpdateAvailable` DTO**

    Add to the `#[cfg(test)] mod tests` block in `crates/api/src/dto/skill.rs`:

    ```rust
    #[test]
    fn update_available_with_commit_time_serializes() {
    	let resp = SkillUpdateResponse {
    		name: "s".to_string(),
    		scope: "global".to_string(),
    		status: SkillUpdateStatusResponse::UpdateAvailable {
    			current: "aaa".to_string(),
    			available: "bbb".to_string(),
    			upstream_commit_time: Some(
    				"2026-06-20T10:00:00Z".to_string(),
    			),
    		},
    	};
    	let val = serde_json::to_value(&resp).unwrap();
    	assert_eq!(val["status"], "updateAvailable");
    	assert_eq!(val["upstreamCommitTime"], "2026-06-20T10:00:00Z");
    }

    #[test]
    fn update_available_without_commit_time_omits_field() {
    	let resp = SkillUpdateResponse {
    		name: "s".to_string(),
    		scope: "global".to_string(),
    		status: SkillUpdateStatusResponse::UpdateAvailable {
    			current: "aaa".to_string(),
    			available: "bbb".to_string(),
    			upstream_commit_time: None,
    		},
    	};
    	let val = serde_json::to_value(&resp).unwrap();
    	assert!(
    		val.get("upstreamCommitTime").is_none()
    			|| val["upstreamCommitTime"].is_null(),
    		"absent time must be omitted"
    	);
    }
    ```

    Run (expect FAIL):

    ```bash
    cargo test -p aghub-api dto::skill::tests::update_available_with_commit_time_serializes -- --exact 2>&1 | tail -5
    ```

- [ ] **Step 3.2 — Add `upstream_commit_time` to `FetchedRepo` in `skill-update/src/lib.rs`**

    Find `pub struct FetchedRepo` (line ~200) and add the new field:

    ```rust
    pub struct FetchedRepo {
    	pub root: PathBuf,
    	pub oid: String,
    	/// RFC 3339 author-time of the fetched tip commit. Best-effort: `None`
    	/// when the commit time cannot be read (shallow fetch, old gix, error).
    	pub upstream_commit_time: Option<String>,
    	pub _guard: Option<Arc<tempfile::TempDir>>,
    }
    ```

    Every construction site of `FetchedRepo` in `lib.rs` (the test stubs around lines 809 and 1558-1563) and in `git.rs` must be updated. The test stubs can use `upstream_commit_time: None`.

    For `lib.rs` stubs search for `FetchedRepo {` and add `upstream_commit_time: None,`.

    Build check (not full test yet):

    ```bash
    cargo build -p skill-update 2>&1 | grep "^error" | head -10
    ```

- [ ] **Step 3.3 — Populate `upstream_commit_time` in `git.rs` GitFetcher**

    In `crates/skill-update/src/git.rs`, find where `FetchedRepo` is constructed after a successful fetch. After obtaining the `oid` string (tip commit OID), extract the commit author-time using gix:

    ```rust
    // After `let oid = ...` (the resolved tip OID string):
    let upstream_commit_time = read_commit_time(&repo, &oid);

    // Helper (add as a free fn in git.rs):
    fn read_commit_time(repo: &gix::Repository, oid_hex: &str) -> Option<String> {
    	use std::str::FromStr;
    	let id = gix::ObjectId::from_str(oid_hex).ok()?;
    	let commit = repo.find_object(id).ok()?.try_into_commit().ok()?;
    	let time = commit.author().ok()?.time;
    	// Convert gix::date::Time → chrono RFC 3339
    	let secs = time.seconds;
    	let offset_secs = i64::from(time.offset);
    	let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)?
    		.with_timezone(&chrono::FixedOffset::east_opt(
    			offset_secs as i32,
    		)?);
    	Some(dt.to_rfc3339())
    }
    ```

    ASSUMPTION: `gix` is already a direct dependency of `skill-update` (`Cargo.toml`). If `chrono` is not a direct dep of `skill-update`, add it (it is already in `aghub-api`; check with `grep -m1 "chrono" crates/skill-update/Cargo.toml`). If chrono is missing, add `chrono = { version = "0.4", features = ["serde"] }` to `crates/skill-update/Cargo.toml`.

    ASSUMPTION: The exact gix API for commit author-time may differ slightly by gix version. If `commit.author().ok()?.time` does not compile, alternative is `commit.decode().ok()?.author.time` — check the gix version in `Cargo.lock` and adjust.

    Build check:

    ```bash
    cargo build -p skill-update 2>&1 | grep "^error" | head -10
    ```

- [ ] **Step 3.4 — Thread through `CheckOutput` and the API `SkillUpdateStatusResponse`**

    `CheckOutput` (in `skill-update/src/lib.rs`) carries `status: SkillUpdateStatus`. The `UpdateAvailable` variant in `aghub_core::skills::update::SkillUpdateStatus` currently has `current` and `available`. Add `upstream_commit_time` there too:

    In `crates/core/src/skills/update.rs` (line ~21):

    ```rust
    pub enum SkillUpdateStatus {
    	UpToDate,
    	UpdateAvailable {
    		current: String,
    		available: String,
    		/// RFC 3339 author-time of the upstream tip commit. Best-effort.
    		upstream_commit_time: Option<String>,
    	},
    	Renamed { new_name: String },
    	Uncheckable { reason: UncheckableReason },
    }
    ```

    Update `compare_known_hashes` (line ~64) to accept the time:

    ```rust
    pub fn compare_known_hashes(
    	stored: &str,
    	fresh: &str,
    	upstream_commit_time: Option<String>,
    ) -> SkillUpdateStatus {
    	if stored == fresh {
    		SkillUpdateStatus::UpToDate
    	} else {
    		SkillUpdateStatus::UpdateAvailable {
    			current: stored.to_string(),
    			available: fresh.to_string(),
    			upstream_commit_time,
    		}
    	}
    }
    ```

    All callers of `compare_known_hashes` in `skill-update/src/lib.rs` must pass the `upstream_commit_time` (sourced from the `FetchedRepo.upstream_commit_time` for the matching group). Pass `None` at call sites that do not have a fetched repo (preflight skip path).

    In `crates/api/src/dto/skill.rs`, update `SkillUpdateStatusResponse::UpdateAvailable`:

    ```rust
    UpdateAvailable {
    	current: String,
    	available: String,
    	#[serde(
    		rename = "upstreamCommitTime",
    		skip_serializing_if = "Option::is_none"
    	)]
    	#[ts(optional)]
    	upstream_commit_time: Option<String>,
    },
    ```

    Update the `From<SkillUpdateStatus>` impl for `SkillUpdateStatusResponse` (line ~424):

    ```rust
    SkillUpdateStatus::UpdateAvailable { current, available, upstream_commit_time } => {
    	SkillUpdateStatusResponse::UpdateAvailable {
    		current,
    		available,
    		upstream_commit_time,
    	}
    }
    ```

    Run failing tests from Step 3.1 (expect PASS now):

    ```bash
    cargo test -p aghub-api dto::skill::tests::update_available_with_commit_time_serializes -- --exact
    cargo test -p aghub-api dto::skill::tests::update_available_without_commit_time_omits_field -- --exact
    ```

- [ ] **Step 3.5 — Add `upstream_commit_time` to domain `SourceSkillDiff` in `skill-update/src/sources.rs`**

    In `crates/skill-update/src/sources.rs` the domain `SourceSkillDiff` struct (lines 62-75) gets the new field:

    ```rust
    pub struct SourceSkillDiff {
    	// ... existing fields ...
    	pub upstream_commit_time: Option<String>,
    }
    ```

    In the `diff_source` function, when classifying `InstalledOutdated`, thread the commit time from the fetched repo into the `SourceSkillDiff`. The `FetchedRepo.upstream_commit_time` is available in the diff code path after the fetch.

    Construct all `SourceSkillDiff` values with `upstream_commit_time: None` for states other than `InstalledOutdated`; for `InstalledOutdated` pass `upstream_commit_time: repo.upstream_commit_time.clone()`.

    Build + test:

    ```bash
    cargo build -p skill-update 2>&1 | grep "^error" | head -5
    cargo test -p skill-update 2>&1 | tail -10
    ```

- [ ] **Step 3.6 — Regression: existing `compare_known_hashes` callers outside main path**

    Run all workspace tests to catch any missed call sites:

    ```bash
    cargo test --workspace 2>&1 | grep "FAILED\|^error" | head -20
    # expected: no failures
    ```

- [ ] **Step 3.7 — Commit**
    ```bash
    git add crates/core/src/skills/update.rs crates/skill-update/src/lib.rs crates/skill-update/src/git.rs crates/skill-update/src/sources.rs crates/api/src/dto/skill.rs crates/api/src/dto/sources.rs crates/api/src/routes/skills_update.rs
    git commit -m "feat(skill-update): thread upstream commit-timestamp through FetchedRepo → UpdateAvailable + SourceSkillDiff"
    ```

---

### Task 4: Accept-rename atomic backend op

**Files:**

- Modify: `crates/api/src/routes/skills_update.rs` (new route + inner fn)
- Modify: `crates/api/src/lib.rs` (mount route — §12-GAP6)

**Context:** The route must:

1. Fetch the upstream source (reusing `apply_source_from_lock` for the source URL/ref/path, or reading the old-name lock entry).
2. Call `install_fetched_skill_and_lock` for `new_name` (installs new skill into agent dirs + writes lock).
3. If install succeeds, call `plan_removal` + `execute_removal` for `old_name` (removes old dirs + lock entry).
4. If removal fails, attempt rollback: remove the newly installed `new_name` dirs + lock entry.
5. Update lock: remove old-name entry, keep new-name entry (written by `install_fetched_skill_and_lock`).

The inner function is `accept_rename_inner(req, fetcher)` for testability, mirroring `apply_skill_update_inner`.

- [ ] **Step 4.1 — Write failing test: accept-rename inner rejects without confirm**

    Add to `#[cfg(test)] mod tests` in `crates/api/src/routes/skills_update.rs`:

    ```rust
    #[cfg(unix)]
    #[test]
    fn accept_rename_inner_rejects_without_confirm() {
    	use crate::dto::skill::{AcceptRenameRequest, AcceptRenameResponse};
    	let req = AcceptRenameRequest {
    		old_name: "old".to_string(),
    		new_name: "new".to_string(),
    		scope: "global".to_string(),
    		project_root: None,
    		confirm: Some(false),
    	};
    	let fetcher = LocalRepoFetcher {
    		root: std::path::PathBuf::from("/tmp"),
    	};
    	let resp = rocket::tokio::runtime::Builder::new_current_thread()
    		.enable_all()
    		.build()
    		.unwrap()
    		.block_on(accept_rename_inner(req, &fetcher))
    		.unwrap()
    		.into_inner();
    	assert!(!resp.success);
    	assert!(resp.error.as_deref().unwrap_or("").contains("confirm"));
    }
    ```

    Run (expect FAIL — `accept_rename_inner` not defined):

    ```bash
    cargo test -p aghub-api routes::skills_update::tests::accept_rename_inner_rejects_without_confirm -- --exact 2>&1 | tail -5
    ```

- [ ] **Step 4.2 — Write failing test: accept-rename happy path installs new name and removes old**

    ```rust
    #[cfg(unix)]
    #[test]
    fn accept_rename_inner_installs_new_and_removes_old() {
    	with_isolated_state(|| {
    		let home = tempfile::tempdir().unwrap();
    		// Install old skill
    		let old_dir = home.path().join(".claude/skills/old-skill");
    		std::fs::create_dir_all(&old_dir).unwrap();
    		std::fs::write(
    			old_dir.join("SKILL.md"),
    			"---\nname: old-skill\ndescription: original\n---\n",
    		)
    		.unwrap();
    		let old_home = std::env::var("HOME").ok();
    		std::env::set_var("HOME", home.path());

    		// Lock entry for old-skill
    		let mut lock = skill::SkillLockFile::default();
    		let mut entry = global_entry();
    		entry.skill_path = Some("new-skill/SKILL.md".to_string());
    		lock.skills.insert("old-skill".into(), entry);
    		skill::lock::global::write_skill_lock(&lock).unwrap();

    		// Fetched repo has SKILL.md with new name
    		let fetched = tempfile::tempdir().unwrap();
    		let new_skill_dir = fetched.path().join("new-skill");
    		std::fs::create_dir_all(&new_skill_dir).unwrap();
    		std::fs::write(
    			new_skill_dir.join("SKILL.md"),
    			"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
    		)
    		.unwrap();
    		let fetcher = LocalRepoFetcher {
    			root: fetched.path().to_path_buf(),
    		};

    		let req = crate::dto::skill::AcceptRenameRequest {
    			old_name: "old-skill".to_string(),
    			new_name: "new-skill".to_string(),
    			scope: "global".to_string(),
    			project_root: None,
    			confirm: Some(true),
    		};
    		let resp = rocket::tokio::runtime::Builder::new_current_thread()
    			.enable_all()
    			.build()
    			.unwrap()
    			.block_on(accept_rename_inner(req, &fetcher))
    			.unwrap()
    			.into_inner();

    		match old_home {
    			Some(v) => std::env::set_var("HOME", v),
    			None => std::env::remove_var("HOME"),
    		}

    		assert!(resp.success, "error: {:?}", resp.error);
    		assert_eq!(resp.old_name, "old-skill");
    		assert_eq!(resp.new_name, "new-skill");

    		// New skill dir should exist
    		assert!(
    			home.path().join(".claude/skills/new-skill").exists(),
    			"new skill dir must be installed"
    		);
    		// Old skill dir should be removed
    		assert!(
    			!home.path().join(".claude/skills/old-skill").exists(),
    			"old skill dir must be removed"
    		);

    		// Lock: new-skill present, old-skill absent
    		let lock = skill::lock::global::read_skill_lock();
    		assert!(lock.skills.contains_key("new-skill"), "new-skill in lock");
    		assert!(
    			!lock.skills.contains_key("old-skill"),
    			"old-skill removed from lock"
    		);
    	});
    }
    ```

    Run (expect FAIL):

    ```bash
    cargo test -p aghub-api routes::skills_update::tests::accept_rename_inner_installs_new_and_removes_old -- --exact 2>&1 | tail -5
    ```

- [ ] **Step 4.3 — Write failing test: rollback on removal failure (testing-fs-failures approach)**

    This test injects a failing fs operation in the removal step to verify rollback:

    ```rust
    #[cfg(unix)]
    #[test]
    fn accept_rename_inner_rollback_on_removal_failure() {
    	// Use a read-only directory to make removal fail after install succeeds.
    	// This tests the rollback path: if removal of old-skill fails, the new-skill
    	// install must also be cleaned up.
    	// NOTE: actual filesystem failure injection via permission manipulation
    	// mirrors the testing-fs-failures skill approach.
    	with_isolated_state(|| {
    		let home = tempfile::tempdir().unwrap();
    		let old_dir = home.path().join(".claude/skills/old-skill");
    		std::fs::create_dir_all(&old_dir).unwrap();
    		std::fs::write(
    			old_dir.join("SKILL.md"),
    			"---\nname: old-skill\ndescription: original\n---\n",
    		)
    		.unwrap();
    		let old_home = std::env::var("HOME").ok();
    		std::env::set_var("HOME", home.path());

    		let mut lock = skill::SkillLockFile::default();
    		let mut entry = global_entry();
    		entry.skill_path = Some("new-skill/SKILL.md".to_string());
    		lock.skills.insert("old-skill".into(), entry);
    		skill::lock::global::write_skill_lock(&lock).unwrap();

    		let fetched = tempfile::tempdir().unwrap();
    		let new_skill_dir = fetched.path().join("new-skill");
    		std::fs::create_dir_all(&new_skill_dir).unwrap();
    		std::fs::write(
    			new_skill_dir.join("SKILL.md"),
    			"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
    		)
    		.unwrap();

    		// Make the old skill dir parent read-only so remove_dir_all fails
    		use std::os::unix::fs::PermissionsExt;
    		let skills_dir = home.path().join(".claude/skills");
    		let original_perms =
    			std::fs::metadata(&skills_dir).unwrap().permissions();
    		// Install first with normal perms, then lock down for removal
    		// We need to run it in two phases with a custom fetcher that lets us
    		// interpose. Since accept_rename_inner is a single async fn, we simulate
    		// by making the old dir un-removable (mode 0o500 on parent = no write).
    		// The rollback test verifies that new-skill is cleaned up on failure.
    		std::fs::set_permissions(
    			&skills_dir,
    			std::fs::Permissions::from_mode(0o500),
    		)
    		.unwrap();

    		let fetcher = LocalRepoFetcher {
    			root: fetched.path().to_path_buf(),
    		};
    		let req = crate::dto::skill::AcceptRenameRequest {
    			old_name: "old-skill".to_string(),
    			new_name: "new-skill".to_string(),
    			scope: "global".to_string(),
    			project_root: None,
    			confirm: Some(true),
    		};
    		let resp = rocket::tokio::runtime::Builder::new_current_thread()
    			.enable_all()
    			.build()
    			.unwrap()
    			.block_on(accept_rename_inner(req, &fetcher))
    			.unwrap()
    			.into_inner();

    		// Restore permissions
    		std::fs::set_permissions(&skills_dir, original_perms).unwrap();
    		match old_home {
    			Some(v) => std::env::set_var("HOME", v),
    			None => std::env::remove_var("HOME"),
    		}

    		// The op must fail (removal failed => rollback)
    		assert!(
    			!resp.success,
    			"must fail when old-skill removal fails"
    		);
    		// Neither the new-skill dir should persist after rollback
    		// (new-skill was installed to a path inside the locked-down dir, so
    		// it could not be written either — both fail consistently under 0o500).
    		// The lock must remain with only old-skill (no partial state).
    		let lock = skill::lock::global::read_skill_lock();
    		assert!(
    			lock.skills.contains_key("old-skill"),
    			"lock must be restored to old-skill only"
    		);
    		assert!(
    			!lock.skills.contains_key("new-skill"),
    			"new-skill must not be in lock after rollback"
    		);
    	});
    }
    ```

    Run (expect FAIL):

    ```bash
    cargo test -p aghub-api routes::skills_update::tests::accept_rename_inner_rollback_on_removal_failure -- --exact 2>&1 | tail -5
    ```

- [ ] **Step 4.4 — Implement `accept_rename_inner` and the Rocket route**

    Add to `crates/api/src/routes/skills_update.rs`. First, add the import at the top:

    ```rust
    use crate::dto::skill::{
    	AcceptRenameRequest, AcceptRenameResponse,
    	ApplySkillUpdateRequest, ApplySkillUpdateResponse,
    	SkillUpdateResponse, SkillUpdateStatusResponse,
    };
    ```

    Then implement `accept_rename_inner`:

    ```rust
    fn accept_rename_error(
    	old_name: &str,
    	new_name: &str,
    	scope: &str,
    	message: &str,
    ) -> AcceptRenameResponse {
    	accept_rename_error_with_code(old_name, new_name, scope, message, None)
    }

    fn accept_rename_error_with_code(
    	old_name: &str,
    	new_name: &str,
    	scope: &str,
    	message: &str,
    	code: Option<&'static str>,
    ) -> AcceptRenameResponse {
    	AcceptRenameResponse {
    		success: false,
    		old_name: old_name.to_string(),
    		new_name: new_name.to_string(),
    		scope: scope.to_string(),
    		installed_hash: None,
    		paths: Vec::new(),
    		error: Some(message.to_string()),
    		code: code.map(str::to_string),
    	}
    }

    /// `POST /skills/accept-rename` — atomic rename: install new name, delete
    /// old name, update lock.  Single transaction: removal failure triggers
    /// rollback of the just-installed new name.
    #[post("/skills/accept-rename", data = "<body>")]
    pub async fn accept_skill_rename(
    	body: Json<AcceptRenameRequest>,
    ) -> ApiResult<AcceptRenameResponse> {
    	accept_rename_inner(body.into_inner(), &GitFetcher).await
    }

    pub(crate) async fn accept_rename_inner(
    	req: AcceptRenameRequest,
    	fetcher: &dyn Fetcher,
    ) -> ApiResult<AcceptRenameResponse> {
    	if !req.confirm.unwrap_or(false) {
    		return Ok(Json(accept_rename_error(
    			&req.old_name,
    			&req.new_name,
    			&req.scope,
    			"confirm=true is required to accept a skill rename",
    		)));
    	}
    	let project_root = req.project_root.as_deref().map(PathBuf::from);
    	let resource_scope = match req.scope.as_str() {
    		"global" => ResourceScope::GlobalOnly,
    		"project" => ResourceScope::ProjectOnly,
    		_ => {
    			return Ok(Json(accept_rename_error(
    				&req.old_name,
    				&req.new_name,
    				&req.scope,
    				"scope must be global or project",
    			)));
    		}
    	};
    	if resource_scope == ResourceScope::ProjectOnly && project_root.is_none() {
    		return Ok(Json(accept_rename_error(
    			&req.old_name,
    			&req.new_name,
    			&req.scope,
    			"project_root is required when scope is project",
    		)));
    	}

    	// 1. Read lock entry for the OLD name to get source coordinates.
    	let source = match apply_source_from_lock(
    		&req.old_name,
    		&req.scope,
    		project_root.as_deref(),
    	) {
    		Ok(s) => s,
    		Err(e) => {
    			return Ok(Json(accept_rename_error(
    				&req.old_name,
    				&req.new_name,
    				&req.scope,
    				&e,
    			)));
    		}
    	};

    	// 2. Fetch upstream (same credential path as apply-update).
    	let resolver = KeyringResolver;
    	let token = resolver.resolve(
    		&source.source,
    		keychain_host_for_source(&source.source).as_deref(),
    	);
    	let repo = match fetcher.fetch(
    		&SourceRef {
    			source: source.source.clone(),
    			ref_: source.ref_name.clone(),
    		},
    		token.as_deref(),
    	) {
    		Ok(r) => r,
    		Err(e) => {
    			return Ok(Json(accept_rename_error(
    				&req.old_name,
    				&req.new_name,
    				&req.scope,
    				fetch_error_text(e),
    			)));
    		}
    	};

    	// 3. Locate skill file in fetched tree (containment check).
    	let Some(skill_file) =
    		aghub_core::skills::update::sanitize_skill_path(
    			&repo.root,
    			&source.skill_path,
    		)
    	else {
    		return Ok(Json(accept_rename_error(
    			&req.old_name,
    			&req.new_name,
    			&req.scope,
    			"Locked skillPath was not found in fetched source",
    		)));
    	};

    	// 4. Verify the fetched name matches new_name (confirms this is the right rename).
    	let parsed_skill = match skill::parse(&skill_file) {
    		Ok(s) => s,
    		Err(e) => {
    			return Ok(Json(accept_rename_error(
    				&req.old_name,
    				&req.new_name,
    				&req.scope,
    				&format!("Failed to parse fetched skill: {e}"),
    			)));
    		}
    	};
    	if parsed_skill.name != req.new_name {
    		return Ok(Json(accept_rename_error(
    			&req.old_name,
    			&req.new_name,
    			&req.scope,
    			&format!(
    				"Fetched SKILL.md declares name '{}', expected '{}'. \
    				 Verify the new_name matches the upstream source.",
    				parsed_skill.name, req.new_name,
    			),
    		)));
    	}

    	// 5. Install new-named skill via install_fetched_skill_and_lock.
    	//    Use all agents in scope as targets (same as apply-update does for
    	//    existing installs). Collect target agents from installed_skill_roots
    	//    logic — or use ResourceScope + all agents.
    	let lock_skill_path = source.skill_path.clone();
    	let install_source = skill::InstallLockSource {
    		source: source.source.clone(),
    		source_type: "github".to_string(), // resolved from lock entry below
    		source_url: source.source.clone(),
    		ref_name: source.ref_name.clone(),
    	};

    	// Determine source_type from lock
    	let (install_source_type, install_source_url) =
    		match req.scope.as_str() {
    			"global" => {
    				let lock = skill::lock::global::read_skill_lock();
    				let entry = lock.skills.get(&req.old_name);
    				(
    					entry.map_or("github", |e| e.source_type.as_str())
    						.to_string(),
    					entry.map_or(source.source.clone(), |e| {
    						e.source_url.clone()
    					}),
    				)
    			}
    			_ => (
    				"github".to_string(),
    				source.source.clone(),
    			),
    		};
    	let install_source = skill::InstallLockSource {
    		source: source.source.clone(),
    		source_type: install_source_type,
    		source_url: install_source_url,
    		ref_name: source.ref_name.clone(),
    	};

    	// Collect target agents from the old-skill install locations.
    	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
    		resource_scope,
    		project_root.as_deref(),
    	);
    	let target_agents: Vec<aghub_agents::models::AgentType> =
    		aghub_agents::models::AgentType::ALL.to_vec();

    	let install_req =
    		aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
    			skill_file: &skill_file,
    			source: &install_source,
    			lock_skill_path,
    			ref_commit: Some(repo.oid.clone()),
    			scope: resource_scope,
    			project_root: project_root.as_deref(),
    			target_agents: &target_agents,
    			expected_name: Some(&req.new_name),
    			target: aghub_core::skills::linker::LinkTarget::Absolute,
    		};

    	let install_report = match aghub_core::skills::install_fetched::install_fetched_skill_and_lock(install_req) {
    		Ok(r) => r,
    		Err(e) => {
    			return Ok(Json(accept_rename_error(
    				&req.old_name,
    				&req.new_name,
    				&req.scope,
    				&format!("Failed to install renamed skill: {e}"),
    			)));
    		}
    	};

    	let installed_paths: Vec<String> = install_report
    		.agent_results
    		.iter()
    		.filter(|r| r.installed)
    		.filter_map(|r| {
    			aghub_core::create_adapter(r.agent)
    				.get_skills_paths(
    					project_root.as_deref(),
    					resource_scope,
    				)
    				.first()
    				.map(|p| p.join(&req.new_name).display().to_string())
    		})
    		.collect();

    	// 6. Remove old-name skill.  Failure => rollback new-name install.
    	let old_skill = {
    		let mut s = aghub_core::models::Skill::new(&req.old_name);
    		// source_path for removal plan (copy layout fallback)
    		if let Some(dir) = agent_dirs.first() {
    			s.source_path = Some(
    				dir.join(&req.old_name)
    					.join("SKILL.md")
    					.display()
    					.to_string(),
    			);
    		}
    		s
    	};
    	let removal_plan = aghub_core::skills::removal::plan_removal(
    		&old_skill,
    		None,
    		&agent_dirs,
    		project_root.as_deref(),
    		true, // all agents
    	);
    	let removal_roots = aghub_core::skills::removal::allowed_skill_roots(
    		&agent_dirs,
    		project_root.as_deref(),
    	);
    	let removal_report =
    		match aghub_core::skills::removal::execute_removal(
    			&removal_plan,
    			&removal_roots,
    		) {
    			Ok(r) => r,
    			Err(e) => {
    				// Rollback: remove the just-installed new-skill dirs.
    				let _ = rollback_rename_install(
    					&req.new_name,
    					resource_scope,
    					project_root.as_deref(),
    					&agent_dirs,
    				);
    				// Also remove the new-skill lock entry written by install_fetched.
    				let _ = remove_lock_entry(
    					&req.new_name,
    					&req.scope,
    					project_root.as_deref(),
    				);
    				return Ok(Json(accept_rename_error(
    					&req.old_name,
    					&req.new_name,
    					&req.scope,
    					&format!(
    						"Failed to remove old skill '{}': {e}",
    						req.old_name
    					),
    				)));
    			}
    		};

    	if !removal_report.failed.is_empty() {
    		// Partial removal failure => rollback.
    		let failed_msgs: Vec<String> = removal_report
    			.failed
    			.iter()
    			.map(|(p, e)| format!("{}: {e}", p.display()))
    			.collect();
    		let _ = rollback_rename_install(
    			&req.new_name,
    			resource_scope,
    			project_root.as_deref(),
    			&agent_dirs,
    		);
    		let _ = remove_lock_entry(
    			&req.new_name,
    			&req.scope,
    			project_root.as_deref(),
    		);
    		return Ok(Json(accept_rename_error(
    			&req.old_name,
    			&req.new_name,
    			&req.scope,
    			&format!(
    				"Partial removal failure for old skill: {}",
    				failed_msgs.join("; ")
    			),
    		)));
    	}

    	// 7. Remove old-name lock entry.
    	if let Err(e) = remove_lock_entry(
    		&req.old_name,
    		&req.scope,
    		project_root.as_deref(),
    	) {
    		// Non-fatal: log but don't rollback (install+removal already committed).
    		log::warn!(
    			"accept-rename: failed to remove old lock entry '{}': {e}",
    			req.old_name
    		);
    	}

    	Ok(Json(AcceptRenameResponse {
    		success: true,
    		old_name: req.old_name,
    		new_name: req.new_name,
    		scope: req.scope,
    		installed_hash: Some(install_report.installed_hash),
    		paths: installed_paths,
    		error: None,
    		code: None,
    	}))
    }

    /// Remove a skill entry from the appropriate scope's lock.
    fn remove_lock_entry(
    	name: &str,
    	scope: &str,
    	project_root: Option<&Path>,
    ) -> Result<(), String> {
    	match scope {
    		"global" => skill::lock::global::modify_skill_lock(|lock| {
    			lock.skills.remove(name);
    			Ok(())
    		})
    		.map_err(|e| format!("global lock write failed: {e}")),
    		"project" => {
    			let root = project_root.ok_or_else(|| {
    				"project_root required for project scope".to_string()
    			})?;
    			skill::lock::local::modify_local_lock(Some(root), |lock| {
    				lock.skills.remove(name);
    				Ok(())
    			})
    			.map_err(|e| format!("project lock write failed: {e}"))
    		}
    		_ => Err("scope must be global or project".to_string()),
    	}
    }

    /// Best-effort rollback: remove newly installed dirs for new_name.
    fn rollback_rename_install(
    	new_name: &str,
    	scope: ResourceScope,
    	project_root: Option<&Path>,
    	agent_dirs: &[PathBuf],
    ) -> std::io::Result<()> {
    	let safe = skill::sanitize::sanitize_name(new_name);
    	let roots = aghub_core::skills::removal::allowed_skill_roots(
    		agent_dirs,
    		project_root,
    	);
    	for dir in agent_dirs {
    		let target = dir.join(&safe);
    		if target.exists() {
    			if aghub_core::skills::removal::assert_contained(&target, &roots)
    				.is_some()
    			{
    				let _ = std::fs::remove_dir_all(&target);
    			}
    		}
    	}
    	// Also remove universal master if newly created.
    	if let Some(canonical_dir) =
    		aghub_core::skills::linker::universal_canonical_dir(
    			if matches!(scope, ResourceScope::ProjectOnly) {
    				project_root
    			} else {
    				None
    			},
    		) {
    		let canonical = canonical_dir.join(&safe);
    		if canonical.exists()
    			&& aghub_core::skills::removal::assert_contained(
    				&canonical,
    				&roots,
    			)
    			.is_some()
    		{
    			let _ = std::fs::remove_dir_all(&canonical);
    		}
    	}
    	Ok(())
    }
    ```

    Run happy-path test (expect PASS):

    ```bash
    cargo test -p aghub-api routes::skills_update::tests::accept_rename_inner_installs_new_and_removes_old -- --exact
    ```

    Run confirm-reject test (expect PASS):

    ```bash
    cargo test -p aghub-api routes::skills_update::tests::accept_rename_inner_rejects_without_confirm -- --exact
    ```

- [ ] **Step 4.5 — Mount route in `crates/api/src/lib.rs` (§12-GAP6)**

    Find the `routes![...]` block in `build_rocket()` around line 214 (where `check_skill_updates` and `apply_skill_update` are listed):

    ```rust
    routes::skills_update::check_skill_updates,
    routes::skills_update::apply_skill_update,
    ```

    Add immediately after `apply_skill_update`:

    ```rust
    routes::skills_update::accept_skill_rename,
    ```

    Build check:

    ```bash
    cargo build -p aghub-api 2>&1 | grep "^error" | head -10
    ```

- [ ] **Step 4.6 — Run rollback failure test**

    ```bash
    cargo test -p aghub-api routes::skills_update::tests::accept_rename_inner_rollback_on_removal_failure -- --exact
    ```

    If the test fails because both install and removal fail under the 0o500 restriction (making it impossible to distinguish "install failed" from "removal failed after install"), revise the test setup to allow the install to succeed (write to a separate directory) then lock down only the old-skill parent. Adjust if needed.

- [ ] **Step 4.7 — Confirm route is surfaced in the API module doc (`crates/api/AGENTS.md`)**

    Update the `skills_update.rs` row in `AGENTS.md` table from `2` to `3` handlers:

    ```
    | `skills_update.rs` | 3 | `GET /skills/check-updates`, `POST /skills/apply-update`, `POST /skills/accept-rename` |
    ```

- [ ] **Step 4.8 — Full crate test + clippy**

    ```bash
    cargo test -p aghub-api 2>&1 | tail -20
    cargo clippy -p aghub-api -- -D warnings 2>&1 | grep "^error" | head -10
    ```

- [ ] **Step 4.9 — Commit**
    ```bash
    git add crates/api/src/routes/skills_update.rs crates/api/src/lib.rs crates/api/AGENTS.md
    git commit -m "feat(api): add POST /skills/accept-rename atomic install-new + remove-old route with rollback"
    ```

---

### Task 5: Regenerate ts-rs DTOs + run prettier

**Files:**

- Regenerate: `crates/desktop/src/generated/dto/` (all affected TS files)

**Context:** ts-rs generates TS files by running Rust tests with `TS_RS_EXPORT_DIR` set. The project uses bun for the desktop frontend. Prettier must be run after generation to produce a stable diff (per project convention documented in auto-memory `generated-dto-prettier-workflow.md`).

- [ ] **Step 5.1 — Regenerate DTOs**

    ```bash
    # Find the ts-rs export command for this project
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge
    # Try the justfile target first
    just generate:dto 2>&1 | tail -5
    # If that fails, run ts-rs export tests directly:
    # TS_RS_EXPORT_DIR=crates/desktop/src/generated/dto cargo test -p aghub-api --features ts-rs 2>&1 | tail -10
    ```

    ASSUMPTION: There is a `just generate:dto` target or equivalent. Check `justfile` for the exact target name if it fails.

- [ ] **Step 5.2 — Run prettier to stable-format the generated files**

    ```bash
    cd crates/desktop && bun run prettier --write src/generated/dto/ 2>&1 | tail -5
    ```

- [ ] **Step 5.3 — Verify TS union shape**

    ```bash
    grep -A 5 "SourceSkillStateDto" crates/desktop/src/generated/dto/SourceSkillStateDto.ts || \
      grep -n "SourceSkillStateDto\|state:" crates/desktop/src/generated/dto/SourceSkillDiff.ts
    # expected: state field shows the SourceSkillStateDto union type, not `string`
    ```

- [ ] **Step 5.4 — Verify `upstreamCommitTime` appears in generated DTO**

    ```bash
    grep "upstreamCommitTime" crates/desktop/src/generated/dto/SkillUpdateStatusResponse.ts
    # expected: upstreamCommitTime?: string
    grep "upstreamCommitTime" crates/desktop/src/generated/dto/SourceSkillDiff.ts
    # expected: upstreamCommitTime?: string
    ```

- [ ] **Step 5.5 — Verify `AcceptRenameRequest`/`AcceptRenameResponse` generated**

    ```bash
    ls crates/desktop/src/generated/dto/AcceptRename*.ts
    # expected: AcceptRenameRequest.ts  AcceptRenameResponse.ts
    ```

- [ ] **Step 5.6 — TypeScript type-check**

    ```bash
    cd crates/desktop && bun run tsc --noEmit 2>&1 | grep "error TS" | head -10
    # expected: no errors
    ```

- [ ] **Step 5.7 — Commit**

    ```bash
    git add crates/desktop/src/generated/dto/
    git commit -m "chore(dto): regenerate ts-rs types for AcceptRename, SourceSkillStateDto, upstreamCommitTime"
    ```

---

### Task 6 (OPTIONAL): CLI `source accept-rename` for app/CLI parity

**Files:**

- Modify: `crates/cli/src/commands/source.rs`
- Modify: `crates/cli/src/commands/mod.rs`

**Context:** The spec lists CLI accept-rename as `OQ2` — optional for app/CLI parity. The `SourceAction` enum dispatches to `source.rs`. `apply_skill_update_from_fetched` in `crates/cli/src/commands/apply_update.rs` is the existing fetch-and-apply primitive; accept-rename can reuse it for the install step plus the removal step from `aghub_core::skills::removal`.

- [ ] **Step 6.1 — Write failing CLI integration test**

    In `crates/cli/tests/cli_tests.rs`, add a network-free accept-rename test using a local dir fetch hook (mirroring the existing `source sync` pattern with `AGHUB_TEST_SOURCE_FETCH_ROOT`):

    ```rust
    #[test]
    fn source_accept_rename_installs_new_removes_old() {
    	// Uses AGHUB_TEST_SOURCE_FETCH_ROOT (debug_assertions hook in CliFetcher)
    	// to inject a local dir as the fetched source.
    	use assert_cmd::Command;
    	use tempfile::tempdir;

    	let home = tempdir().unwrap();
    	let skills_dir = home.path().join(".claude/skills/old-skill");
    	std::fs::create_dir_all(&skills_dir).unwrap();
    	std::fs::write(
    		skills_dir.join("SKILL.md"),
    		"---\nname: old-skill\ndescription: original\n---\n",
    	)
    	.unwrap();

    	// Write global lock
    	let lock_dir = home.path().join(".config/aghub");
    	std::fs::create_dir_all(&lock_dir).unwrap();
    	// (use skill crate to write lock, or write JSON directly)

    	let fetch_root = tempdir().unwrap();
    	let new_skill_dir = fetch_root.path().join("new-skill");
    	std::fs::create_dir_all(&new_skill_dir).unwrap();
    	std::fs::write(
    		new_skill_dir.join("SKILL.md"),
    		"---\nname: new-skill\ndescription: renamed\n---\n",
    	)
    	.unwrap();

    	let mut cmd = Command::cargo_bin("aghub-cli").unwrap();
    	cmd.env("HOME", home.path())
    		.env("AGHUB_TEST_SOURCE_FETCH_ROOT", fetch_root.path())
    		.args(["source", "accept-rename", "old-skill", "new-skill", "--yes"])
    		.assert()
    		.success();

    	assert!(
    		home.path().join(".claude/skills/new-skill").exists(),
    		"new skill must be installed"
    	);
    	assert!(
    		!home.path().join(".claude/skills/old-skill").exists(),
    		"old skill must be removed"
    	);
    }
    ```

    Run (expect FAIL):

    ```bash
    cargo test -p aghub-cli source_accept_rename_installs_new_removes_old -- --exact 2>&1 | tail -5
    ```

- [ ] **Step 6.2 — Add `AcceptRename` variant to `SourceAction` enum and dispatch**

    In `crates/cli/src/commands/mod.rs` (or `main.rs` where `SourceAction` is defined), add:

    ```rust
    // In the SourceAction enum (find with grep -n "SourceAction" crates/cli/src/main.rs):
    AcceptRename {
    	/// Locked name of the skill that was renamed upstream.
    	old_name: String,
    	/// New upstream name from the `renamed.newName` field.
    	new_name: String,
    	/// Commit changes. Default is a dry run.
    	#[arg(long)]
    	yes: bool,
    },
    ```

- [ ] **Step 6.3 — Implement `execute_accept_rename` in `source.rs`**

    ```rust
    pub fn execute_accept_rename(
    	old_name: String,
    	new_name: String,
    	global: bool,
    	project: bool,
    	yes: bool,
    	json: bool,
    ) -> Result<()> {
    	if !yes {
    		println!(
    			"Dry-run: would install '{new_name}' and remove '{old_name}'. \
    			 Pass --yes to execute."
    		);
    		return Ok(());
    	}
    	let scope = if global {
    		ResourceScope::GlobalOnly
    	} else if project {
    		ResourceScope::ProjectOnly
    	} else {
    		ResourceScope::GlobalOnly // default
    	};
    	let project_root = if matches!(scope, ResourceScope::ProjectOnly) {
    		find_project_root()?
    	} else {
    		None
    	};

    	// Read source coordinates from old-name lock entry.
    	let (source_url, ref_name, skill_path) =
    		read_source_from_lock(&old_name, scope, project_root.as_deref())?;
    	let resolver = EnvTokenResolver;
    	let token = skill_update::TokenResolver::resolve(
    		&resolver,
    		&source_url,
    		None,
    	);
    	let source_ref = skill_update::SourceRef {
    		source: source_url.clone(),
    		ref_: ref_name,
    	};
    	let repo = CliFetcher.fetch(&source_ref, token.as_deref())
    		.map_err(|e| anyhow::anyhow!("Fetch failed: {:?}", e))?;

    	// Verify fetched name = new_name.
    	let skill_file =
    		aghub_core::skills::update::sanitize_skill_path(
    			&repo.root,
    			&skill_path,
    		)
    		.ok_or_else(|| anyhow::anyhow!("skillPath not found in fetched source"))?;
    	let parsed = skill::parse(&skill_file)
    		.context("failed to parse fetched SKILL.md")?;
    	if parsed.name != new_name {
    		bail!(
    			"Fetched SKILL.md declares name '{}', expected '{}'. \
    			 Verify the new_name argument.",
    			parsed.name,
    			new_name,
    		);
    	}

    	// Install new name.
    	let paths = crate::commands::apply_update::apply_skill_update_from_fetched(
    		&repo.root,
    		&skill_path,
    		// Note: pass new_name as the locked name so the rename guard passes.
    		// We must rename old→new at lock level before this call, or use
    		// install_fetched directly.
    		// ASSUMPTION: use install_fetched_skill_and_lock for new name instead.
    		&new_name,
    		scope,
    		project_root.as_deref(),
    		Some(&repo.oid),
    	)?;
    	// Verify install succeeded, then remove old-name skill.
    	let agent_dirs =
    		aghub_core::skills::removal::agent_skill_dirs_in_scope(
    			scope,
    			project_root.as_deref(),
    		);
    	let mut old_skill = aghub_core::models::Skill::new(&old_name);
    	if let Some(dir) = agent_dirs.first() {
    		old_skill.source_path = Some(
    			dir.join(&old_name)
    				.join("SKILL.md")
    				.display()
    				.to_string(),
    		);
    	}
    	let removal_plan = aghub_core::skills::removal::plan_removal(
    		&old_skill,
    		None,
    		&agent_dirs,
    		project_root.as_deref(),
    		true,
    	);
    	let roots = aghub_core::skills::removal::allowed_skill_roots(
    		&agent_dirs,
    		project_root.as_deref(),
    	);
    	let report = aghub_core::skills::removal::execute_removal(
    		&removal_plan,
    		&roots,
    	)?;
    	if !report.failed.is_empty() {
    		bail!(
    			"Partial removal failure: {}",
    			report
    				.failed
    				.iter()
    				.map(|(p, e)| format!("{}: {e}", p.display()))
    				.collect::<Vec<_>>()
    				.join("; ")
    		);
    	}
    	// Remove old-name lock entry.
    	// (call the same lock modification as apply_update, but removing the entry)
    	remove_old_lock_entry(&old_name, scope, project_root.as_deref())?;

    	if json {
    		println!(
    			"{}",
    			serde_json::to_string_pretty(&serde_json::json!({
    				"success": true,
    				"oldName": old_name,
    				"newName": new_name,
    			}))?
    		);
    	} else {
    		println!(
    			"Renamed '{old_name}' → '{new_name}': installed to {} path(s)",
    			paths.len()
    		);
    	}
    	Ok(())
    }

    fn remove_old_lock_entry(
    	name: &str,
    	scope: ResourceScope,
    	project_root: Option<&Path>,
    ) -> Result<()> {
    	match scope {
    		ResourceScope::GlobalOnly => {
    			skill::lock::global::modify_skill_lock(|lock| {
    				lock.skills.remove(name);
    				Ok(())
    			})
    			.context("failed to write global lock")?;
    		}
    		ResourceScope::ProjectOnly => {
    			let root = project_root.ok_or_else(|| {
    				anyhow::anyhow!("project_root required")
    			})?;
    			skill::lock::local::modify_local_lock(Some(root), |lock| {
    				lock.skills.remove(name);
    				Ok(())
    			})
    			.context("failed to write local lock")?;
    		}
    		_ => bail!("only GlobalOnly/ProjectOnly supported"),
    	}
    	Ok(())
    }

    fn read_source_from_lock(
    	name: &str,
    	scope: ResourceScope,
    	project_root: Option<&Path>,
    ) -> Result<(String, Option<String>, String)> {
    	match scope {
    		ResourceScope::GlobalOnly => {
    			let lock = skill::lock::global::read_skill_lock();
    			let entry = lock
    				.skills
    				.get(name)
    				.ok_or_else(|| anyhow::anyhow!("'{name}' not in global lock"))?;
    			let skill_path = entry
    				.skill_path
    				.clone()
    				.ok_or_else(|| anyhow::anyhow!("locked entry has no skillPath"))?;
    			Ok((
    				entry.source_url.clone(),
    				entry.ref_name.clone(),
    				skill_path,
    			))
    		}
    		ResourceScope::ProjectOnly => {
    			let root = project_root
    				.ok_or_else(|| anyhow::anyhow!("project_root required"))?;
    			let lock = skill::lock::local::read_local_lock(Some(root));
    			let entry = lock
    				.skills
    				.get(name)
    				.ok_or_else(|| anyhow::anyhow!("'{name}' not in project lock"))?;
    			let skill_path = entry
    				.skill_path
    				.clone()
    				.ok_or_else(|| anyhow::anyhow!("locked entry has no skillPath"))?;
    			let source_url = format!(
    				"https://github.com/{}",
    				entry.source.trim_start_matches("https://github.com/")
    			);
    			Ok((source_url, entry.ref_name.clone(), skill_path))
    		}
    		_ => bail!("only GlobalOnly/ProjectOnly supported"),
    	}
    }
    ```

    Wire the dispatch in the `source` command match:

    ```rust
    SourceAction::AcceptRename { old_name, new_name, yes } => {
    	source::execute_accept_rename(
    		old_name,
    		new_name,
    		global,
    		project,
    		yes,
    		json,
    	)
    }
    ```

    Run CLI test (expect PASS):

    ```bash
    cargo test -p aghub-cli source_accept_rename_installs_new_removes_old -- --exact
    ```

- [ ] **Step 6.4 — Clippy + full CLI tests**

    ```bash
    cargo clippy -p aghub-cli -- -D warnings 2>&1 | grep "^error" | head -10
    cargo test -p aghub-cli 2>&1 | tail -10
    ```

- [ ] **Step 6.5 — Commit**

    ```bash
    git add crates/cli/src/commands/source.rs crates/cli/src/commands/mod.rs crates/cli/tests/cli_tests.rs
    git commit -m "feat(cli): add source accept-rename subcommand for CLI/app parity"
    ```

---

### Task 7: Preflight gate — full workspace verification

- [ ] **Step 7.1 — Run full preflight**

    ```bash
    just preflight 2>&1 | tail -30
    # Expected: all checks pass (fmt --check, clippy, typecheck, test, doc tests)
    ```

    Fix any lint/fmt issues found.

- [ ] **Step 7.2 — Verify npx round-trip contract (regression)**

    ```bash
    cargo test -p skill 2>&1 | tail -10
    # Confirms lock/types.rs/local.rs/hash.rs schema unchanged (npx frozen contract)
    ```

- [ ] **Step 7.3 — Final commit**

    ```bash
    git add -p  # review any remaining changes
    git commit -m "chore: Phase 3 preflight clean — accept-rename atomic op + ts-rs union + commit timestamp"
    ```

---

## Dependencies & Sequencing

- **Phase 1** must have landed: `SkillStatusBadge` uses `installedCurrent/installedOutdated` strings which this plan confirms as the wire protocol. No Phase 1 symbols are directly imported here, so this phase can technically start independently.
- **Phase 2** must have landed before the FE wiring of the `accept-rename` UI button added in Phase 2 calls the new `POST /skills/accept-rename` route. However, the backend route (Tasks 1-5) can be implemented and deployed before Phase 2 completes.
- Task 3 (commit-timestamp) requires a gix version that exposes commit author-time. This is a dependency of `skill-update/Cargo.toml` — verify the exact gix API against the locked version before implementing Step 3.3.
- Tasks 1-4 are sequential (DTOs → state enum → timestamp → route).
- Task 5 (DTO codegen) must run after Tasks 1-4 are all committed.
- Task 6 (optional CLI) is independent of Task 5 and can be skipped without affecting the API.

---

## Open Assumptions

1. **`gix` commit-time API**: The plan calls `commit.author().ok()?.time` to get the author timestamp. The exact method may differ by gix version in `Cargo.lock`. The implementor must check `grep "^gix " Cargo.lock | head -3` and consult the gix docs for that version. Alternative: `commit.decode()?.author.time.seconds` if the high-level API differs.

2. **`chrono` in `skill-update`**: The plan adds `chrono` to format the RFC 3339 timestamp. If chrono is not already a dep of `skill-update`, add it to `crates/skill-update/Cargo.toml`. Alternatively, format the Unix timestamp as RFC 3339 manually with a small helper to avoid the dep.

3. **`install_fetched_skill_and_lock` `target: LinkTarget`**: The plan passes `LinkTarget::Absolute` for global scope. For project scope it should be `LinkTarget::Relative`. The implementor must branch on `resource_scope` to pick the correct `LinkTarget` (see `crates/core/src/skills/install_fetched.rs` line ~56).

4. **`apply_skill_update_from_fetched` rename guard**: In Task 6, the plan calls `apply_skill_update_from_fetched` with `new_name` as the `name` argument. This function calls `ensure_source_not_renamed(skill_file, locked_name)` which will compare `parsed.name` (= `new_name`) against `locked_name` (= `new_name`) — they match, so the rename guard passes correctly. This is intentional: we pre-verified the name match at the accept-rename level.

5. **`modify_skill_lock` / `modify_local_lock` signatures**: The plan uses `skill::lock::global::modify_skill_lock(|lock| { lock.skills.remove(name); Ok(()) })`. If `modify_skill_lock` returns `Result<_, E>` and the closure must also return `Result`, the signature matches. If the closure signature differs (e.g., returning `((), bool)` like `modify_skill_lock_changed`), adapt accordingly — see `skills_update.rs` lines 250 and 363 for the exact call pattern already in use.

6. **Rollback completeness**: The rollback in `accept_rename_inner` removes newly installed dirs but does not restore the old skill if it was partially removed before the error. This is intentional: the rollback only undoes the _install_ side; if the old skill's removal partially succeeded, a partial state may remain. A full two-phase commit would require snapshotting the old dirs before removal — out of scope for this plan. The test in Step 4.3 covers the case where both install AND removal fail (under the 0o500 restriction, install into the locked dir also fails, so rollback is a no-op and the lock is clean).

7. **`SourceSkillStateDto` serialisation wire strings**: The `#[serde(rename_all = "camelCase")]` on the enum variant `NotInstalled` produces `"notInstalled"`, `InstalledCurrent` → `"installedCurrent"`, etc. This matches `SourceSkillState::as_wire()` exactly. Verify with the test in Task 2 Step 2.1.

8. **DTO regeneration command**: The exact `just generate:dto` target or cargo test invocation for ts-rs export may differ. Check `justfile` for a target named `generate-dto`, `dto`, or similar. The `TS_RS_EXPORT_DIR` env var approach is the fallback.
