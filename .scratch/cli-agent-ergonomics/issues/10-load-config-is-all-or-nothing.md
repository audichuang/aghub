# 10 — 讀一個資源失敗會炸掉另外兩個

**Status:** open · high · 刻意延後到 v2.15.0 之後

`crates/core/src/adapter.rs:145-166` 的 `load_config` 把 mcps / skills /
sub-agents 綁成一次全有全無的讀取，三個都是 `?`。所有走
`crates/cli/src/main.rs:1410` `run_for_agent` 的指令都繼承這個行為：
`get`、`describe`、`add`、`update`、`delete`、`enable`、`disable`。
`main.rs:1425-1441` 的 tolerate-missing 逃生口只認 `ErrorKind::NotFound`。

```
chmod 000 ~/.claude/skills   → get mcps / add mcps / enable mcps 全部 exit 1
chmod 000 ~/.claude/agents   → get skills exit 1
```

**歸因**：全有全無是**既有結構**（`main` 上 `load_mcps(...)?` 已經是這樣，
壞掉的 `~/.claude.json` 一直都會害 `get skills` 掛掉）。v2.15.0 把觸發條件
從 1 個變成 3 個（skills、sub-agents），round 7 的檔案層修法又再擴一次
（讀不到的 `SKILL.md` 或 sub-agent `.md` 現在也會讓該 agent 的 `get mcps` 失敗）。

**為什麼不在這一版修**：這個 atomicity **就是 `load_failed` 能運作的原因**。
`load_all_agents` 的單 scope 分支靠 `manager.load()` 回 `Err` 來設
`load_failed`，而 `transfer::skill_holders` 的整個安全性質建立在它上面。
改成 per-resource 粒度要重新設計 `AgentConfig` 的失敗攜帶方式，在發版壓力下
動這個等於重開我們花六輪關上的那個機制。

方向是對的但要獨立做：讓失敗**按資源**攜帶，而不是中止整次讀取。

**在此之前的行為是刻意的**：fail loud、方向安全。走
`load_all_agents` 的指令（`check`、`doctor`、`coverage`、`skill-usage`、
`get -a all`）仍然 fail open，已驗證。
