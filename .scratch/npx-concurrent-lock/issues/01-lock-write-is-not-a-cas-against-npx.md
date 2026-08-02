# 01 — aghub 的 lock 讀改寫對 npx 不是 CAS

**Status:** open

**Severity:** P1 — 靜默狀態損毀，但需要 aghub 與 `npx skills` 在毫秒級窗口內併發

## 問題

`modify_skill_lock` / `modify_skill_lock_changed`（`crates/skill/src/lock/io.rs`）
是 read → modify → write 三步。interprocess mutation guard 只序列化 aghub 對
aghub —— 這件事 `crates/skill/src/lock/guard.rs` 的模組註解本來就寫明了：

> Scope: this serializes aghub against aghub only. `npx skills` takes no lock of
> ours, so a concurrent `npx skills` run is still unserialized.

所以任何 aghub 寫 lock 的路徑，在它的 live-read 與 persist 之間，都可能被
`npx skills` 的寫入插隊，然後被 aghub 的記憶體快照整份覆蓋。

## 具體時序（以 check-updates 的 auto-heal 為例）

1. 磁碟與 global lock 都是 tree A；npx entry 只有 `skillFolderHash=T_A`，
   `contentHash` 未知
2. aghub 快照 lock A、掃磁碟得 hash A、fetch 上游得 B。因 hash 未知，check
   產生 `heal_hash=A`
3. `npx skills update` 把磁碟更新成 B
4. aghub 的 writer 讀 live lock —— 仍是 A（npx 還沒寫 lock），前置條件通過
5. npx 寫入 lock B
6. aghub persist 步驟 4 的記憶體快照，覆蓋 B；`apply_content_hash` 同時清空
   `skillFolderHash`

結果：磁碟是 B，lock 說 `contentHash=A`、`skillFolderHash=""`。npx 之後會因為
空的 `skillFolderHash` 直接跳過更新檢查，該 skill 在兩邊都不再更新。

## 為什麼不在 v2.10.2 修

- **不是那批修復引入的**：這是所有 `modify_*_lock` 路徑共有的既有性質
  （install / apply-update / prune / rename 全都一樣），不是 check-updates 特有
- v2.10.2 的 `HealPrecondition` + 讀取順序修正**大幅縮小**了這個窗口
  （從「完全不比對」變成四欄位前置條件 + 正確的讀取順序），只是沒有消滅它
- 消滅它要在 `modify_skill_lock_changed` 這個所有 lock 寫入的共用底層做 CAS，
  發版當下改那裡的風險大於它修的東西
- 上游 npx 自己寫 lock 也是直接 `writeFile`（無 temp+rename，見
  `src/skill-lock.ts` / `src/local-lock.ts`），所以連 npx 對 npx 都不保證原子。
  aghub 單方面無法完全解決

## 可能的做法

- **檔案版本 CAS**：`modify_*_lock` 讀取時記下檔案的 mtime+size 或內容 hash，
  persist 前重讀比對，不同就重跑 closure。需要 closure 冪等 —— 要逐一檢查現有
  呼叫端，這是這個 ticket 的主要工作量
- **temp + rename**：讓 aghub 自己的寫入原子化。不解決覆蓋問題（仍是 last
  writer wins），但至少不會留下半寫的 lock
- **縮小窗口**：heal writer 在 persist 前重讀一次並比對。窗口從
  read→persist 縮到 re-read→persist，仍非零

## 附帶（P2，non-blocking）

`crates/cli/src/commands/check.rs` 仍是舊順序（先掃 hash、後讀 lock），同一個
併發可能讓它把 hash A 配上 lock B、回報一次錯誤的更新狀態。CLI 不 heal、不寫
lock，所以只是顯示錯誤而非資料損毀 —— 但為了一致性應該一起改成先讀 lock。

## 出處

codex 對 v2.10.2 那批修復的第四輪對抗性複查（2026-08-02）。前三輪找到的
六 + 四 + 一項都已修進 `53e2aa9b`…`bfdabcd7`。
