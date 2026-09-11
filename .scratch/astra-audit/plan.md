# Skill / AGENTS.md 精簡執行計畫（Claude Code + Codex 雙引擎）

## 總結

最大的問題不是「skill 太多」，而是**截斷已經發生而且是靜默的**：在這個 repo 開一個 session，Claude 端 85 個可見 skill 裡有 48 個只剩名字、描述被整段丟掉（丟掉的合計 17,062 字元），其中包含 `upstream-skills-flow`、`plan-desktop-ui`、`npx-skills-contract`、`testing-fs-failures` 這四個 aghub 自己的領域 skill；同時 Codex 在這個 repo 看不到任何一個 aghub 領域 skill，它唯一看得到的 23 個全是前端設計動詞包，實測 0 次呼叫。

執行完這份計畫後（**前提：閘門一選 B、閘門二照建議**）：唯一 skill 名稱 **101 → 63**（背景資料說的 117 是含跨 root 的實體副本；`inv.json` 的唯一名稱是 101）；Claude 在 aghub repo 看到的描述量 **33,456 → 14,313 字元（−57%）**；Codex **23,768 → 13,150 字元（−45%）**，而且省下來的位置換成它本來就該有的 8 個 aghub 領域 skill；根 `AGENTS.md` **573 行 / 37,041 字元 → 約 300 行 / 約 18KB**，搬走的內容全部落到只有動該 crate 時才載入的 per-crate 檔；並補上這份文件現在完全沒有的兩樣東西——「這條路安全，放手做」的授權句與「做完」的定義。

**三個必須由你先裁決的閘門**（後面六個階段的一半內容取決於答案）：兩個在階段 0（設計動詞包、Cloudflare 家族），一個在階段 6（要不要把 `just preflight` 升進自主欄）。在你回答之前不要往下做。

**誠實標註**：Claude 端的字元節省是**上限不是實得**——被刪掉的那批多半描述早就被丟光了，刪掉不保證別人的描述會復活（丟棄規則實測與長度、字母序都無關）。Codex 端是截短不是丟棄，所以那一側的節省是確定的。階段 0 有一個最小可證偽實驗來決定要不要繼續用字元數當計分板。

---

## 階段 0：觀測、決策閘門、與零 Codex 影響的 plugin 清理

主題：先取得三份無法從檔案推斷的觀測，並讓你裁決兩個會決定後面一半工作的問題。**這一階段不刪任何 skill。**

### 0-A 三份觀測（做完才有基準）

| #   | 動作                                                                                                    | 為什麼                                                                                                                                                                                                                                                   | 受益   |
| --- | ------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| O1  | 在 **Codex 互動 session** 裡列一次可用 skill（`/skills` 或 `$`），把輸出存下來                          | 一份輸出同時回答三個問題：(a) `grilling`、`tdd`、`skillgenie` 這種同時存在於 `~/.codex/skills` 與 `~/.agents/skills` 的 42 個名字有沒有**出現兩次**；(b) Codex 認不認 `disable-model-invocation`（13 個帶旗標的 skill 在不在清單裡）；(c) 哪些描述被截短 | codex  |
| O2  | 在 aghub repo 開一個**互動** Claude session（不是 subagent），記下哪些 skill 只有名字沒有描述，存成基準 | 目前是 48 個。這是唯一可驗收的指標                                                                                                                                                                                                                       | claude |
| O3  | 階段 1 只刪 Cloudflare 那批之後，重跑 O2                                                                | **最小可證偽實驗**：如果「只剩名字」的數量沒有下降、也沒有新描述復活，就代表截斷機制不是純總量預算，後面所有階段的理由要從「省預算」改寫成「省 Codex 預算 + 減少誤選 + 停止腐爛」，並停止拿字元數當計分板                                                | both   |

O1 的結果直接 gate 三件事，在拿到之前不要做：`-a cline,warp` 的 referrer 清掃、`grill-me` / `grill-with-docs` 的刪除、以及所有基於 `disable-model-invocation` 的動作在 Codex 側的效果宣稱。

### 0-B 閘門一：21 支前端設計動詞包（proj-agents + proj-claude）

**事實**：`aghub-cli skill-usage` 與 32 個 transcript 都顯示這 19 個動詞 0 次 Skill 呼叫、0 次 slash；Codex 側 1,441 個 session 檔也 0 次命中。**但** `.impeccable.md` 此刻是 `M`（未 commit）、工作區有 15 個 `.tsx` 正在改——這個工作流是活的，只是沒走 Skill tool。它們佔 proj-agents 6,084 的 **100%**、proj-claude 10,762 的 **53%**。

**建議預設（選 B）**：

- **A（最便宜，立刻可做）**：19 個 `SKILL.md` 的 frontmatter 各加一行 `disable-model-invocation: true`。Claude 端已實測 13/13 有此旗標的 skill 完全退出模型清單（連名字都不列），所以立刻釋出 proj-claude 約 4,948 字元；slash 呼叫不受影響。這些檔是 git 追蹤的，就地改再 commit **本身就是 fork**，不需要先換 provenance；lock 條目只有 `source` + `computedHash`、沒有 `skillPath`/`ref`，`apply-update` 跑不動，真正要防的只有日後 `aghub-cli source sync -p`（在 `UPSTREAM.md` 記一筆即可）。Codex 端效果待 O1 確認。
- **B（推薦，真正兩邊都省）**：做漸進揭露——建 `.agents/skills/design-pass/SKILL.md` 當 router（描述約 150 字元），19 份 `SKILL.md` 降級成 `references/<verb>.md`，`.claude/skills` 下的 19 個 symlink 換成 `design-pass` 一個。**只有減少實際條目數，Codex 才確定省得到**（它是截短不是丟棄）。
- **C（只在你明說不用時）**：`aghub-cli delete skills <verb> -p --all-agents --yes`（實測會自動 prune lock），連同 `skills-lock.json` 變動一起 commit。

**無論選哪個，這三支排除在外**：`frontend-design`（router 正本 + 品味反模式清單是真資產）、`heroui-react`（repo 真的用 HeroUI v3）、`project-form-patterns`（repo 特有、描述已是文章的 Good 形狀）。`teach-impeccable` 跟著這個閘門走，不單獨處理。

**這個閘門同時決定**：`delight` 的觸發詞撞車、`audit`/`critique` 的假評分刻度、`MANDATORY PREPARATION` 的 16 份副本、`overdrive` 的瀏覽器支援度快照、兩支的 19 個指令名硬編清單、`critique` 的兩個死引用、以及階段 3 的動詞描述改寫——選 C 則全部自動失效，選 A/B 則階段 7 有一節專門處理。

### 0-C 閘門二：Cloudflare 家族（14 支，claude 與 codex 各一份實體副本）

我實測 `find ~/research -maxdepth 4 -name 'wrangler.*'`：**13 個 Workers 專案**，不是稽核初稿說的 3 個。所以：

| 處置             | 對象                                                                                                                                                                                      | 理由                                                                                                                                                                                                                                       |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **留在全域**     | `wrangler`(336)、`workers-best-practices`(359)                                                                                                                                            | 13 個 Workers 專案都可能用到，降級到 3 個 repo 會斷掉 10 個                                                                                                                                                                                |
| **留在全域**     | `web-perf`(453)                                                                                                                                                                           | 不是 Cloudflare skill，是 Chrome DevTools 的 Core Web Vitals 稽核，aghub 有 React/Tauri 前端                                                                                                                                               |
| **留、但縮描述** | `cloudflare-mcp-worker`(789→≤250)                                                                                                                                                         | 唯一被 aghub 納管的 CF skill（Master 在 `~/.aghub`），而且 honcho/mcp、booking_badminton_mcp 都是 MCP-on-Workers，是真的在走的架構                                                                                                         |
| **降級到專案層** | `agents-sdk`(431)                                                                                                                                                                         | 全機掃 import，只有 `honcho/mcp/src/index.ts` 一個消費者                                                                                                                                                                                   |
| **刪除（9 支）** | `cloudflare`、`cloudflare-one`、`cloudflare-one-migrations`、`turnstile-spin`、`cloudflare-email-service`、`durable-objects`、`sandbox-next`、`sandbox-stable`、`sandbox-migrate-to-next` | 擴大到全部 13 個 CF 專案重 grep，turnstile / Zero Trust / @cloudflare/sandbox 仍 0 命中；DurableObject 與 email 唯二命中在 `skillsgate/apps/web/cloudflare-env.d.ts`，該檔第 2 行寫「Generated by Wrangler」，是自動生成的型別全集不是使用 |

**還原路徑**（已逐一驗證）：12 個 CF skill × 2 個 root 全部與 `~/.skillshub/<n>` byte-identical，`cp -r ~/.skillshub/<name> ~/.claude/skills/` 一行、離線可做。Claude 端另有 fallback：把 `~/.claude/settings.json` 的 `cloudflare@claude-plugins-official` 設回 `true`，但只救得回 11 支、且三個 sandbox 變體會 collapse 成單一 `sandbox-sdk`。四個 skill root 都沒有 `.git`，無 dotfiles 版控風險。

### 0-D plugin 清理（Claude-only 最大槓桿，對 Codex 零影響）

這是整份計畫 CP 值最高的一塊，而且不需要任何閘門。

| 動作                            | 對象                                                                 | 字元                      | 理由                                                                                                                 | 風險                                                                                                                                                   |
| ------------------------------- | -------------------------------------------------------------------- | ------------------------- | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 全域 `false`                    | `claude-code-setup@claude-plugins-official`                          | 354                       | 0 次呼叫                                                                                                             | 需要時重開                                                                                                                                             |
| 全域 `false`                    | `andrej-karpathy-skills@karpathy-skills`                             | 219                       | 0 次呼叫，且「avoid overcomplication / surgical changes」與正在運作的 ponytail 完全同義，兩條互相搶同一次判斷        | 無                                                                                                                                                     |
| 全域 `false`                    | `hookify@claude-plugins-official`                                    | 395（5 條）               | 0 次呼叫，資料目錄 `~/.claude/plugins/data/hookify-claude-plugins-official` 是**空的**——裝了幾個月一條規則都沒建過   | 無（無規則可失效）。**不要**順手 rm 舊版本快取：實測 12 個目錄裡有兩個時間戳相同，刪錯就是刪掉現行版本，而且停用後快取本來就不進 context               |
| **專案層** `false`              | `ui-ux-pro-max@ui-ux-pro-max-skill`                                  | 2,798（7 條）             | 0 次呼叫；`ui-styling` 主打 shadcn/Radix，本 repo 是 HeroUI v3 + React Aria，技術棧相衝                              | 只在 aghub 關掉，其他 repo 照舊                                                                                                                        |
| **專案層** `false` 或改上游描述 | `imagine@agent-fleet`                                                | 980                       | `imagine-prompts` 878 字元是所有 plugin 中最長的描述，內容在講兩個 engine 的 prompt 差異——那是本文該講的事。0 次呼叫 | marketplace 是你自己的 `audichuang/agent-fleet-cc`，**優先**改上游描述成一行（約 50 字元）把 engine 差異移進 `references/`，功能留著、其他機器同步受益 |
| 檢視                            | `pyright-lsp`(Python)、`jdtls-lsp`(Java)                             | 0 描述                    | 對 Rust + TypeScript 的 aghub 完全無用，代價是每 session 起 LSP server 的記憶體與啟動時間                            | 若有別的 Java/Python repo 就留著                                                                                                                       |
| 記一筆                          | `code-simplifier` 的 `/simplify`                                     | —                         | 與個人 `code-review`(421，描述活著)、`ponytail:ponytail-review` 三方搶同一觸發點                                     | 先觀察，不動                                                                                                                                           |
| 記一筆                          | `agent-fleet` 的 `delegate` / `fleet`（已快取、不在 enabledPlugins） | `delegating-to-fleet` 671 | 待爆彈，誤開就是一筆大支出                                                                                           | 不動，只記錄                                                                                                                                           |

專案層做法：**優先用 `/home/audichuang/research/aghub/.claude/settings.local.json`**（per-user、不會被 commit）；`settings.json` 會進版控、會影響其他貢獻者，只有在你確定要讓所有人都關掉時才用它。實測這個 repo 目前兩個檔都沒有。**動手前先確認 `enabledPlugins` 在專案層真的被吃**——若只有全域吃得到，就退回全域停用。

`ponytail` 不動（825 字元的描述雖然肥，但它的 hook 已無條件注入、行為永遠開著；上游不是你的，改不了）。

---

## 階段 1：零風險刪除

主題：完全重複、測試殘留、錯的平台、確認無用。每一項都可還原。

| 目標                                                           | 動作                                                                                                                                                                                                                                                           | 受益   | 影響 | 風險                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| -------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ | ---- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `~/.agents/skills/alpha`                                       | `rm -rf /home/audichuang/.agents/skills/alpha`（51 bytes，description 是 `a test skill`，本文就一個字 `body`，不在任何 lock、不是 Master、無 referrer 指向它）                                                                                                 | codex  | 低   | 零。**由你親自執行**——AGENTS.md 權限表把真實 `~/.agents` 列在 Ask first                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `~/.claude/skills/verifying-inherited-root-causes`             | `rm -rf`（與 `validate-inherited-root-cause` 逐行 diff 只差一行 H1，實體目錄、不在任何 lock、不需要 aghub-cli）                                                                                                                                                | claude | 高   | **刪之前先抄走它的描述**：`awk '/^description:/{print;exit}' ~/.claude/skills/verifying-inherited-root-causes/SKILL.md`——那 248 字元的英文版寫得比留下來的 483 字元版好，階段 3 要用它                                                                                                                                                                                                                                                                                                                                                 |
| Cloudflare **9 支**（階段 0-C 決議中除 `agents-sdk` 外的全部） | `aghub-cli delete skills <name> -g --all-agents`（先看 preview 確認 outcome 不是 `kept`，再加 `--yes`）。**注意 `-a all` 會被 CLI 拒絕**，正確旗標是 `--all-agents`。`agents-sdk` 是降級不是刪除，走階段 2 的 `rm -rf` + `cp` 一種機制就好，不要在這裡也刪一次 | both   | 高   | 還原見 0-C                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `~/.claude/skills/mole-cleanup`（symlink）                     | `rm`（描述自承需 macOS + `mo`；實測 `uname -s`=Linux、`command -v mo` 無輸出）Master 留在 `~/.skillshub`                                                                                                                                                       | claude | 中   | `~/.claude` 無 `.git`，git-based dotfiles 同步已排除；非 git 的同步機制（Syncthing/rsync）未查，動手前確認一次                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `~/.claude/skills/openclaw-plugin-dev-skill`（symlink）        | `rm`（`~/github/openclaw` 最後 commit 2026-03-02，半年沒動）Master 留在 `~/.skillshub`                                                                                                                                                                         | claude | 中   | 回去做 plugin 時 `cp -r` 到該 repo 專案層                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `brainstorming`                                                | `rm -f ~/.claude/skills/brainstorming ~/.skillshub/brainstorming`（**只拆 symlink，不動 `.repos/` 裡的 clone**，否則下次更新會還原）                                                                                                                           | claude | 低   | frontmatter 是 `risk: unknown` / `source: community`，且第 22 行的「You are **not allowed** to implement, code, or modify behavior while this skill is active」正是文章第 5 點說會讓新模型提早停手的語氣。與 `grilling` 入口重疊。若捨不得 design-facilitator 視角，先濃縮成 `grilling/references/` 一個檔再拆連結                                                                                                                                                                                                                     |
| `writing-for-agents`                                           | `aghub-cli delete skills writing-for-agents --all-agents -g --yes`（source 是 `mattpocock/skills`，改描述會被 sync 蓋回，所以刪比改對）。刪前確認它 10.7KB 本文沒有 `agents-md-architecture` 缺的東西                                                          | both   | 中   | 它是「寫 CLAUDE.md」的第五個 claimant，同時與 `skillgenie` 搶「create skills」                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `skill-doctor`                                                 | `aghub-cli delete skills skill-doctor -g --all-agents --yes`；**跑完接 `aghub-cli doctor`，若報 `orphanMaster` 就手動 `rm -rf ~/.aghub/skill-doctor`**（preview 顯示 Master 落在 `skipped`，會被 `kept`）                                                      | both   | 中   | `skillgenie` 22 次 vs `skill-doctor` 1 次，而 skillgenie 的描述明文涵蓋 measure performance 與 optimize description。「從真實對話歷史評分」正是這次稽核在做的事，做完就不需要常駐                                                                                                                                                                                                                                                                                                                                                      |
| `.claude/skills/init-deep`                                     | `cd /home/audichuang/research/aghub && git rm -r .claude/skills/init-deep`                                                                                                                                                                                     | claude | 高   | 它第 129/132/244/245/246/249 行反覆要求 `ln -s AGENTS.md CLAUDE.md`、第 388 行把「沒有配對 CLAUDE.md」列為 Anti-Pattern，**與本 repo「every CLAUDE.md is a one-line `@AGENTS.md` import (a real file, not a symlink)」直接相反**——在這個 repo 跑它會反向改掉既有約定。它還寫死了 `task(subagent_type=...)`、`LspServers()`、`background_output(task_id=...)` 這些兩個引擎都不存在的呼叫語法。功能由全域 `agents-md-architecture` 分支 A/B 接手（兩個引擎都掛得到）。**刪除會進 git status**，與正在進行的 15 個 `.tsx` 修改分開 commit |

---

## 階段 2：合併重疊群集

主題：同一句話命中多支、或兩支互相解釋分工的，收斂成一支。

| 目標                                                          | 動作                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | 受益   | 影響         | 風險                                                                                                                                             |
| ------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ | ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `yt-dlp-capture` → `agent-reach`                              | 見階段 4-B 的 agent-reach 鏈（有嚴格順序）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | both   | 高           | —                                                                                                                                                |
| `agents-sdk` 降級                                             | `rm -rf ~/.claude/skills/agents-sdk ~/.codex/skills/agents-sdk` 後 `cp -r ~/.skillshub/agents-sdk /home/audichuang/research/honcho/.claude/skills/`（**放 repo root `/home/audichuang/research/honcho`，不是 `honcho/mcp` 子目錄**）                                                                                                                                                                                                                                                                                                          | both   | 中           | 只有一個消費者，已驗證                                                                                                                           |
| `notebooklm` / `synology-container` 降級                      | `aghub-cli delete skills notebooklm -a claude,codex,cline,warp -g`（先不加 `--yes` 看 preview，確認 outcome 是 `removed` 不是 `kept`）。**必須含 cline 與 warp**——`~/.agents/skills` 的全域寫入者是它們，而 Codex 同時讀 `.codex/skills` 與 `.agents/skills`，少了就等於 Codex 一字未省。**不要用 `-a all`**。落點：`notebooklm` → `~/research/audiskill/podcast-lab`（marker 齊全，已驗）；`synology-container` → `~/ops` **但該目錄零 project marker（沒有 .claude/、.git、.mcp.json），`-p` 必定 bail**，安裝前先 `mkdir -p ~/ops/.claude` | both   | 中           | `source sync --install-missing` 會不會把它們裝回全域未驗證，降級後觀察一次。**真實 `~/.agents` 由你執行**                                        |
| `orca-upstream-pr`                                            | **不要搬到 `~/research/orca/.claude/skills`**——實測 `git check-ignore -v .claude/skills` 回 `.gitignore:140`，且 `git ls-files` 為 0，linked worktree 看不到；而這支 skill 的全部用途就是在 worktree 裡跑。改為原地縮描述（階段 3）                                                                                                                                                                                                                                                                                                           | claude | 中           | —                                                                                                                                                |
| `research`                                                    | **不刪**。`ask-matt` 的路由圖第 81 行明確指派它。只把描述裡「Use when the user wants a topic researched」這半拿掉（那正是撞 `agent-reach` 的那句）。source 是 `mattpocock/skills`，改了會被 sync 蓋回——接受或不動                                                                                                                                                                                                                                                                                                                             | both   | 低           | —                                                                                                                                                |
| `grill-me` / `grill-with-docs`                                | **gate 在 O1**。本文各只有 39B / 67B（`Call the Skill tool with "grilling".`），13 支帶 `disable-model-invocation` 的在 Claude 端零成本。若 O1 顯示 Codex 吃帳 → `aghub-cli delete skills grill-me --all-agents -g --yes`；若不吃 → 整條不做。`ask-matt` 等 11 支有實質內容的一律保留                                                                                                                                                                                                                                                         | codex  | 低           | —                                                                                                                                                |
| `~/.codex/skills` 與 `~/.agents/skills` 的 42 支同名同 Master | **gate 在 O1**。若 Codex 雙列 → 逐支 `aghub-cli delete skills <name> -a cline,warp -g --yes` 拆掉 `.agents/skills` 那條 referrer（cline/warp 未安裝：`~/.cline`、`~/.clinerules`、`~/.warp` 皆不存在、`which` 全空），先拿一支試、跑 `doctor --verify-links -g` 確認 codex 仍 `linked` 再批次。**先排除 `agent-reach`（Codex 唯一取得路徑）與 `alpha`**                                                                                                                                                                                       | codex  | 高（若成立） | 拆之前務必確認 `.agents/skills` 的其他讀者（copilot / cursor / opencode / pi / grok / omp）各自都有私有 referrer。**真實 `~/.agents`，由你執行** |
| `redbook` / `spec-from-code` 鏡射到 Codex                     | `ln -s /home/linuxbrew/.linuxbrew/lib/node_modules/@lucasygu/redbook ~/.codex/skills/redbook`；`ln -s /home/audichuang/research/spec-from-code/skill/spec-from-code ~/.codex/skills/spec-from-code`                                                                                                                                                                                                                                                                                                                                           | codex  | 低           | 這會在 Codex 側製造 aghub 看不見的條目（doctor 不管也不判紅）。記進「已知管理盲區」，否則下次稽核又會當成新發現的孤兒                            |

---

## 階段 3：描述瘦身（逐支新描述全文在「要貼上的文字」）

主題：把「該放本文的東西」從描述裡拿出來。**先做描述還活著的那批**（改完立刻見效），已被丟掉描述的那批排後面。

### 3-A 正本歸屬（決定怎麼改，不是改不改）

| 類別                            | 對象                                                                                                                                                                                                                                                                     | 改法                                                                                                                                |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| **可直接改**（不在任何 lock）   | `~/.claude/skills/agent-reach`、`~/.agents/skills/agent-reach`、`~/.claude/skills/orca-upstream-pr`、`.claude/skills/{releasing-aghub,verify-desktop-ui,plan-desktop-ui,upstream-skills-flow,npx-skills-contract,aghub-skills}`、`~/.claude/skills/writing-claude-rules` | 直接編輯，零覆蓋風險                                                                                                                |
| **audi-skill 正本**             | `validate-inherited-root-cause`、`cloudflare-mcp-worker`、`hindsight-knowledge`、`hindsight-project-bank`、`remote-mcp-headless-auth`、`dual-host-plugin`、`worktree-cwd-guard`、`aghub-cli`、`skillgenie`、`notebooklm`                                                 | **一次 `git fetch` → 一次 commit → 一次 `aghub-cli source sync`**（MEMORY 已記 audi-skill 有平行 session，撞到要 merge 並逐項重驗） |
| **第三方 upstream**（推不上去） | `orchestration`/`orca-cli`（stablyai/orca）、`research`/`grill-*`（mattpocock）、`heroui-react`（heroui-inc）、21 動詞（pbakaus）                                                                                                                                        | 手改 = 永久 `update-available`，`apply-update --yes` 會蓋回。**決定去留，不要改寫**                                                 |
| `~/.skillshub`                  | `project-context-layout`、`ticktick-skill`、`funday-course`                                                                                                                                                                                                              | 實測 `command -v skillshub` rc=1——**沒有 CLI，是休眠殘骸**，就地改在重裝前都安全                                                    |

### 3-B 描述還活著、改完立刻見效（優先做）

| Skill                           | dlen       | 改成                                                                                                                                                                                                                                                                                            | 受益   |
| ------------------------------- | ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| `agent-reach`                   | 890 → ~160 | 見貼上文字（**兩份實體副本都要改，或收編後只改 Master**）                                                                                                                                                                                                                                       | both   |
| `orca-upstream-pr`              | 846 → 208  | 整條 flow（branch→測試→gate→Codex review→PR body→draft→CodeRabbit→ready）移進本文，只留中文觸發短語                                                                                                                                                                                             | claude |
| `project-context-layout`        | 830 → ~262 | 刪 `~300 lines` 門檻。**Spring Boot / multi-module Maven 那組觸發詞先問你**——你的信箱是 cathaybk，Java 工作 repo 極可能存在，不要因為 aghub 是 Rust 就逕刪                                                                                                                                      | claude |
| `hindsight-project-bank`        | 802 → 271  | `optInPaths` / `bankIdTemplate` / Grok hook replay 移進本文；刪掉把另外兩支觸發詞整批搬進來的那句 `Not for...`                                                                                                                                                                                  | both   |
| `releasing-aghub`               | 757 → 262  | `ALSO use for any aghub version/maintenance question` → 三個具名情境                                                                                                                                                                                                                            | claude |
| `remote-mcp-headless-auth`      | 744 → 243  | 四個 whenever 串接 + `Covers…` 功能列表移進本文，保留症狀字串                                                                                                                                                                                                                                   | both   |
| `worktree-cwd-guard`            | 555 → 164  | bullet 情境列舉移進本文，保留末句負向邊界                                                                                                                                                                                                                                                       | both   |
| `aghub-cli`                     | 560 → 259  | 刪 `(Claude Code, Codex, Cursor, and others)`                                                                                                                                                                                                                                                   | both   |
| `verify-desktop-ui`             | 559 → 231  | 方法論步驟移本文，中文觸發短語全留                                                                                                                                                                                                                                                              | claude |
| `hindsight-knowledge`           | 512 → 250  | 保留 `Does not replace hindsight-coding-agent`（近義雙生需要），刪第二句 `Not for plugin install, bank config...`                                                                                                                                                                               | both   |
| `writing-claude-rules`          | 508 → 165  | 「與 project-context-layout 互補：…」整段搬本文                                                                                                                                                                                                                                                 | claude |
| `skillgenie`                    | 497 → 245  | 兩個 Use when 併成一個                                                                                                                                                                                                                                                                          | both   |
| `validate-inherited-root-cause` | 483 → ~220 | 用 `verifying-inherited-root-causes` 那 248 字元英文版當骨幹，但**保留「修好了、過 review、過測試，但使用者說沒效果」這條獨立入口**（那是不同的觸發時機不是重複強調），並留一句「N 個 subagent 都同意 / 兩份分析一致」（handoff 文件真的會出現的字面語句，屬文章第 3 點豁免的「真實踩過的坑」） | both   |
| `ticktick-skill`                | 383 → 182  | `Supports tasks CRUD, subtasks, ...` 功能列表移本文                                                                                                                                                                                                                                             | claude |

### 3-C 描述已被丟掉、即時收益在 Codex（排後面）

| Skill                       | dlen       | 改成                                                                                                                                                                                                                                                   | 受益                 |
| --------------------------- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------- |
| `upstream-skills-flow`      | 987 → ~287 | 三條軸線裡最長的一支。**壓縮時必須把現有描述結尾那整段三方消歧義完整帶走**（`For the frozen round-trip contract use npx-skills-contract; for aghub-side invariants use aghub-skills`）——那是全清單寫得最好的消歧義之一，病因是 987 字元的長度不是矛盾  | claude→both          |
| `cloudflare-mcp-worker`     | 789 → 257  | 刪 house-pattern 強制觸發句與技術棧列舉；**保留 `Not for configuring an MCP client entry in an agent's config.`**——這句是本條最有價值的部分（aghub 一天到晚在處理 MCP 設定的解析與序列化）                                                             | both                 |
| `plan-desktop-ui`           | 629 → 232  | 方法論步驟與排除句移本文。順手處理它指向的 `spec-from-code`——那支只掛在 claude-global、對 Codex 是永遠斷掉的指標                                                                                                                                       | claude→both          |
| `dual-host-plugin`          | 575 → 157  | 7 種故障徵象的 bullet 移本文，保留最有辨識度的 3 個                                                                                                                                                                                                    | both                 |
| `npx-skills-contract`       | 461 → 258  | 給三者不相交的觸發面                                                                                                                                                                                                                                   | claude→both          |
| `aghub-skills`              | 418 → 228  | 同上                                                                                                                                                                                                                                                   | claude→both          |
| `yt-dlp-capture`            | 399        | **合併進 agent-reach 後刪除**（見階段 4-B）。若決定保留，至少把首句「Use when **Codex** needs to…」改成引擎中立——描述裡點名某個引擎等於對另一個引擎宣告「不適用」                                                                                      | both                 |
| `heroui-react`              | 315 → 172  | 刪 `Keywords: HeroUI, Hero UI, heroui, @heroui/react, @heroui/styles.`（同一產品名的 5 種拼法對語意模型完全無效）。**但它 source=`heroui-inc/heroui`，改完永久 update-available、此後不可對它跑 apply-update**——先確認要不要留                         | both                 |
| `openclaw-plugin-dev-skill` | 451 → 210  | 若階段 1 已刪 symlink 則只在回去做 plugin 時才需要。順手清掉描述裡 `before\_prompt\_build` 漏進 YAML 的 markdown 跳脫字元                                                                                                                              | claude               |
| 保留的 Cloudflare 系        | —          | 刪掉 `Biases towards retrieval from Cloudflare docs over pre-trained knowledge`（73 字元 × 10 份、`web-perf` 是 79「from current documentation」、`cloudflare-one` 是 90「Retrieval-first: ...」），移到各自 `SKILL.md` 本文首行。只對還保留的那幾支做 | both（即時在 Codex） |

### 3-D 把手動型 skill 標成 slash-only（比刪除更便宜的槓桿）

只對**純個人生活／外部工具類**且描述還活著的做：`notebooklm`(307)、`redbook`(69)、`ticktick-skill`(383)、`synology-container`(267)，合計約 1,026 字元，全部是實得。frontmatter 加 `disable-model-invocation: true`（改 `~/.skillshub` 或 `~/.aghub` 的 Master，referrer 自動生效）。

**明確排除**：`releasing-aghub` / `verify` / `verify-desktop-ui`——它們正是該自動觸發的 repo 專屬 skill，改成 slash-only 會回到「假驗收」那個坑；`funday-course` / `mole-cleanup` 描述已被丟光、加旗標省不到。Codex 側效果 gate 在 O1。

---

## 階段 4：雙引擎收斂

主題：把「兩份實體副本」變成「一份 Master + 兩條 Referrer」，並把 Codex 在這個 repo 缺的東西補上。

### 4-A 8 個 aghub 領域 skill 開放給 Codex（純增益、不犧牲任何東西）

**必須排在閘門一的動詞包縮減之後**——否則 Codex 專案層會從 6,084 直接跳到 11,141。

```bash
cd /home/audichuang/research/aghub
for n in verify releasing-aghub aghub-skills npx-skills-contract \
         upstream-skills-flow testing-fs-failures plan-desktop-ui verify-desktop-ui; do
  git mv .claude/skills/$n .agents/skills/$n
  ln -s ../../.agents/skills/$n .claude/skills/$n
  git add .claude/skills/$n
done
# 順手補上漏掛的那一條：project-form-patterns 只有 .agents/skills 實體、
# .claude/skills 底下沒有對應 symlink（其他 21 個都有），所以 Claude 在這個
# repo 從來沒看過它。若不是刻意的就補上：
ln -s ../../.agents/skills/project-form-patterns .claude/skills/project-form-patterns
git add .claude/skills/project-form-patterns
```

驗收：`ls -la .claude/skills | grep -c '^d'` 應為 0（全 symlink、無實體目錄）。

- `.claude/skills/verify-desktop-ui/SKILL.md:52` 有 `node .claude/skills/verify-desktop-ui/scripts/run-scenarios.mjs` 這個 repo 相對硬編路徑——靠「symlink 必須一起 commit」繼續解析得到，所以 **`git add .claude/skills/$n` 那步不可省**。
- **副作用**（必須知道）：`.agents/skills` 在專案 scope 不只 Codex 讀。`aghub-cli repair -p` 的拒絕訊息實測列出 `codex, gemini, cline, copilot, antigravity, kimi, amp, warp`——8 個 agent 都讀得到。你機器上 antigravity(agy) 是實際在用的，所以這些描述也會進 agy 的預算。這不是阻擋理由，但要知道這是給整個 roster 不是只給 Codex。
- **與 15 個 `.tsx` 修改分開 commit。**
- `init-deep` 不在這 8 個裡（階段 1 已刪）。

### 4-B agent-reach 收編 + 瘦身 + 合併 yt-dlp（嚴格順序）

```bash
BAK=/tmp/skill-converge-bak && mkdir -p $BAK/stage
# 1 stage（不要直接指 referrer 路徑，那正是要被取代的位置）
cp -a ~/.claude/skills/agent-reach $BAK/stage/agent-reach
# 2 先在 staged copy 裡把 description 改成 ~160 字元版（見貼上文字），
#   並把 yt-dlp-capture 的 cookie/bot-check/impersonation 階梯與 host preset 表
#   併進 $BAK/stage/agent-reach/references/video.md
#   （實測 video.md 4,219B 內 cookies-from-browser|impersonate|keyring|bot 命中 0，確是獨有內容）
# 3 清掉三個 root 的實體副本
mv ~/.claude/skills/agent-reach $BAK/claude-agent-reach 2>/dev/null
mv ~/.agents/skills/agent-reach $BAK/agents-agent-reach 2>/dev/null
# 4 收編成單一 Master
aghub-cli add skills --from $BAK/stage/agent-reach -n agent-reach -a claude,codex -g
# 5 驗收
readlink -f ~/.claude/skills/agent-reach ~/.codex/skills/agent-reach
# 6 確認綠了才刪 yt-dlp-capture
aghub-cli delete skills yt-dlp-capture --all-agents -g --yes
```

- **順序不可反**：先瘦身再收編，否則等於把全庫最長的一條描述（890）原封不動複製到第二個引擎。
- `--from` 匯入的 Master 沒有可 fetch 的 source，`check` 會**永遠**回報 `local`（永久性理由），等於「aghub 管得到但永遠不會自動更新」。仍值得做（15 份散裝副本的漂移風險更大），但要知道代價。
- `yt-dlp-capture` 的 source 是 audi-skill，刪完下次 `source sync` 可能裝回來——同一次 audi-skill commit 裡把它從上游移除，或接受。
- **真實 `~/.agents` 動作由你執行。**

### 4-C `browser-use` 與其餘散裝副本

`browser-use` 三個 root 全等、只有一個 `SKILL.md`、描述約 130 字元——收編報酬（<0.1M 磁碟、描述 0 變化）不值三個 root 的手術風險。**建議不動**；真要做就排到所有其他收斂都綠了之後。另注意 `~/.claude/skills/browser-use` 權限是 `drwx------`（其他 skill 都是 755），收編前先確認不是刻意鎖的——AGENTS.md 的 shape 章節明講權限問題會被 classify 成 `Failed`。

### 4-D `~/.skillshub` 退役（最後一步）

所有 skillshub 項目處理完後，驗收條件是 `readlink -f ~/.claude/skills/* | grep skillshub` **回空**，才執行：

```bash
mv ~/.skillshub ~/.skillshub.retired.$(date +%F)
```

觀察一兩週再刪。**注意 `~/.skillshub/brainstorming` 是活的 symlink 掛載點**，`mv` 會當場打斷它——所以階段 1 的 brainstorming 拆連結是這一步的硬前置。另外 skillshub 裡還有 `cloudflare-mcp-worker` 與 `remote-mcp-headless-auth` 的過期鏡像（aghub 早就在管，`~/.claude/skills` 下已是指向 `~/.aghub` 的 symlink），那兩份是沒人引用的孤兒。

### 4-E 兩個全域指令檔的對稱（最便宜的 dual-engine 修補）

`~/.codex/AGENTS.md` 整份只有一行 `@/home/audichuang/.codex/RTK.md` + CodeGraph 段，**個人偏好零條**。把 `~/.claude/CLAUDE.md` 的三條抄進去（見貼上文字），插在 `@RTK.md` 之後、`<!-- CODEGRAPH_START -->` **之前**——絕對不要寫進 CODEGRAPH 區塊（那是自動維護區）。

`~/.claude/CLAUDE.md` 第 6 行的 attribution 那條**縮短不刪除**（`~/.claude/settings.json:6-9` 實測 `"attribution": {"commit": "", "pr": ""}` 已強制，但刪掉整行會一併刪掉「為什麼這裡沒寫」的自我說明，日後容易被人再加回來）。

`~/.codex/RTK.md`：`rtk gain` 實測 **56,666 次指令、省 168.4M tokens（60.1%）**，是全 session 最大的單一省量機制——**不要**把「Always prefix shell commands with `rtk`」放寬成「預設」而不給替代機制。但 `rtk gain` 自己就印出 `[warn] No hook installed — run 'rtk init -g' for automatic token savings`：**有一個比散文更可靠的機制沒被採用**。建議（需你點頭，動的是全域 codex 設定）：跑 `rtk init -g` 裝 hook，由 hook 自動改寫指令，散文就不必扛絕對令。若不裝 hook，只刪 `## Verification` 整節（典型的「動手前先驗一輪」）與 `## Meta Commands` 裡給人看的 `rtk gain`，保留「預設加」的語氣與「rtk 不在 PATH 時直接跑原指令，不要為此停下來問」這個出口。**不要**把 rtk 規則複製到 Claude 側。

---

## 階段 5：AGENTS.md 重構

主題：根檔 573 行 / 37,041 字元是兩個引擎唯一真正共載、而且無條件全文載入的檔案（比兩邊所有 skill 描述加起來還大）。把只在動某個 crate 時才需要的內容搬下去。

### 5-0 硬約束（先知道，否則第一步就弄紅測試）

`crates/cli/tests/cli_tests.rs:5924-5953` 的 `cli_command_surface_block()` + `agents_md_command_surface_lists_phase7_subcommands` 會把根 `AGENTS.md` 的 `## CLI Command Surface` 切到下一個 `## ` 為止，並斷言該區塊含 `inference` / `transfer` / `reconcile` / `coverage` 四個字。**根檔的 stub 必須保留那個標題與四個字**。

### 5-1 CLI Command Surface（L161–381，16,334 字元）分四路搬

| 內容                                                                                                                                                                                                                                             | 行                                              | 去處                                                                                                                                                                                                                                                                                                               |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 標題 + 前四行（clap 權威、aliases、`-a`/`-g`/`-p`/`--all`）+ **英文指路段（含四個關鍵字）**                                                                                                                                                      | L161–169                                        | **留在根檔**                                                                                                                                                                                                                                                                                                       |
| destructive defaults、scope 互斥與 rootless `-p` bail、`skill-usage`、`coverage`、narrowed args、`inference`、`reconcile` 需至少一個 `--add`/`--remove`、**doctor 的 surface 語意**（`--fail-on-issues`、`linkAudit.state`、預設不改 exit code） | L170–182, 288–303, 362–364, **L183–202 的前半** | `crates/cli/AGENTS.md`（取代第 6–8 行那句 disclaim）。**L177–182 只帶 cli 檔缺的兩件事**：(a) clap ArgGroup 不傳播到 `global = true` args 所以是 exit 1 不是 exit 2；(b) 破壞性 `--yes` 預設清單。其餘已幾乎逐字在 cli/AGENTS.md L27–38（還多了釘住的測試名），不要搬。（L365–367 刪除、L368–377 進 core，見 5-2） |
| **doctor 的 per-agent 判決來源**（判決是 `classify_shape` 的、doctor 自己不推導；`chain`、`masterUnusable`、`master-is-symlink`）、repair 四道守門、repair 拒絕 git 追蹤、transfer/reconcile 的 `RemovalCredits`                                 | **L183–202 的後半**、203–248、304–353、358–361  | `crates/core/AGENTS.md`，**與該檔 L39 既有的 `read_effect_after` 段落合併去重**，不要變成第三份副本。壓縮時**必須留住 L222–226**（agent 永遠不會把自己記成自己 slot 的 reader；專案層 `.agents/skills` 是 amp 的寫槽而其他 reader 各有私槽）——那是「為何兩道守門不能合併」唯一推不出來的理由，只留兩個測試名不夠   |
| `check` offline 預設、`network` 理由歸 orchestrator、`source diff` 永遠 fetch、per-scope origin                                                                                                                                                  | L249–254, 275–280                               | `crates/skill-update/AGENTS.md`。**L255–257（scope 預設 BOTH）改搬 `crates/cli/AGENTS.md`**（那是 scope 表的事實）。**L258–265 整段刪除**——與 `crates/skill-update/AGENTS.md` L35–43「更新狀態的語意」是同一套規則的兩種語言版本，而 L265 自己就指向那個檔；根檔只留一句英文指路                                   |
| `GET /skills/sources/diff?scope=all` 的 `SOURCE_AMBIGUOUS` 分歧                                                                                                                                                                                  | L281–285                                        | `crates/api/AGENTS.md`（實測該檔 grep 不到 `SOURCE_AMBIGUOUS`，是真空缺）。兩邊各留一句互指                                                                                                                                                                                                                        |
| `delete mcps <name> -a claude -p --yes` 無守門的事實                                                                                                                                                                                             | L354–357                                        | **留在根檔 delete 那一條旁邊**（埋進 core 的 reconcile 節，看 delete 的人找不到）                                                                                                                                                                                                                                  |

### 5-2 其餘搬移

| 內容                             | 行                         | 去處 / 動作                                                                                                                                                                                                                                                                                                                                                                                         |
| -------------------------------- | -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `legacy_inference_db_hint` 敘事  | L87（句中切）–103          | `crates/desktop/AGENTS.md` L12 之後，壓成 5 行。**切點在 L87 行內的句子邊界**：保留到 `…and the CLI's commands::tests::app_data_dir_matches_core_seam.` 為止（那個測試名要留在根檔），從 `A desktop upgrading from before that unification…` 起才搬                                                                                                                                                 |
| Skills Discovery 的實作細節      | L389–396, 398–403, 405–420 | `crates/core/AGENTS.md` skills/ 段之後。L384–387（mutation lock）原樣留根檔。**L389–396 保留兩條 aghub 不變量原字**（每個受支援 agent 都取 Referrer、兩條安裝路徑都必須走 `classify_agent`），只壓 dead-code warning 的推導。**L405–420 壓到 6–7 行，必須同時留住**：根 `SKILL.md` 是唯一判準、**絕不可用 `DESCRIPTION.md` 當標記**、探針要分辨 ABSENT 與 UNREADABLE（`chmod 000` 必須報 `Failed`） |
| Testing 段前半                   | L494–501                   | 壓成英文兩行 + 指路。**先把 `observed: a live ~/.config/orca/... in a test's allow-listed roots` 搬進 `crates/core/AGENTS.md` L69 再刪根檔**（core 版沒有這個實證，它是唯一證明「只換 `$HOME` 真會漏」的證據）。順序不能反                                                                                                                                                                          |
| `--json` 失敗格式                | L365–367                   | 刪除。`crates/cli/AGENTS.md` L74–78 已有更完整版本（還講了 `note_answer_on_stdout` 的雙 JSON 陷阱），只在句尾補一句 `Failures exit 1; clap usage errors stay exit 2 with prose.`                                                                                                                                                                                                                    |
| `delete` 的 outcome 五態         | L368–377                   | `crates/core/AGENTS.md` dto 段下方（保留 `kept` 的 `success: true` 悖論、`executed: true` 在全數失敗時仍為真、`absent` 壓過 caller 意圖、`would_prune_lock_entries` 與 `pruned_lock_entries` 的分離）。**另在 `crates/api/AGENTS.md` 補一行指向它並點名 api-only 的 `failed`**——桌面前端讀的是 API 回應、完全不會載入 core 的 AGENTS.md                                                             |
| L467–490（無寫者的共享唯讀目錄） | —                          | **不刪、不搬去 core**。它的讀者是改 descriptor 的人（工作在 `crates/agents`），而且帶著 L203–232 沒有的兩件事：`$XDG_CONFIG_HOME/agents/skills` 被 amp 與 kimi 讀、無人寫的具體實例，以及「完全沒有 write slot 也算 not served」。壓成約 5 行留在 roster 小節                                                                                                                                       |

### 5-3 原地修正（不搬家）

| 行                                              | 現況                                                                                                                                  | 改成                                                                                                                                                                                                                                                                                                                                        |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| L23                                             | `## Maps & Decisions (read these first)`                                                                                              | `## Maps & Decisions`。L25–36 十條條目**一字不動**（它們已經是文章的 Good 形式，只有標題那句把它降級成前置閱讀指令）                                                                                                                                                                                                                        |
| L37                                             | `.impeccable.md — Rust style`                                                                                                         | **這是確定的事實錯誤**。該檔實測是 `teach-impeccable` 產生的**前端設計 context**（Design Context / Users / Brand Personality / Aesthetic Direction、reference: Linear、anti-reference: Apple），與 Rust style 無關。兩個引擎都在照這行錯誤指路                                                                                              |
| L34–35, L554                                    | `Deep domain playbooks: project skills under .claude/skills/ (auto-register in Claude Code…)`                                         | 階段 4-A 搬完後改寫成 post-move 狀態，說明其他 agent 也讀得到 `.agents/skills`——**不要**寫成「Codex 看不到這個 skill，請直接讀路徑」（那是一連結就過期的暫時事實）                                                                                                                                                                          |
| L62–63                                          | `a legacy real-directory layout that migration deliberately leaves alone, D7`                                                         | 敘述不完整（不是自我矛盾）：讀者會把「migration 不動它」外推成「這個目錄絕對安全」，但 `repair` 會搬走**未被 git 追蹤**的實目錄。改寫見貼上文字                                                                                                                                                                                             |
| L139–141                                        | `capabilities.skills.universal: true` ALSO appends XDG `$XDG_CONFIG_HOME/agents/skills` — a SECOND shared slot (amp + kimi at global) | **與 L474–477 是同一個目錄的兩種說法**，而 L474–477 講的是「read by amp and kimi and written by neither」。只讀到 L139–141 的人會以為 amp/kimi 在那裡有寫槽，正好是 L474–477 要更正的誤解。兩處合併成一句，或在 L141 補上「讀而不寫」                                                                                                       |
| L450–454                                        | `(jetbrains-ai has been in that state for releases: the asset is jetbrains_ai.svg, so the key never matches.)`                        | **前提是假的**：`crates/desktop/src/lib/agent-icons.tsx:25-27` 已有底線 fallback（`${id.replaceAll("-","_")}.svg`），圖示沒壞。**只改文件、不要 `git mv` 檔案**：刪掉那個括號，並把 L451 更正為 globs 後先試 `${id}.svg` 再試底線拼法。保留真正的不變量：真的缺檔會靜默 fallback 成首字母 avatar，沒有任何 build/lint/typecheck/test 報錯   |
| L532                                            | `NEVER bypass ConfigManager`                                                                                                          | 與 `crates/core/AGENTS.md` L78 `NEVER bypass ConfigManager for config mutations` 重複（與 `--json` 那條同一個模式）。留 core 那份即可。同節 L533（API 錯誤不得回傳內部路徑）是 api 專屬，而 `crates/api/AGENTS.md` L103 已經反向指回根檔——這一對要一起決定歸屬。L540–545（`resolve_existing` 路徑正規化）在 core 沒有對應，**確認留在根檔** |
| `crates/api/AGENTS.md` ROUTES 表 `coverage` 列  | `per-agent coverage of .agents/skills master`                                                                                         | v2.18 前的說法，牴觸根檔的 Master 規則。改成 `per-agent skill coverage matrix (classify_all) — static capability view, no skill names or counts`。**同批順手修 `crates/api/src/routes/coverage.rs:13` 的 docstring**（`the canonical .agents/skills master SKILLS-DIR`）——留著就是留下重新汙染文件的來源                                    |
| `crates/desktop/AGENTS.md` L15–17               | `the universal Master (.agents/skills/)`                                                                                              | 同上。保留其後 `not auto-registered in Claude Code`（實測仍為真）                                                                                                                                                                                                                                                                           |
| `crates/agents/AGENTS.md` `format/` bullet      | 硬編 `All 24` / `17 json_map` / `the seven hand-written dialects`                                                                     | **一次改乾淨**，5 個硬編數字全改（只改前兩個會做出 7+17=24 算式斷掉的自相矛盾段落）。刪掉 `(it was a second copy, Discriminator, until it was merged)`——該型別已不存在，合併史仍在 `format/mod.rs:28` 的註解裡                                                                                                                              |
| `crates/markdown/AGENTS.md` `## DEPENDENTS`     | 較弱的 `extra_frontmatter` 副本                                                                                                       | 壓成一行指向 `crates/agents/AGENTS.md`（那邊多了真正會咬人的那句：一次 save 會重寫目錄裡**每一個** sub-agent，沒有這個機制的話新建一個就會抹掉 siblings 的 `tools`/`model`/`color`）。保留其後 `Keep this crate generic over T` 與兩條 ANTI-PATTERNS                                                                                        |
| `crates/skills-sh/AGENTS.md` + `CLAUDE.md`      | 531 B / 15 行，全是根檔模組圖已寫過的話                                                                                               | 刪除兩個檔。唯一有資訊量的一句併回根檔模組圖那行（見貼上文字）。已 grep 確認沒有腳本或測試在列舉 per-crate AGENTS.md 配對                                                                                                                                                                                                                   |
| `crates/skill/AGENTS.md` `## NPX LOCK CONTRACT` | 整個外包給 Codex 在本 repo 看不到的 `npx-skills-contract`                                                                             | 改成**內聯事實 + 可直讀路徑**（見貼上文字）。**不要**用 `aghub add`（會在 git 追蹤的 repo 裡生出 `.aghub/` Master——`.aghub` 不在 `.gitignore` 裡）也**不要**用 `repair`（對 git 追蹤目錄會拒絕）。階段 4-A 搬完後這句可以再簡化                                                                                                             |

### 5-4 指標衛生（與每次搬家同批完成，漏一處就是死指標）

1. `crates/cli/AGENTS.md` L6–8 的 `User-facing semantics … live in root AGENTS.md "CLI Command Surface" and are not repeated here.` 搬完就是反的，必須同批改寫。
2. 根檔 L298 的 `(reconcile-with-removals is dry-run — see above)` 改指 `crates/cli/AGENTS.md`。
3. 根檔 L484 的 `(see the repair bullet above)` 隨整段處理消失或改指 core。
4. `crates/api/AGENTS.md` 新增兩句：指向 core 的 reconcile 移除憑證、指向本檔新增的 `scope=all` 分歧。
5. 已掃過 inbound 引用：`.claude/skills/aghub-skills/SKILL.md:51` 與 `crates/agents/AGENTS.md:85` 指向根檔「Adding / Removing an Agent」、`crates/api/AGENTS.md:103` 指向根檔 Anti-Patterns——這三節都不動，所以沒有第五個死指標。

### 5-5 第二階段（同批規劃，可分次做）

`crates/core/AGENTS.md` 收完會從 82 行膨脹到約 230 行，而這個 repo 大部分非瑣碎工作都在 core——等於把稅從「每個任務」改成「每個 core 任務」。**必須同批做它自己的去重**（它已有 L39 `read_effect_after`、L41 mutation attribution 兩面牆），或把 skills/repair/removal 再下沉一層到 `crates/core/src/skills/AGENTS.md`。

同時處理 `crates/core/AGENTS.md` 的 Mutation attribution 段（3,696 B，比整份 `crates/git/AGENTS.md` 還大）：**只刪「當年怎麼爆」的病例敘事尾巴**，句尾加一行 `Full hole list and rationale: docs/specs/2026-07-29-skill-mutation-interprocess-lock.md.`。**必須逐字保留這五條**（原稽核的壓縮清單漏掉它們，照做是實害）：(a) reentrant per thread、keyed on ONE identity per scope（以別的東西排序就是 deadlock）；(b) 順序錯的巢狀 acquire 是 REFUSED 不是 deadlock；(c) 10s bound 只涵蓋 FOREIGN processes、同 process 排隊刻意不設上限；(d) `created_referrer_dirs` 刻意排除 `already_linked`；(e) `update_skill` 取 guard 但**不**重讀這個 documented exception（刪掉例外，下一個 agent 會「順手補上重讀」，而那次重讀曾讓 macOS 的 universal-rename relink 回歸）。預期省 600–900 B，**不要拿 2,300 B 當驗收標準**。

**這次完全沒被打開過的檔案，收工前至少讀一次**：`crates/cc-plugins/AGENTS.md`、`crates/inference/AGENTS.md`、`crates/remote/AGENTS.md`（各約 2KB）、`docs/agents/issue-tracker.md`、`docs/agents/domain.md`（根檔 L567–573 把工作流外包給後兩份，邊界——例如「issue 狀態要不要先問使用者才改」——很可能藏在那裡，稽核在指標處就停住了）、以及 `.impeccable.md` 本身（根檔第一行就點名它，而它現在是 `M` 未 commit）。

另外接收端要先量再倒：`crates/skill-update/AGENTS.md` 已經 **119 行**，是全部 crate 檔裡最長的（比 core 的 82 行還長），而且它的 PREFLIGHT 段與「更新狀態的語意」段本身就有重疊；`crates/api/AGENTS.md` 103 行同理。「搬下去就變便宜」只在接收端本身不肥時成立。

**收尾**：`cargo test -p aghub-cli agents_md_command_surface_lists_phase7_subcommands` 綠 + 從 **repo 根**跑 `bun run format`（pre-push hook 跑 `prettier --check .`，`just preflight` 不含它）。

---

## 階段 6：授權與完成定義

主題：這份 573 行的文件給了七項 ask-first，卻沒有任何一句放手授權、也沒有一句定義「做完」。文章第 4 點與 Persistence 段完全是在講這個。**把散在五處的修改合併成一次編輯**（`## Testing` 開頭、`## Commands` 之後、L516–525 權限表、L529–530 blockquote、L557）。

| 動作                                     | 位置                                         | 受益  | 風險                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ---------------------------------------- | -------------------------------------------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 新增 `## Definition of done`             | `## Commands` 之後                           | both  | scoped test 指令**必須用完整 module 路徑**——`--exact` 配短名會跑 0 個測試且 exit 0，拿它當「做完的定義」等於內建一個假綠門                                                                                                                                                                                                                                                                                                                                                |
| 保留 `Tag v* only after green CI`        | L557 只刪與 L150–153 重複的後半              | both  | 這是**唯一**一處要求「等 CI 綠了才打 tag」的敘述，新的 DoD 段沒有承接；刪掉就從文件裡蒸發了（latest.json 多腿競態那類事故都在 tag 之後才爆）                                                                                                                                                                                                                                                                                                                              |
| L150–157 兩個 bullet **原文搬進** DoD 段 | —                                            | both  | **不要改寫成結論**——「`crates/desktop` 自己的 `format:check` 看不到根目錄檔案」與 featured-check「指向別人的 repo 會腐壞」是因果，只留結論模型無法推廣                                                                                                                                                                                                                                                                                                                    |
| 權限表改三層並加 `Why` 欄                | L516–525                                     | both  | **閘門三（需你拍板）**：今天左欄寫的是 `scoped tests`，把整包 `just preflight` 升進自主欄是**政策變更**不是描述現況，這份稽核不能自行改寫。其餘：三層而非兩層，才能讓「合法但要問」不被讀成「不該做」。**不要**用捏造的理由子句（`zstd-sys`/`aws-lc-sys`/383GB 那組在原始出處講的是別的事）。dev-dep **不要**升到 Free 層——原表「without a clear need」本來就含出口。`Real ~/.agents` 擴寫成 `~/.aghub` + agent skill dirs + keyring（v2.18 Master 已搬家），右欄其餘不動 |
| `## Testing` 段開頭插入授權句            | L492 之後                                    | both  | **不要**宣稱 `--features agent-validation` 是網路——實測 `crates/core/Cargo.toml:34` 是空 feature，`integration_tests.rs:352-375` 的註解寫「require actual CLI binaries in PATH」，它 gate 的是 PATH 上要有真的 agent CLI。git 測試是 loopback `git daemon`（`git://127.0.0.1`）。也**不要**寫成「保證不外洩」——L497-499 記載真的外洩過，該講成「設計如此、外洩是那個測試的 bug」                                                                                          |
| Anti-Patterns blockquote 追加分類句      | L529–530                                     | both  | **不要**寫 `Approval boundaries live in exactly one place`——`.claude/skills/verify/SKILL.md:17` 與 `plan-desktop-ui/SKILL.md:8` 各有自己的閘門，寫下去當場就是假的                                                                                                                                                                                                                                                                                                        |
| WIP commit 授權                          | **不進 repo AGENTS.md**                      | codex | 這是你個人的工作流記憶，不是 aghub 的不變量，而 AGENTS.md 是 checked-in、會指導其他貢獻者的 agent。而且 Claude Code harness 預設就是 `Commit or push only when the user asks`，寫在 repo 檔壓不過 harness。放 `~/.codex/AGENTS.md` + `~/.claude/CLAUDE.md`。若真要在 repo 內留痕，只在 Testing 段的 `revert the fix` 那句後面加半句                                                                                                                                       |
| 全域「完成定義」段                       | `~/.claude/CLAUDE.md` + `~/.codex/AGENTS.md` | both  | 授權句**必須明文讓位給既有 stop-rule**——`codex:codex` plugin 的 contract 寫著「findings must never be auto-fixed — ask the user first」，寫成「只有破壞性動作才停」等於作廢它                                                                                                                                                                                                                                                                                             |

### 6-B `.agents/skills` 裡 10 句壞掉的 ask-the-user 閘門

同一句 `ask the user directly to clarify what you cannot infer.` 在 Codex 可見的 23 個 skill 裡出現 **10 次**，而且是壞掉的 find/replace 殘骸——`overdrive/SKILL.md:28` 是「2. **ask the user directly to clarify what you cannot infer.** to present these directions and get the user's pick before writing any code.」，句子接不起來；`critique/SKILL.md:176`、`teach-impeccable/SKILL.md:77` 同型。

這正是文章第 5 點指的「為舊模型寫的提早停手訊號」，而且它們比 AGENTS.md 的權限表更常被真的載入。改成「無法從 codebase 推得時才問」或直接刪。**這件事跟著閘門一走**——選 C（刪除）則自動消失。

---

## 階段 7：漸進揭露拆檔（工最大、收益中等，放最後）

主題：`root SKILL.md` 當 minimal router。**硬性順序：先判存廢（階段 0–2），再拆檔**——否則會先做 154KB 的 impeccable 拆檔再發現那 21 支要刪。

### 7-0 兩條判準（先訂，否則會拆出負收益）

1. **門檻**：`blen ≥ 10KB` **且**存在**互斥**分支才拆。10KB 以下、或雖大但每段都會執行的 runbook（`verify` 9.8KB、`init-deep` 的 Phase 1）一律不動。這條門檻回頭正好否掉 `wrangler` 的 per-binding 拆法（最大段僅 1,471 B，拆成十幾個 700B 小檔，每讀一次的往返成本高於內容本身）。
2. **`extra` 數字不能當漸進揭露指標**——要看 `references/` 是否真的存在。`redbook` 的 `extra=215` 全部是 npm 的 `dist/node_modules`，它的 `references/` 目錄根本不存在。

### 7-1 依 usage × 檔案大小排序（不是依檔案大小）

| 順位 | 目標                                | usage  | root            | 動作                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| ---- | ----------------------------------- | ------ | --------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1    | `notebooklm`                        | **33** | 32,604 B        | root 只佔全 skill 的 15% 卻每次全量載入。Episodic Podcast 段併入**既有的** `references/episodic_prompts.md`(17,524)、Publish 段併入**既有的** `references/publish-verify.md`(5,735)——**不要新建 `episodic.md`**（那會製造第二層重複，正是這條在罵的病）。逐段 diff 再刪。**四條唯一正本必須留字**（refs 命中 0）：`2026-08-19`、`35 集已回填`、`三輪重建`、`剝檔尾換行`。（`12 集實測` refs 已有 3 處，不必列入必留清單。）root → 約 6KB |
| 2    | `skillgenie`                        | **20** | 36,593 B        | 自有正本、已有 `## Reference files` 路由骨架（1,750 B），骨架齊了只是 workflow 沒搬。拆 `references/{creating,running-evals,improving,description-optimization,hosts}.md`。「教別人怎麼寫 skill 的 skill 自己違反漸進揭露」——投報率最高。動工前 `git -C ~/research/audi-skill fetch && git log --oneline HEAD..origin/master`                                                                                                            |
| 3    | `releasing-aghub`                   | **14** | 15,936 B        | 拆 `references/{troubleshooting,versioning,re-release}.md`。`## Cut a release` 的指令序列**一字不動**。**路由表必須逐字對上 description 承諾的兩個情境**（`macOS security import` / `sccache Cargo Fetch`、`-dev` 版號），否則 description 的承諾就斷了                                                                                                                                                                                  |
| 4    | `project-context-layout`            | **13** | 11,897 B        | 稽核初稿整個漏掉的一支。它已有 `references/{decision-matrix,memory-hierarchy-cheatsheet,topology-recipes}.md`，**但 root 裡還留著 `## Decision matrix: what goes where`(1,047) 與 `## The memory hierarchy (cheatsheet)`(1,050) 兩段同名內容**——跟 notebooklm 同一種病。另有 `## Workflow` 3,754（全檔最大段）、`templates/` 已外置但 root 仍有 `## Templates` 395                                                                       |
| 5    | `aghub-cli`                         | 2      | 24,234 B        | `## 3. Take one branch`（L216–356 的七個 h3）各搬成 `references/branch-<slug>.md`。**明確排除 L42 的 `### withheld is not coverage`**（它屬第 2 節「讀現況」，每次都要用，按 h3 機械掃描會誤搬）。root 留 1./2./storage model/4. 加症狀路由表——症狀欄要用 doctor 輸出的可觀察字樣，不要用抽象動詞                                                                                                                                        |
| 6    | `init-deep`                         | —      | —               | **階段 1 已刪，本條失效**                                                                                                                                                                                                                                                                                                                                                                                                                |
| 7    | `cloudflare-one` / `turnstile-spin` | 1 / 0  | 22,293 / 28,708 | **只在 0-C 決定保留時才做**（依 0-C 建議是刪除，所以預設跳過）                                                                                                                                                                                                                                                                                                                                                                           |
| 8    | 19 個設計動詞                       | 0      | 141,280 B       | **完全取決於閘門一**。選 B 就是這一步；選 A/C 則不做。先做 `delight`/`harden`/`critique` 三支驗證路由表寫法，再決定要不要推到其餘 16 支                                                                                                                                                                                                                                                                                                  |

### 7-2 `hindsight-coding-agent`：只做一步，不要拆

51,355 B、`## Configuration` 佔 81.5%、`extra=0`，看起來是最該拆的——但 usage 只有 **1 次**，而且它是 `vectorize-io/hindsight` 的第三方 source。**只做**：`aghub-cli apply-update skills hindsight-coding-agent -g --yes`（lock 的 `contentHash` 與 `check` 回報的 current 相同 → 沒有本地編輯會被蓋掉，這一步安全，成本一分鐘），做完量 `blen`。若上游仍是單檔，**停在這裡不拆**——1 次呼叫不值得接手一個外部 51KB 檔的維護。**刪掉「fork 後換 provenance」這個選項。**

### 7-3 `redbook` / `last30days` / `diagnosing-bugs`：三個「不要動」

- `redbook` 60,555 B、usage **0**、實體在 brew 的 `node_modules`——wrapper 化等於自己抄一份 60KB 副本跟 npm 版永久漂移，製造第 16 支散裝副本。只保留「`extra=215` ≠ 已拆檔」這個**盤點更正**寫進結論。
- `last30days` Master 240,205 B（全機最大），但**Claude 端根本不在 `~/.claude/skills`**（它走 plugin，快取版 3.18.4 / Master 3.23.0），所以「兩個引擎同時付」的前提不成立，只有 Codex 那條線在付；usage 0。**預設走解除連結而不是 fork**：先跑 dry-run `aghub-cli delete skills last30days -a codex -g`，讀 outcome 與 `would_prune_lock_entries`，確認 `~/.agents/skills/last30days` 那條 referrer 由誰持有再決定完整拼法——**不要照抄單一 `-a codex`**，那多半會被拒絕或留下共用 referrer。
- `diagnosing-bugs`：**不動**。主體（沒有 red-capable command 就不准往下、一次只動一個變數、`[DEBUG-a4f2]` tag 慣例、先產 3–5 個可證偽假說、Phase 6 清理檢查表）正是文章說該留的行為矯正；可刪的只有 1,400 B，卻要換一次第三方 fork 與永久同步成本。

### 7-3b `frontend-design` 與 `.impeccable.md` 的正面衝突（獨立於閘門一，一定要做）

`frontend-design` 在閘門一的**每一個選項下都會被保留**，但它的 Frontend Aesthetics Guidelines 那幾張 DON'T 與本 repo 的 `.impeccable.md` 直接對撞：

- `DON'T: Use overused fonts—Inter, Roboto, Arial, Open Sans, system defaults` vs `.impeccable.md` 的 **`Inter (current) is acceptable`**——而 Inter 正是這個桌面 app 的現行字體。
- `DON'T: Use monospace typography as lazy shorthand for 技術/開發者 vibes`——對一個顯示路徑、hash、JSON 設定的 agent 設定管理工具，等寬字是功能不是 vibe。
- `DON'T: Wrap everything in cards / DON'T: Use identical card grids / DON'T: Center everything / DO: 用 asymmetry、刻意打破 grid` vs `.impeccable.md` 的「資訊密度優先」「用結構而非裝飾建立層級」「Calm through consistency」。

修法：**在 `frontend-design/SKILL.md` 開頭加一條蓋全檔的規則**（不是只掛在 Design Direction 段開頭）：「若 repo 有 `.impeccable.md`，它與本檔任何 DO/DON'T 衝突時以 `.impeccable.md` 為準。」DO/DON'T 清單本身**不要刪**（它是這包唯一真正的資產），只是被覆寫。同時把 Context Gathering Protocol 的 `MUST / NOW / Do NOT skip / Do NOT attempt` 這組語氣改成陳述句，但**保留「別改用讀 code 猜」那半句**。這個檔是 git 追蹤的，就地改再 commit 本身就是 fork。

順手修 `critique/SKILL.md:151` 的兩個死引用（只在閘門一選 A/B 時）：它叫模型「use the selection table in the reference」但 `personas.md` 裡**沒有任何 selection table**；它還去讀 `.github/copilot-instructions.md` 的 `## Design Context`，而本 repo 的正本是 `.impeccable.md`，那個路徑不存在。死引用讓模型多跑一趟空搜尋。

### 7-4 router 的形狀（照抄本機已有的，不要自創）

樣板三份（實測大小）：`~/.claude/skills/cloudflare/SKILL.md` **8,991 B** + 319 個 reference；`~/.claude/skills/agent-reach/SKILL.md` **6,096 B** + 6 refs；`.agents/skills/frontend-design/SKILL.md` **9,569 B** + 7 refs（注意它的參考目錄叫 `reference/` 單數）。

統一 root 結構：`frontmatter → 1–3 行用途 → 每次都需要的那一節（前置條件／安全界線／隔離步驟）→ 兩欄表「可觀察的情境 → references/<file>.md（那裡有什麼）」`。路由句照 `frontend-design` 那種「去哪裡＋拿得到什麼」寫（`→ Consult [typography reference](reference/typography.md) for scales, pairing, and loading strategies`），不要寫「建議參考」。

**目標值：目錄型 root 收斂到 3–6KB、runbook 型 6–8KB**（`agent-reach` 真實是 6KB 的 router；本機沒有任何樣板做得到 1.5–3KB，不要訂那個數字）。

**拆檔前先檢查現有路由層**：`notebooklm` 有 `## References`(646)、`skillgenie` 有 `## Reference files`(1,750)、`project-context-layout` 有 `## References`(391) + `## Templates`(395)——這些要**改寫**而不是在旁邊另起一張新表，否則同一份 root 出現兩張路由表，模型更難選。

---

## 刻意不改的

### 複查階段判為 REJECTED 的

| 項目                                                                                 | 為什麼留                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **cloudflare 傘狀 vs 9 個葉子當成重複刪掉**                                          | 「傘狀 references/ 已完整涵蓋」是錯的。實測 `cloudflare/references/sandbox/` 裡 `grep -c 'sandbox@next'` = **0**（三支 sandbox 的全部價值就是那條 @next/stable 分線）；`turnstile/*.md` 裡 `api/v4/accounts` 命中 **0**，而 `turnstile-spin` 帶 4 支可執行腳本 + 6 支框架整合指南，傘狀一支都沒有；`agents-sdk` 葉子有 19 支主題 references，傘狀只有 5 個檔的通用摘要。傘狀的 63 個子目錄是同一套五檔模板的產物，是「文件摘要」；葉子是「動作流程＋腳本＋版本分線」。它們最後被刪是因為**這台機器上完全沒有這些技術（scope）**，不是因為重複——刪的理由不同，還原的判斷也不同 |
| **專案 `skills-lock.json` 22 筆 orphan-lock 所以該刪整個 lock**                      | 「沒有任何一條路能讓它變綠」是假的——`repair` 自己的拒絕訊息就印出逃生口：`git rm -r --cached <path> (and commit that), then re-run aghub skills repair <n>`。而且刪 lock 會**銷毀唯一的來源憑證**（22 筆各帶 `source: pbakaus/impeccable` + `computedHash`），此後 `check --online` / `source diff` / `apply-update` 永久失效、也再查不出它們來自哪裡。另外實測 `prune-lock -p` 現在就回「No orphaned lock entries」，它根本不認這 22 筆是 orphan，刪了目錄也不會動它                                                                                                         |
| **codex/antigravity/grok 的 16 條管線指令（attach/cancel/logs/result/status/wait）** | 它們**早就有** `disable-model-invocation: true`，從來沒被計費，刪了省 0。（唯一還成立的旁證：`codex:handoff` 的描述寫著 `Build a GPT-5.6 prompt for Codex`，而 Codex 現在跑 GPT-6 Astra，版本號過時——但那是 plugin 上游的事）                                                                                                                                                                                                                                                                                                                                                 |
| **Release 段的 `Tag v* only after green CI` 與 pubkey 兩行搬走**                     | 前半是**排序性的 release 知識**不是核准邊界；pubkey 那行是全檔後果最大的一行，而它現在所在的位置正是「人要發版的當下」會讀到的位置。為了省 310 字元把它移到 40 行外，換來的是唯一一次就近提醒消失                                                                                                                                                                                                                                                                                                                                                                             |

### 複查把「互補」救回來的（不要當成重複刪掉）

`skillgenie` vs `skill-doctor`（寫/改 skill vs 拿真實對話紀錄打分）已在階段 1 決定刪後者，但理由是 usage 22:1 不是「重複」。以下**一律不刪不合併**：

- `dual-host-plugin` vs `openclaw-plugin-dev-skill`（宿主完全不同）
- `codebase-design` / `domain-modeling` / `prototype` / `improve-codebase-architecture`（`ask-matt` 路由圖逐一指派的不同角色，而且你真的在跑這套：`.scratch/<feature>/issues/` + `docs/adr/`）
- `hindsight-coding-agent` vs `hindsight-knowledge`（官方 skill vs 自家家規的補充，不是分身）
- `audit` vs `critique`（技術品質檢查表 vs UX 評價，兩條不同軸；刪 audit 等於刪掉整份唯一的無障礙/效能檢查表）
- `quieter` vs `distill`（降低視覺強度 vs 刪元素降複雜度）
- `colorize` vs `bolder` vs `overdrive`（配色 / bland→impact / shaders+spring physics，各有不同執行手冊）
- 三支 sandbox 的互相 `Not for X (use Y)`
- 13 支帶 `disable-model-invocation` 的 matt-pocock skill（本來就沒計費，刪了只會少掉能 slash 呼叫的工具）

### 短的消歧義句要留、長的路由散文才刪

文章的 bad example 是過度**寬泛的正面觸發詞**（`Use when working with databases`），它批評的是 over-emphasize when skills SHOULD be used。兩支近義 skill 之間一句 10–15 字的 `Not for X (use Y)` 是**消歧義**，刪掉反而讓 `verify` / `verify-desktop-ui`、`plan-desktop-ui` / `polish` 更常誤觸發。**保留**：`aghub-cli`、`verify-desktop-ui`、`plan-desktop-ui`、`dual-host-plugin`、`worktree-cwd-guard` 的短排除句、`hindsight-knowledge` 的 `Does not replace hindsight-coding-agent`、`upstream-skills-flow` 的三方指路。**只刪**長篇路由散文（`orchestration` 866 字裡約 450 字在講 orca-cli / Computer Use / Playwright 該用哪個）。

### repo 不變量與 runbook 步驟（文章第 3 點明確豁免）

- `crates/api/AGENTS.md` 的 CORS & BROWSER-DRIVE-BY DEFENCE 全段（尤其 `every /api/v1 route (except OPTIONS) takes _origin: TrustedLocalOrigin as its first parameter`——新增路由時唯一的步驟提示）、`in_mutation_pool` 的 NEVER
- `crates/desktop/AGENTS.md` 的 Tauri capabilities 兩條 NEVER、sync `#[tauri::command]` 阻塞 NEVER、以及 `## CRITICAL: HEROUI V3`（記憶會騙人、必須抓 live docs；階段 5 要搬同一個檔，這段必須原地留下）
- `crates/git/AGENTS.md` 的 HTTPS-only token / redact userinfo
- `crates/json/AGENTS.md` 的 `patch_jsonc_object` 會刪掉 `T` 沒列的欄位
- 根檔 L70–74 的 `## Where to Look — Only where the obvious guess is wrong; everything else, ask CodeGraph`（全檔最符合文章第 4 點 good 範例的一段）
- `releasing-aghub` 的 `## Cut a release` 指令序列、`verify` 的 Build + launch 隔離步驟、`init-deep` 的 Phase 1（都是每次都要執行的連續流程，拆出去只增加往返）
- Testing 段的「畸形 fixture + fail CLOSED」那半（lock 讀取路徑對 `check`/`doctor`/`source` 是 fail CLOSED，少一個欄位會讓指令在**讀取階段**就 bail，斷言通過而受測程式從沒被執行）
- `overdrive` 的 fallback 紀律（每個技法都要有能看的 fallback、一定要處理 `prefers-reduced-motion`）與 `@supports` / `if ('gpu' in navigator)` 偵測寫法——**只刪會腐化的具名瀏覽器支援度快照，不要順手改寫成一份新快照**

### 真正的 ask-first 邊界（權限表右欄不動）

`git push` / force-push / amend published history、release tags / `just bump` / Homebrew tap、真實 `~/.aghub` + `~/.agents` + agent skill dirs + 系統 keyring、shipped `tauri.conf.json` 的 updater `pubkey` 與 `endpoints`。Codex 在 `approval_policy = "never"` + `sandbox_mode = "danger-full-access"` 之下，那張表是唯一的剎車。

### 其他複查降級為「不動」的

`writing-claude-rules` 與 `project-context-layout` 保持 **Claude-only**（`.claude/rules` 放置決策與 `paths` frontmatter 是 Claude Code 專屬概念；`agents-md-architecture` 的 `evals/trigger-evals.json:50` 還留了一條註記明說「`.claude/rules` 拓樸不是 B 分支的地盤，誤觸屬預期」——作者是刻意劃出去的。折進一支兩引擎共用的 skill 反而會讓 Codex 載入一套它永遠用不到的指令）。

---

## 要貼上的文字

### A. 根 `AGENTS.md` — `## Definition of done`（插在 `## Commands` 之後）

```markdown
## Definition of done

Done is a green gate, not a first implementation that compiles. Pick the gate by
blast radius, run it yourself, and do not come back for review between
implementing and verifying.

- **Scoped change**: the change's own test exists and
  `cargo test -p <crate> <full::module::path::name> -- --exact` is green. A bare
  short name under `--exact` runs ZERO tests and exits 0 — check the test count.
- **Before push or tag**: `just preflight` AND `bun run format:check` from the
  REPO ROOT. Neither alone is a pushable tree — preflight runs no
  prettier/eslint, and `crates/desktop`'s own `format:check` never sees root
  files; the pre-push hook runs no tests.
- **Before tagging a release**: tag `v*` only after green CI.
- **After editing `crates/desktop/src/data/featured-skills.json`**:
  `just featured-check`. It needs the network and a `gh` login, which is why it
  sits outside preflight — the catalog points at other people's repos and rots
  on their schedule.

Return early only when an ask-first item below blocks you, or when the gate
fails for a reason outside the requested change. A failure you caused is part
of the task, not a reason to stop.
```

### B. 根 `AGENTS.md` — `## Testing` 段開頭插入（現有 `**Do not pollute real home**` 之前）

```markdown
**The existing suite is designed to write only into temp dirs, an isolated
`$HOME` and `$AGHUB_DATA_DIR` — a leak into the real home is a bug in that test,
not a reason to ask before running the suite.** The Rust test suite makes no
outbound network calls: its git-backed tests serve `git://` from a loopback
`git daemon`. What reaches outside a plain `cargo test` is `just featured-check`
(public GitHub plus a `gh` login) and the `verify` chain; `--features
agent-validation` needs real agent CLIs on `PATH`, not the network. Run
`cargo test`, `cargo test --workspace` or `just preflight` freely, fix the
failures your change caused, and rerun without asking for approval at each step.
What keeps this true is a rule, not a question: `crates/core/AGENTS.md`
ANTI-PATTERNS forbids clearing `skills_path_override` for a global write without
isolating `$HOME`. Honour it in the tests you WRITE, and the suite stays free to
RUN.

When you write a new test, the isolation is yours to get right:
```

其後接壓縮過的原段：

```markdown
**Never pollute the real home**: a global-scope write still lands in `~/.aghub`
plus each agent's own skills dir, and overriding `$HOME` alone is not enough.
Isolation mechanics, the one-env-mutex-per-binary rule and the inode-assertion
trap: `crates/core/AGENTS.md` Testing.
```

### C. 根 `AGENTS.md` — 權限表（整段替換 L516–525）

```markdown
## Agent permissions / approval boundaries

Reasons are given so you can generalize to the case not listed here.

| Tier                                                     | What                                                                                                                                                                                                                                                                      | Why                                                                                                                                                 |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Free — do it, do not ask**                             | Editing code; `just fmt` / `just lint`; `cargo build`; `bun run typecheck` / `lint:check` / `format:check`; the Rust test suite at any scope, `just preflight` included; reading and writing under a temp dir, `$AGHUB_DATA_DIR`, or a tempdir project root               | Run it, fix the failures your change caused, and rerun. Stop and ask only if a test would need the real `~/.agents`, the OS keyring, or the network |
| **Ask first — legitimate, but external or irreversible** | `git push`, force-push, amending published history; release tags, `just bump`, the Homebrew tap; touching the developer's REAL `~/.aghub`, `~/.agents`, agent skill dirs or the system keyring; adding any new workspace dependency (runtime or dev) without a clear need | These leave the machine or cannot be undone; the dependency budget is the maintainer's call                                                         |
| **Never — no task reaches these**                        | Changing the shipped `tauri.conf.json` updater `pubkey`, or pointing its `endpoints` elsewhere. Committing secrets                                                                                                                                                        | It bricks auto-update for every installed user                                                                                                      |
```

### D. 根 `AGENTS.md` — Anti-Patterns blockquote 追加（L529–530 同一個 blockquote 內）

```markdown
> These are correctness invariants, not approval boundaries: they constrain
> WHICH design you pick, never WHETHER you proceed. None is a reason to stop and
> ask — and none is negotiable either; pick a design that satisfies them. The
> same holds for every `NEVER` in a per-crate `AGENTS.md`. Approval boundaries
> are the section above; a project skill may add its own gate for its own
> workflow.
```

### E. 根 `AGENTS.md` — `## CLI Command Surface` 的 stub（取代 L170–381，四個關鍵字必須逐字保留）

```markdown
Flag-level semantics (scope exclusivity and the rootless `-p` bail, destructive
dry-run defaults, `--json` failure shape, narrowed resource args, `skill-usage`,
`coverage`, `inference`): `crates/cli/AGENTS.md`. Removal and keep semantics for
`delete`, `transfer` and `reconcile`: `crates/core/AGENTS.md`. `check` /
`source diff` orchestration, and why "update available" includes a local edit:
`crates/skill-update/AGENTS.md`. The `scope=all` divergence of
`GET /skills/sources/diff`: `crates/api/AGENTS.md`.

- **`delete mcps <name> -a claude -p --yes` has no roster guard** — it reports
  `outcome: "removed"` and copilot loses the server too (verified; left alone
  deliberately).
```

### F. 根 `AGENTS.md` — L62–63 改寫

```markdown
Also at the repo root: `.agents/skills/` (this repo's own hand-edited skills, a
legacy real-directory layout that lazy migration deliberately leaves alone, D7)
and `justfile`. `repair` still moves a real directory there into the store when
git does NOT track it; a tracked one is refused, and the refusal prints the
escape (`git rm -r --cached <path>`). Details: `crates/core/AGENTS.md`.
```

### G. 根 `AGENTS.md` — L34–35 / L554（階段 4-A 搬完後）

```markdown
- **Deep domain playbooks**: project skills under `.agents/skills/`, mirrored as
  symlinks in `.claude/skills/` — Claude Code auto-registers them, and every
  other agent in the roster reads the `.agents/skills/` copy (do not re-list the
  catalog here)
```

### H. 根 `AGENTS.md` — 模組圖 `skills-sh` 那行（刪掉該 crate 的 AGENTS.md/CLAUDE.md 後）

```
  skills-sh/     # skills.sh registry client (search only); go through `Client`,
                 #   base URL from `SKILLS_API_URL` / `ClientBuilder::api_url` —
                 #   never hardcode it in a caller
```

### I. `crates/skill/AGENTS.md` — `## NPX LOCK CONTRACT` 整段替換

```markdown
## NPX LOCK CONTRACT (do not break)

The global lock stays frozen at v3, the project lock at v1; the global entry's
`skill_folder_hash` is always empty; every write is temp-file + rename. Full
contract: the `npx-skills-contract` project skill (read
`.agents/skills/npx-skills-contract/SKILL.md` directly if your agent does not
auto-register it).
```

### J. `crates/cli/AGENTS.md` — 取代 L6–8 的 disclaim

```markdown
User-facing semantics live below in this file (scope flags, destructive `--yes`
defaults, the `--write-result` filename guard, `inference --api-key -`).
Cross-surface removal and repair semantics are in `crates/core/AGENTS.md`.
```

### K. `crates/markdown/AGENTS.md` — `## DEPENDENTS` 整段替換

```markdown
## DEPENDENTS

`aghub-agents` (`sub_agents.rs`) — sub-agent frontmatter schema and the
`extra_frontmatter` preservation rule (including what a single save rewrites)
live in `crates/agents/AGENTS.md`.
```

### L. `~/.codex/AGENTS.md` — 插在 `@/home/audichuang/.codex/RTK.md` 之後、`<!-- CODEGRAPH_START -->` 之前

```markdown
- 回答與 commit message 一律使用繁體中文。
- 程式碼中的註解使用英文。
- Git commit / PR 不要加 Co-Authored-By 或任何 attribution 行。
- 開啟 .md 檔請用 `code <path>`。

## 完成的定義

改完不等於做完：跑過這次改動影響到的檢查（測試／typecheck／lint）、修掉自己造成的失敗、再跑一次綠了，才算完成。不要在第一版實作完就停下來要我 review，中間每一步也不用逐次問我。

派 subagent 或 review agent 之前、以及做反證測試（把修復 revert 掉看斷言變紅）之前，先在非 main 分支做一次本地 WIP commit——這不用問我；把那些 commit 推出去才要問。

例外（仍要停下來問）：破壞性或不可逆的動作（push、發版、動真實家目錄與 keyring）；以及既有 skill／plugin 明文要求停手的情形（例如 Codex review 回來的 finding 一律先問過再修）。要我停在探索階段時我會明說。
```

同一段「完成的定義」也貼進 `~/.claude/CLAUDE.md`。

### M. `~/.claude/CLAUDE.md` — 第 6 行改寫

```markdown
- Git commit / PR 不加 attribution 行（Claude 側已由 `~/.claude/settings.json` 的 `attribution` 強制；此行僅為說明）。
```

### N. 新描述全文（可直接複製到各自 `SKILL.md` 的 `description:`）

**`agent-reach`**（890 → 約 160）

```
從網路抓取內容:使用者要調研/搜尋/查/找某個主題,貼了任何 URL,或提到 Twitter/X、B站、Reddit、LinkedIn、YouTube、V2EX、雪球、小宇宙播客等平台。小紅書先用 redbook。只負責取得內容,不做發文按讚等寫入,也不做報告撰寫或翻譯。
```

改的是 `description:` 欄位。**`triggers:` 區塊與 `metadata.openclaw.homepage` 不要一起刪**——它們不計入描述預算（`dlen` 只算 `description`），是 openclaw 的路由資料。反過來也要知道：`triggers:` 在選擇階段兩個引擎都不讀，所以被砍掉的平台名是真的從選擇面消失，它不是安全網。

**`orca-upstream-pr`**（846 → 208）

```
對 Orca upstream (stablyai/orca) 開啟或更新 pull request,從 audichuang fork。用於 提PR / 開PR / 上游 PR,以及後續的 轉 ready / CodeRabbit 回來了沒 / 看看 CI。
```

**`releasing-aghub`**（757 → 262）

```
Cut a desktop + CLI release of this aghub fork via the tag-driven GitHub Actions pipeline, and fix the failures it hits (macOS `security import`, sccache Cargo Fetch). Also for `just bump`, a `-dev` version string, or verifying artifacts / latest.json / the Homebrew tap.
```

**`cloudflare-mcp-worker`**（789 → 257）

```
Scaffold a remote MCP server that deploys to Cloudflare Workers in TypeScript — OAuth 2.1 + JSON-RPC over Streamable HTTP + KV. Use when building or deploying an MCP server ON Cloudflare Workers. Not for configuring an MCP client entry in an agent's config.
```

**`hindsight-project-bank`**（802 → 271）

```
Point a local project at a Hindsight memory bank so every coding agent on the machine shares it. Use when adding a repo to Hindsight, mapping a path to a bank id, asking why a repo has no memory, or importing old conversations. Not for page content (hindsight-knowledge).
```

**`hindsight-knowledge`**（512 → 250）

```
House rules for what belongs on a Hindsight knowledge page versus AGENTS.md, and how to fix a page whose scope is wrong. Use when finishing or shipping a feature (收口 / 做完了 / 上線了), writing or correcting a knowledge page, or choosing page vs AGENTS.md. Does not replace hindsight-coding-agent.
```

**`remote-mcp-headless-auth`**（744 → 243）

```
Finish the OAuth login for a remote HTTP/SSE MCP server on a box where the localhost callback can't be opened (VM, SSH, container). Use when `claude mcp list` shows "Needs authentication", or an expired token made its `mcp__*` tools disappear.
```

**`dual-host-plugin`**（575 → 157）

```
讓同一份 plugin / skill 在 Codex 與 Claude Code 兩個引擎都裝得起來、跑得動。用於跨引擎移植、從頭寫雙宿主 plugin,或 Codex 端裝不動 / 裝了跑不動(marketplace.json 格式、plugin not found、hook 沒觸發)。只牽涉單一引擎則不用。
```

**`worktree-cwd-guard`**（555 → 164）

```
交棒前確認自己在正確的 worktree / branch:派 subagent 或 codex 前的 preflight、產生自驗 prompt、codex companion gate 三旗用法,或 GIT_DIR / GIT_WORK_TREE 汙染害 git toplevel 跑錯。不涉及交棒的單純 git 問題不用。
```

**`aghub-cli`**（560 → 259）

```
Install, relink, update and diagnose skills through the `aghub-cli` binary. Use when an agent can't see an installed skill, a skill needs linking to every agent, an authored edit needs publishing to its git source, or `doctor` / lock-file state needs reading. Not for dual-host plugin loading (dual-host-plugin).
```

**`skillgenie`**（497 → 245）

```
Create, edit and evaluate agent skills. Use when writing a new skill, reviewing a SKILL.md against authoring best practices, fixing a skill that fails to trigger, tuning its description, or splitting content between SKILL.md and reference files.
```

**`validate-inherited-root-cause`**（483 → 約 220）

```
接手 handoff、上一個 session 的筆記、或另一個 agent 的分析,裡面已經「確認了 root cause」(常附「不要再重查」「N 個 subagent 都同意」「兩份分析一致」),而你準備據此動工(spec / plan / code)之前,先驗證那個前提是真的。尤其當你自己重現不了這個 bug、或改完卻「使用者說沒效果」時。你能自己重現且觀察一致就不用。
```

**`upstream-skills-flow`**（987 → 約 287）

```
Side-by-side map of what the upstream vercel-labs `skills` CLI does at each lifecycle step (add/install/update/remove/sync) and which aghub function mirrors it. Use when you need to answer "what does upstream do here?". Frozen contract: npx-skills-contract; aghub-side invariants: aghub-skills.
```

**`npx-skills-contract`**（461 → 258）

```
The frozen interop contract aghub must preserve to stay round-trip compatible with `npx skills` — lock-file schemas, the .agents Master + symlink layout, and the folder-content hash. Use when changing a lock schema, the install layout, or the hash algorithm.
```

**`aghub-skills`**（418 → 228）

```
aghub-side invariants for the skill-management subsystem — lock files, install layout, transactional fs mutations, cross-crate wiring. Use when editing skill install, update, remove, prune, rename or discovery code in this repo.
```

**`plan-desktop-ui`**（629 → 232）

```
Plan a page-level change to aghub's desktop UI before writing it — prototype with real local data, get approval, then write the spec. Use on 這頁怪怪的 / 版面很亂 / 重新設計這個頁面, or when a redesign is big enough that guessing wrong is expensive.
```

**`verify-desktop-ui`**（559 → 231）

```
Run aghub's desktop frontend in headless Chromium against a real aghub-api, screenshot it, and assert on the rendered DOM. Use on 跑一下桌面版 / 截圖看看 / 這頁點下去會怎樣, or when a `crates/desktop` change needs more than typecheck and unit tests. Not for aghub's CLI or its skill-install chain — that is `verify`.
```

**`writing-claude-rules`**（508 → 165）

```
撰寫或審查單一條 Claude Code 規則,以及決定它該放 CLAUDE.md / .claude/rules / skill / 不留。也用於 path-specific rule 的 paths frontmatter、CLAUDE.md 瘦身。整個 repo 的分層拓樸用 project-context-layout。
```

**`project-context-layout`**（830 → 262；Java 觸發詞待你確認）

```
Architect the Claude Code context layout across a multi-service repo or monorepo — CLAUDE.md hierarchy, `.claude/rules/`, `.claude/skills/`. Use when restructuring context across several packages or services, or asking 這條規則該放哪. Single rule or one file: writing-claude-rules.
```

**`ticktick-skill`**（383 → 182）

```
Manage TickTick tasks, projects, tags and habits via CLI. Use when the user asks to create, query, update, complete or delete a task, manage projects or tags, or check in on a habit.
```

**`heroui-react`**（315 → 172；改了會永久 update-available）

```
HeroUI v3 React components (Tailwind v4 + React Aria). Use when building UI with @heroui/react — Buttons, Modals, Forms, Cards — or configuring its oklch dark/light themes.
```

**`openclaw-plugin-dev-skill`**（451 → 210）

```
Write, structure, install and publish OpenClaw plugins. Use when creating a new OpenClaw plugin, fixing an existing one, or publishing it to npm. Triggers on openclaw plugin / openclaw插件 / openclaw.plugin.json.
```

**`research`**（238 → 約 110；第三方 source，改了會被 sync 蓋回）

```
Delegate primary-source reading to a background agent and leave a cited Markdown file in the repo.
```

**`design-pass`**（新建 router，約 150；只在閘門一選 B 時需要）

```
aghub 桌面前端的視覺調整動作集：排版／間距／色彩／動效／無障礙稽核。要改 crates/desktop 的視覺呈現時使用;各動詞的執行手冊見 references/。
```

---

## 驗收方式

### 主驗收（可觀察，不依賴字元數）

1. **開一個全新的互動 Claude session**（不是 subagent）在 `/home/audichuang/research/aghub`，檢查 skill 清單：**8 個 aghub 領域 skill（`verify`、`releasing-aghub`、`aghub-skills`、`npx-skills-contract`、`upstream-skills-flow`、`testing-fs-failures`、`plan-desktop-ui`、`verify-desktop-ui`）全部帶著描述出現**。這是主驗收——現在有 4 個只剩名字。
2. **次驗收**：「只剩名字」的 skill 數量從 48 往下降。註記：只改寫描述**必然達不到 0**（85 個可見 skill 就算每個壓到 150 字元也要約 14.5k + 名字，已經貼著今天觀察到的約 16k 天花板），唯有刪掉整包才可能接近。
3. **在 Codex 開一個 session** 做同樣的觀察——階段 0 的 O1 是 before、這裡是 after。整份稽核對 Codex 的效果只有這個能證明。

### 分項驗收

| 階段    | 指令                                                                          | 期望                                                                                                         |
| ------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| 0-C / 1 | `readlink -f ~/.claude/skills/* \| grep skillshub`                            | 全部處理完後回空，才能退役 `~/.skillshub`                                                                    |
| 4-A     | `cd /home/audichuang/research/aghub && ls -la .claude/skills \| grep -c '^d'` | 0（全 symlink，無實體目錄）                                                                                  |
| 4-B     | `readlink -f ~/.claude/skills/agent-reach ~/.codex/skills/agent-reach`        | 兩行都指向 `~/.aghub/agent-reach`                                                                            |
| 4-B     | `aghub-cli doctor -g --verify-links --fail-on-issues`                         | exit 0（註記：`untracked` 是受支援的靜止狀態、不判紅；`--from` 匯入的 Master 之後 `check` 會永遠回 `local`） |
| 5       | `cargo test -p aghub-cli agents_md_command_surface_lists_phase7_subcommands`  | 綠（這是根檔重構唯一的機械護欄）                                                                             |
| 5       | `wc -l AGENTS.md`                                                             | 每搬一段對一次帳，最終約 300 行                                                                              |
| 5       | 從 **repo 根**跑 `bun run format`                                             | pre-push hook 跑 `prettier --check .`，`just preflight` 不含它                                               |
| 5       | `just preflight`                                                              | 綠                                                                                                           |
| 1 / 2   | `aghub-cli doctor`                                                            | 刪 `skill-doctor` 後若報 `orphanMaster` → 手動 `rm -rf ~/.aghub/skill-doctor`                                |
| 7       | 各拆檔 skill 的 `wc -c SKILL.md`                                              | 目錄型 3–6KB、runbook 型 6–8KB                                                                               |

### 不要用來驗收的

- **不要拿 8,000 字元當目標**。你記憶裡的那個數字極可能是 token 不是字元（16k 中英混排 ≈ 8k token），而實測今天存活的描述就有約 16.1k rendered。
- **不要拿「省了 N 字元」當成功指標**，除非 O3 的最小可證偽實驗顯示刪除真的讓被剝掉的描述復活。丟棄規則實測與長度、字母序、mtime、來源目錄都無關（`agent-reach` 890 存活、`prototype` 179 被丟；`cloudflare-one` 325 存活、`cloudflare` 368 被丟），所以「壓短就會救回來」是未經證實的假設。
