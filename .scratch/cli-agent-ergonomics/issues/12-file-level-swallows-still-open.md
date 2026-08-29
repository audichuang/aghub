# 12 — 檔案層的 `.ok()` 還有幾處沒收（round 7 掃到但未重現）

**Status:** open · medium/low · 已定位，未端到端重現

round 7 修了四處會造成資料遺失的檔案層吞錯。同一個掃描還點到這些，
都因為「沒重現出破壞性後果」而留下：

- `crates/agents/src/agents/codex/sub_agent.rs:129` — codex 自己的 sub-agent
  loader 還是 `fe0db092` 之前的形狀：`let Ok(entries) = fs::read_dir(dir)
else { return Vec::new() }`、`.flatten()`、`.ok()?`。**不是資料遺失**：
  codex 的寫入路徑會重讀原檔（`read_original`/`validate_existing_file`）
  並以 `IO_ERROR` 拒絕，已驗證（transfer 進 codex 覆蓋 chmod-000 的
  `foo.toml` → exit 1，原檔保留）。
- `crates/core/src/skills/prune.rs:449` — `entry.path().join("SKILL.md").is_file()`
  是這個否則相當 fail-closed 的掃描器裡唯一沒守的 stat。0400 目錄下 readdir
  成功（d_type）而子層 stat 失敗 → 已裝的 skill 看起來不存在 → prune-lock
  可能砍掉活的 lock entry。機制半證，端到端要 git-sourced 安裝。
- `crates/core/src/skills/rename.rs:308` — `accept_rename` 的 `target_agents`
  用 `r.skills.iter().any(..)` 過濾，不看 `load_failed`；Step 8
  (`rename.rs:591`) 用 `plan_removal(.., all_agents=true)` 依名字掃。
  讀不到的持有者拿不到新名字卻被掃掉舊名字。未重現（需要 lock entry + 真 fetch）。
- `crates/core/src/skills/removal.rs:106` `installed_skill_roots_in` 不看
  `load_failed`：apply-update / check-updates 會靜默跳過讀不到的 agent。fail-safe。
- `crates/skill/src/parser.rs:159` `scan_subdir` 的 `if let Ok` + `.flatten()`：
  skill 的 `scripts`/`references`/`assets` 清單會靜默截斷。不影響存在性判斷。
- `crates/core/src/transfer.rs:333` `has_unhashed_entries` 其他地方都 fail closed，
  只有 `for entry in entries.flatten()` 這行會跳過中途的 readdir 錯誤。
- `crates/core/src/skills/discovery.rs` — `Linker::is_link` 成功但
  `canonicalize` 失敗時 `canonical_path` 留 None，會把 symlink 安裝**誤判成
  Copy layout**，讓後續刪除走依名字的全 agent 掃描而不是 canonical 比對。
  一個讀取失敗因此擴大了刪除的波及面。未重現。

## round 8 刻意換來的一個洞（accepted risk，不是「這樣就好」）

round 7 讓 `SkillError::Io` 一律傳播,round 8 把 `InvalidData` 和
`IsADirectory` 收回去照舊遞迴 —— 因為前者讓一個 latin-1 位元組就 exit-1 掉
該 agent 的每一個指令,連刪掉那個壞檔都做不到。

**但收回去的代價要寫清楚**:一個 `SKILL.md` 不是 UTF-8 的持有者,對
`transfer::skill_holders` 一樣是隱形的 —— 跟 round 7 修掉的 EACCES 情況
**完全同一個 sweep 曝險**,只是觸發的 errno 不同。copy layout 下就是那個
「毀掉沒被指名的 agent 的 skill 目錄」重現,把 EACCES 換成 InvalidData。

換句話說:我們用「一個壞檔不會癱瘓整個 agent」換掉了「一個壞檔的持有者不會
隱形」。在這一版這個取捨是對的（前者是每天都可能踩到的可用性,後者要
copy layout + 剛好在 reconcile 的路徑上），但它**是風險不是解決**。

正解需要第三種答案:`load_config` 要能表達「這裡有東西但我解不開」,讓守衛
把它算成持有者、讓列表把它標成 invalid,而不是在「傳播」與「當成不存在」
之間二選一。這是設計變更,不是發版前夕的補丁。

`doctor` 目前**看得到**這個狀態（health `invalid-skill`),所以使用者不是
完全沒有工具 —— 只是守衛沒讀它。
