# 13 — 兩個 loader 對同一種檔案系統形狀給相反答案

**Status:** open · medium · 一致性債，非資料遺失

同一個 commit (`fe0db092`) 改的兩個 loader，對「路徑存在但不是目錄」的判斷相反：

- `crates/core/src/skills/discovery.rs` — ENOTDIR 是**錯誤**，該 agent 的所有
  指令都硬失敗
- `crates/agents/src/sub_agents.rs:206-213` — 不是目錄就回 `Ok(vec![])`，是**答案**

已驗證：`~/.claude/skills` 是檔案 → `get mcps` exit 1；
`~/.claude/agents` 是檔案 → `get skills` exit 0。

相關：

- **ELOOP**：skills 目錄裡放一個自我指向的 symlink（`ln -s loop loop`）現在讓
  `fs::metadata` 回 os error 40，該 agent 所有指令硬失敗。舊的 `path.is_dir()`
  會跳過它。aghub 自己管理這些 symlink，所以一次失敗的 relink 就能造出這個形狀。
  per-entry 的 ELOOP 比較接近「這一筆不是 skill」而不是「這個目錄讀不到」。
- **`load_failed` 沒有進任何 DTO**：`crates/core/src/all_agents.rs:25` 的旗標在
  CLI 與 API 的 DTO 都找不到。`get skills -a all --json` 回 `[]` exit 0，
  `GET /agents/all/skills` 回 200 短清單，只有 `log::warn!` 進 stderr。
  desktop 的 `src/requests/{skills,mcps,sub-agents}.ts` 全部走 `agents/all/*`，
  所以 UI 是**靜默少列**，看不出有 agent 被跳過。
- **同一路由族兩種答案**：`?scope=global` 回 500（`manager.load()`），
  `?scope=all` 回 200 `[]`（`load_both_annotated` 仍然吞掉 per-scope 錯誤）。
- `crates/agents/src/descriptor.rs:265` `get_universal_skills_path` 不過濾**空的**
  `XDG_CONFIG_HOME`（kimi 的 `KIMI_SHARE_DIR` 有濾），會得到相對路徑
  `agents/skills`，對 cwd 解析。amp/kimi 仍從 descriptor 讀到正確絕對路徑，
  所以不會漏持有者；風險是靜默讀到 cwd 底下不相干的 `./agents/skills`。
