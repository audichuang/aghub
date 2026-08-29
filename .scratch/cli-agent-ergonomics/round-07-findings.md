# 第七輪 — 三個 lens 的多 agent 自檢

跑在 `3b8147b3`。三個獨立 lens：loader 錯誤語意 / `load_failed` 完整性 / 回歸波及面。
兩條有衝突的由我親自重現裁決。

## 統一的病因

前六輪把「讀不到 ≠ 沒有」修在**目錄這一層**（`read_dir`、per-entry `stat`），
沒有修**檔案這一層**（`read_to_string`、parse 內部的 stat），也沒有修
**旁邊做同一個判斷的兄弟函式**（`removal.rs` 的兩個 sweep、codex 自己的 loader）。

## Blocking（全部已重現）

### 1. `crates/core/src/skills/discovery.rs:90` — CRITICAL

`Err(_) => collect_skills(&path, skills)?` 把 `parse_skill_dir` 的**任何**失敗
都當成「這不是 skill 目錄，往下遞迴」。`SKILL.md` 讀不到時 `parse_skill_dir`
回 `SkillError::Io(EACCES)`，遞迴進去只看到檔案，回 `Ok` —— 於是
`load_failed` 保持 false，`skill_holders` 把真持有者算成非持有者。

兩個 lens 各自獨立找到。其中一次的重現直接毀掉一個**沒被指名**的 agent 的
整個 skill 目錄（copy layout，兩次跑只差 gemini SKILL.md 的權限位）。

**既有**（`main` 上就是 `Err(_) => collect_skills(&path, skills)`），但這個
release 的賣點之一就是「讀不到的 agent 不再被當成沒有」——這行不修，那句話是假的。

修法：`SkillError::Io` 與 `MissingSkillMd` 是分得開的變體，只有後者該遞迴。

### 2. `crates/agents/src/sub_agents.rs:37` + `:114` — CRITICAL（我親自重現）

`fs::read_to_string(path).ok()?`；`is_regular_file` 把 stat 錯誤映成 `false`。
讀不到的 sub-agent 檔案被回報成不存在，於是 transfer 的「已存在？」檢查放行，
直接覆蓋。

```
CONTROL (0644): exit 1  "Resource already exists: sub_agent 'foo'"   原檔完好
FAULT   (0000): exit 0  success:true  already_present:false  stderr 空
                claude 的 foo.md 內容變成 GROK BODY —— 原本的沒了
```

`fe0db092` 修的就是這個函式的 `symlink_metadata`/`read_dir`/`entry?`，
停在下面那一層。**既有**。

### 3. `crates/core/src/skills/removal.rs:449` + `linker/mod.rs:303` — HIGH（我親自重現）

`dir_has_external_referrer` 靠 `Linker::is_link`，它對 EACCES 回 `false`；
`canonicalize(...).unwrap_or(false)` 再吞一次。

```
claude 擁有實體目錄 ~/.claude/skills/demo，gemini symlink 指過去
CONTROL (gemini 目錄 0755): paths [] / skipped [claude/skills/demo] / 目錄還在 / 連結完好
FAULT   (gemini 目錄 0400): paths [claude/skills/demo] / outcome "removed"
                            claude 實體目錄被刪 / gemini 連結變斷鏈 / exit 0 / stderr 空
```

**既有**。可達性前提：copy layout + 跨 agent symlink，現在的 aghub 只裝 symlink，
所以這個磁碟形狀來自舊版 copy 安裝、`npx skills`、或手工佈局——正是這個函式
存在的理由（它自己有 junction 測試）。

### 4. `crates/core/src/skills/removal.rs:295` — HIGH

`let Ok(_meta) = std::fs::symlink_metadata(&entry) else { continue; };`
把 stat 不到的 agent 目錄整個踢出 referrer sweep：`delete --all-agents` 既不
把它算成持有者、也不解它的 referrer，回報的 `paths` 靜默少一筆。**既有**。

master 目前逃過一劫是**巧合不是設計**：`~/.agents/skills` 自己也在
`all_agent_dirs`（cursor `agents/cursor.rs:36`、opencode `opencode.rs:108`
直接讀它），所以 `other_refs` 恆為 true。這個結構性保護沒有任何文件寫。

### 5. `crates/core/src/adapter.rs:152-166` — HIGH（回歸）

`load_config` 把 mcps / skills / sub-agents 綁成全有全無。讀不動的
`~/.claude/skills` 現在會擋掉該 agent 的全部 MCP 讀寫；讀不動的
`~/.claude/agents` 會擋掉 `get skills`。

歸因要誠實：**這個全有全無結構是既有的**（main 上 `load_mcps(...)?` 已經是
這樣），分支做的是把觸發條件從 1 個變成 3 個。三分之二算這個分支的。

## 其他（未全部重現）

- `crates/agents/src/agents/codex/sub_agent.rs:129` — codex 自己的 sub-agent
  loader 還是 `fe0db092` 之前的形狀（`let Ok(entries) = ... else { return Vec::new() }`
    - `.flatten()` + `.ok()?`）。寫入路徑會重讀原檔並拒絕，所以不是資料遺失。
- `crates/agents/src/errors.rs:7` — `IO error: {0}`，不帶路徑。使用者被告知
  MCP 操作失敗 errno 13，而出事的目錄他根本沒問。API 端同一串走 HTTP 500。
- `crates/core/src/skills/rename.rs:308` — `accept_rename` 的 `target_agents`
  不看 `load_failed`；Step 8 用 `all_agents=true` 依名字掃。未重現（需要 lock
  entry + 真的 fetch）。
- `crates/core/src/skills/prune.rs:449` — `entry.path().join("SKILL.md").is_file()`
  是這個否則相當 fail-closed 的掃描器裡唯一沒守的 stat。機制半證。
- 兩個 loader 對「路徑是檔案不是目錄」給相反答案：skills 硬失敗、sub-agents 回空。
- 符號連結自我迴圈（ELOOP）現在會炸掉該 agent 的所有指令。
- `load_failed` 沒有進任何 DTO；desktop 走 `agents/all/*`，靜默少列。
- `crates/agents/src/descriptor.rs:265` — `get_universal_skills_path` 不過濾空的
  `XDG_CONFIG_HOME`，會得到相對路徑 `agents/skills`（對 cwd 解析）。

## 兩個 lens 之間的分歧，以及誰對

- sub-agent 覆蓋：lens B 說「沒有破壞性消費者」，查了 `save_scoped_sub_agents`
  和 `remove_sub_agent_planned`，**漏掉 transfer 的「已存在？」判斷讀的就是那份
  載入清單**。我親自重現，lens A 對。
- `dir_has_external_referrer`：lens B 重現失敗，因為它用的形狀是
  master-in-all_agent_dirs（`other_refs` 被迫為 true），不是 claude 實體目錄
    - gemini symlink。我親自重現，lens A 對。

教訓同一句：**重現不出來不等於不存在，只等於這個形狀沒中。**
