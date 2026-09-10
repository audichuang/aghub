# 兩台機器的遷移收尾狀態（2026-09-10）

## ubuntuvm

| 範圍                               | 結果                                                                                             |
| ---------------------------------- | ------------------------------------------------------------------------------------------------ |
| global                             | 40 個 lock 條目 → `health: ok` 40/40、`linkAudit: verified` 40/40、`--fail-on-issues` **exit 0** |
| `$HOME` project                    | 11 個 `migrated`；仍 exit 1，因 **9 個我動手前就壞的** `orphan-lock`（見下）                     |
| leetcode-video / podcast-lab / AWS | 3 / 3 / 1 個 migrated，三個 doctor 全綠                                                          |
| aghub-wt/auto-deploy               | 遷移後**完整還原**（B6 事故）；`git status --porcelain` 空                                       |
| `~/.aghub`                         | 51 個 master（40 global + 11 來自 `$HOME` project）；`.quarantine` 5.2M                          |

## 本機

| 範圍                                            | 結果                               |
| ----------------------------------------------- | ---------------------------------- |
| global                                          | 42/42 綠（動手前就已遷移完成）     |
| AWS / podcast-lab                               | 1 / 3 個 migrated，doctor 綠       |
| `research/aghub`（22 排程 / 40 追蹤檔）         | **拒絕遷移** —— B6                 |
| `research/leetcode-video`（3 排程 / 73 追蹤檔） | **拒絕遷移** —— B6，3 個全是追蹤檔 |
| caveman                                         | 本來就乾淨                         |

## 等使用者決定的四件事

1. **`nas-container` 安裝**：`accept-rename` 永遠會被稽核擋（B1 無 override）。
   可行是兩步：`source sync audichuang/audi-skill --skill nas-container --install-missing
-a all --yes --force-unsafe`，成功後再 `delete skills synology-container -a <逗號清單> --yes`。
   代價是失去 rename 的原子性。**中途失敗會怎樣**：兩步是兩個獨立交易，
   而順序是「先裝新的、再刪舊的」，所以不存在「舊的沒了、新的沒裝上」的空窗——
   第一步失敗＝什麼都沒變（`synology-container` 還在）；第一步成功、第二步失敗＝
   兩支並存（重複，不是遺失），第二步可以直接重跑。
   兩步都**沒有實測過**（在等你決定要不要繞過稽核閘門），
   我只能確定順序上的安全性，不能保證第一步一定裝得起來。
2. **VM `$HOME` 的 9 個 `mattpocock/skills` 殘留 lock**（內容早就不在）：
   `caveman`、`review`、`to-issues`、`to-prd`、`write-a-skill` 上游還活著；
   `design-an-interface`、`qa`、`request-refactor-plan`、`ubiquitous-language` 上游已在
   `skills/deprecated/`。選項：全部還原 / 全部 prune / 還原 5 個 + prune 4 個。
3. **兩份 lock 都沒收的孤兒真目錄**（D5 明文不動）：
   VM `agent-reach`、`alpha`、`browser-use`、`browser-use-cli`、`self-distill`；
   本機 `agent-reach`、`alpha`、`browser-use`。`alpha` 的 description 是「a test skill」。
   要納管就得先把 agent 目錄裡的副本挪開再 `add --from`。
4. **垃圾**：VM 的 `~/.claude/skills/adversarial-generation-workspace/` **沒有 SKILL.md**，
   裡面是 85KB 的 `run_loop.log` 與 `trigger-eval.json`（`ForeignDir` 形狀，agent 掃到也讀不出東西）。

## 給接手修 bug 的 agent

`findings.md` 六條，B6 是最重要的（已造成實害並還原，附還原配方與掃描結果）。
B1 與 B6 都有明確的修改位置；B2 是行為變更要先確認意圖；B4／B5 是低風險小修。

## Quarantine 已驗（不是「計畫說一樣」而是逐檔比對）

兩台共 102 份 `.aghub/.quarantine/<name>/<stamp>/` 條目，對現行 master 做逐檔 sha256：
VM 58/58 完全相同；本機 44 份有 43 份相同，唯一差異是 `skillgenie` 的
`scripts/__pycache__/*.pyc`（quarantine 是 09-06、master 09-08 跑過腳本才產生）。
**沒有任何人寫的內容遺失**，所以 quarantine 可以放心清，但清之前不必再驗一次。

兩台的 `$HOME` 都**不是** git repo（`git rev-parse --git-dir` 失敗），
所以 `$HOME` project 那次遷移雖然把 referrer 建進 `~/.claude/skills`、`~/.agents/skills`，
並沒有構成第二次 B6。
