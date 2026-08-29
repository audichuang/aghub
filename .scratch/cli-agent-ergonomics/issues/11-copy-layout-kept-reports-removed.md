# 11 — copy layout 因外部 referrer 保留目錄時，`outcome` 仍回報 `removed`

**Status:** open · medium · 刻意延後（修法會擴張一個語意更窄的欄位）

`crates/core/src/dto/removal.rs:96` 的 `RemovalKind::Kept` 條件是
`shared_master_kept && paths.is_empty()`。`plan_copy_removal`
(`crates/core/src/skills/removal.rs:409-417`) 有兩支「保留」分支：

- `is_universal_master(&root, ..)` → 設 `shared_master_kept = true`
- `dir_has_external_referrer(&root, ..)` → **只 push 進 `skipped`，不設旗標**

於是 copy layout 下一個因為別的 agent 還有活連結而被保留的目錄，
回報是 `outcome: "removed"` / `paths: []` / `success: true`，東西還在磁碟上。

```
claude 擁有實體目錄，gemini symlink 指過去
delete skills demo -a claude --yes
  → paths [] / skipped [.claude/skills/demo] / outcome "removed" / 目錄還在
```

**既有**（round 7 的 CONTROL 就是這個結果；那一輪的修法只是讓不可讀的
FAULT 也走到同一支）。

**為什麼不順手改**：一行就能設旗標，但 `shared_master_kept` 的文件語意是
「解析到共用 universal Master 所以拒絕」，比這個情況窄；而且
`crates/core/src/manager/skill.rs:786` 和 `crates/core/src/skills/removal.rs:592`
拿它決定要不要跳過 lock prune、`crates/core/src/transfer.rs:1686` 拿它做
match guard。擴張它等於在三個決策點改語意，AGENTS.md 對這件事有明文警告
（把私有流程升成公開接縫要重新確認前提）。

誤導的是字串；`paths: []` 和 `skipped: [...]` 已經把真相寫在旁邊。
正解是給 copy 路徑自己的「因外部 referrer 保留」狀態，而不是借用 master 那個。
