# 遷移實戰打出來的 aghub 缺口（2026-09-10, ubuntuvm + 本機）

環境：`aghub-cli 2.21.2`（brew）。VM 是 pre-2.18 佈局，40 個 global lock 條目
（39 個 `foreignLink`）＋一份以 `$HOME` 當 project root 的 `~/skills-lock.json`（29 條）。

每條都附「怎麼重現」與「我實際觀察到什麼」。修法欄是建議，不是結論。

---

## B1 — `accept-rename` 沒有 `--force-unsafe`，被稽核擋住的技能永遠改不了名

**Status:** open · 影響：中 · 證據等級：實測

`--force-unsafe` 只在 `apply-update` 與 `source sync` 上（`add` 刻意沒有，ADR 0002）。
但 `accept-rename` 也走同一條 `skill-update/src/mutation.rs` 的 source-mutation seam，
那個 request struct 有 `force_unsafe`（`mutation.rs:104`），
`crates/core/src/skills/rename.rs:528` 直接硬寫 `force_unsafe: false`。

於是一支被稽核判 `Malicious` 的技能：**裝得起來**（`source sync --force-unsafe`）、
**更新得了**（`apply-update --force-unsafe`）、**永遠改不了名**。

### 重現

```sh
aghub-cli source accept-rename --help    # Options 裡沒有 --force-unsafe
# 對一支稽核判 malicious 的技能：
aghub-cli source accept-rename old new -a claude --yes   # → VALIDATION_FAILED，無 override
aghub-cli source accept-rename old new -a claude --yes --force-unsafe  # → clap error, exit 2
```

實例：`audichuang/audi-skill` 的 `nas-container`（22 項 findings / 2 critical，
`aghub_credential_file_exfil` 誤判，見同目錄 `nas-container-audit.md`）。

### 建議修法

在 `crates/cli/src/main.rs` 的 accept-rename 加 `--force-unsafe`，一路傳到
`rename.rs:528`。要保持 ADR 0002 的「override 是 request 欄位」不變。
**要一起想清楚的**：rename 是交易式的（ADR 0001），override 落在交易的哪一端。

---

## B2 — `$HOME` 被當成 project root，造成 project/global 路徑塌在一起

**Status:** open · 影響：中高（會產生長期的假 issue）· 證據等級：實測

`core/src/paths.rs` 往上找 agent marker；`$HOME` 底下有 `.claude/`，所以任何在
「沒有更近的 marker」的目錄下跑的 project-scope 命令都會把 root 解到 `$HOME`。

VM 上的實際後果：

- `~/skills-lock.json`（29 條 project lock）—— 幾乎確定是誤觸產生的
- project root = `$HOME` 時，project store 就是 `~/.aghub`（**與 global 同一個目錄**），
  project claude 目錄就是 `~/.claude/skills`（**與 global 同一個目錄**）
- 於是 `doctor -p` 看到 31 個 `untracked`（master 在但 project lock 沒收）
  ＋ 9 個 `orphan-lock`，`--fail-on-issues` 永遠是 exit 1
- 9 個名字同時存在兩份 lock，兩個 scope 共用同一個實體 master

### 重現

```sh
cd ~ && aghub-cli doctor --verify-links -p --json | python3 -c "
import json,sys,collections; d=json.load(sys.stdin)
print(collections.Counter(r['health'] for r in d))"
# → {'untracked': 31, 'orphan-lock': 9, 'ok': 20}
```

### 建議修法

`paths.rs` 拒絕（或至少警告）`project_root == dirs::home_dir()`。
`$HOME` 不是專案。**注意**：這是行為變更，要先確認沒有人刻意用 `$HOME` 當專案。

---

## B3 — `repair`（不帶 NAME）第一輪不會收拾私有目錄裡的重複副本，要跑第二次

**Status:** open · 影響：中（第一輪的成功訊息會蓋掉殘留）· 證據等級：實測

第一輪：39 個 `unmigrated_copy → migrated`，exit 0。但 `hindsight-coding-agent`
在 claude/copilot/cursor/gemini/grok 五個私有目錄各有一份**真目錄副本**，
第一輪一個都沒收（那時 master 還不存在，claude 那格被判 `realPathConflict`，
沒進計畫）。`doctor --verify-links` 仍回 `issues`。

**一模一樣的第二次 `repair <name>`** 就把它判成 `conformant` / `reconciled`，
5 份收成 5 條 Referrer。

根 `AGENTS.md` 記載 compat-dir 拆除已經做成單趟（「或本次要 adopt 成 master 的那個目錄」），
但**私有目錄裡的 ForkedCopy 沒有被同一條補償涵蓋**。

### 重現

```sh
# pre-2.18 佈局 + 某個 agent 私有目錄有同名真目錄
aghub-cli repair -g --yes --json      # 40 skills, 全部 migrated/tidied, exit 0
aghub-cli doctor --verify-links -g --json   # 仍有 realPathConflict
aghub-cli repair <that-name> -g --yes --json  # → reconciled
```

### 建議修法

`plan_repair` 對「本次會建立 master」的技能，把私有目錄的 `ForkedCopy` 一起排進計畫
（compare-then-quarantine 的判斷條件已經有了，缺的是排程順序）。
或者退一步：第一輪結束時明確回報「還有 N 格需要再跑一次」，不要只回 exit 0。

---

## B4 — `doctor` 的 `updatable` 在 master 不存在時是 false，與它自己的文件矛盾

**Status:** open · 影響：低（JSON-only 提示）· 證據等級：讀碼＋實測

`crates/cli/src/commands/doctor.rs:218` 的註解寫
「True when the source is a git repo — i.e. `check`/`apply-update` can refresh it」，
但第 601 行是 `updatable = fetchable && skill_path.is_some() && valid_skill`，
而 `valid_skill` 要求 master 是可解析的目錄。

於是 `orphan-lock`（master 不存在）一律 `updatable: false` —— 偏偏
**`apply-update` 正是官方用來把不見的資料夾裝回來的手段**（見 `Skill update pipeline`
知識頁：「Applying an `UpdateAvailable` result is what restores a missing folder」）。
遷移前 VM 上 39 列全是 `updatable: false`，source 卻明明是 `audichuang/audi-skill`。

### 重現

```sh
# 任何 orphan-lock 的列
aghub-cli doctor -g --json | python3 -c "
import json,sys
for r in json.load(sys.stdin):
    if r['health']=='orphan-lock': print(r['skill'], r['source'], r['updatable'])"
```

### 建議修法

要嘛把 `valid_skill` 從 `updatable` 拿掉（它是「來源可取得」不是「本機健康」），
要嘛改註解並改名（例如 `refreshable_in_place`）。兩者都可以，但目前這樣會誤導自動化呼叫端。

---

## B5 — 安全稽核會讀 `.skill` 打包刻意排除的 `tests/`、`evals/`

**Status:** open · 影響：低 · 證據等級：實測

`crates/skill/src/package.rs` 刻意排除根層 `tests/`、`evals/`（不打包），
但稽核的輸入走 `skill::collect_skill_files`（folder hash 的走訪），會讀進去。
`nas-container` 因此有 3 個 low findings 落在**永遠不會安裝出去的檔案**上
（`tests/test-fnos-ssh.sh`、`tests/test-env-compare.py`、`evals/*.json`）。

不是安全漏洞（多報不是少報），但會讓 findings 數字失真、也讓作者去修不會出貨的檔案。

### 建議修法

稽核輸入沿用打包的排除規則，或在 finding 上標記「此檔不隨安裝出貨」。
**反方意見**：git-source 安裝其實會把 `tests/` 一起 materialize（lock 的
`contentHash` 也涵蓋它們），所以「不會出貨」這個前提要先驗證再動。

---

## 非 bug（查證後排除）

- **`doctor` 對 `orphan-lock` 的建議是對的**：第 807 行確實印
  「run `aghub-cli prune-lock`」。原本懷疑它只給 reinstall 建議，讀碼後排除。
- **`repair` 不動未受管的真目錄**：那是 D5 明文設計（lock 是 worklist，
  沒有 lock entry 指名的實體目錄只回報不動）。VM 上 5 個孤兒
  （`agent-reach`、`alpha`、`browser-use`、`browser-use-cli`、`self-distill`）屬於此類。

---

## B6 — `repair -p` 會把 git 追蹤的「就地撰寫」技能搬進未追蹤的 `.aghub`（我實際踩到並還原）

**Status:** open · 影響：**高** · 證據等級：**實際造成損害並還原**

### 我做了什麼、發生了什麼

在 VM 的 `~/research/aghub-wt/auto-deploy`（aghub 的 git **worktree**）跑
`aghub-cli repair -p --yes`，它回報 `{'migrated': 22}`、exit 0、doctor 綠。
但那個 repo 的 `.agents/skills/` 是**就地撰寫、git 追蹤**的技能原始碼（40 個追蹤檔）。
結果：

```
git status --porcelain | awk '{print $1}' | sort | uniq -c
     29 ??      ← .aghub/、.grok/、.omp/、.opencode/、.pi/、.cursor/skills/、22 條新 symlink
     39 D       ← .agents/skills/**/SKILL.md 全變成「已刪除」
     22 M       ← .claude/skills/<n> 原本是追蹤的相對 symlink ../../.agents/skills/<n>
                   被改指成絕對路徑 <root>/.aghub/<n>
```

技能原始碼被 rename 進 `.aghub/.quarantine/<name>/<stamp>/`。**exit 0、沒有任何警告。**
一個照著「完整遷移到 .aghub」逐專案跑 repair 的使用者，會在 commit 前看到 39 個刪除，
或更糟——沒看 git status 就繼續工作。

### 為什麼這與文件矛盾

根 `AGENTS.md` 寫：`.agents/skills/`（this repo's own hand-edited skills — a legacy
real-directory layout that **migration deliberately leaves alone, D7**）。
但 D7 的「leaves alone」只涵蓋 **lazy** 路徑（某個 mutating flow碰到 legacy 技能時
才順手遷移）；**bulk 的 `repair` 明文就是「遷移 lock 指名的每一個技能」**，
所以那句保證在使用者執行 `repair -p` 的那一刻就失效。這不是散文寫錯，是保證的前提沒有守衛。

### 重現（在 aghub repo 或它的任何 worktree 裡）

```sh
cd <aghub repo 或 worktree>
git ls-files .agents/skills | wc -l        # 40 —— 就地撰寫且被追蹤
aghub-cli repair -p --json                 # 計畫 22 個 migrated，看不出任何危險
aghub-cli repair -p --yes                  # exit 0、doctor 綠
git status --porcelain                     # 39 D + 22 M + 29 ??
```

### 還原配方（實測有效，quarantine 與 git 內容逐檔相同、沒有未追蹤檔遺失）

```sh
for d in .agents/skills/*; do [ -L "$d" ] && rm "$d"; done
git checkout -- .agents/skills .claude/skills
rm -rf .aghub .grok .omp .opencode .pi .cursor/skills .agents/.aghub-mutation.lock
git status --porcelain                     # 應為空
```

### 建議修法（三種，優先序由高到低）

1. **`plan_repair` 對「real dir 是 git 追蹤的」的技能改判 refuse**，訊息說明它是
   就地撰寫的來源、遷移會讓它離開版本控制。判準可以是 `git ls-files --error-unmatch <path>`
   等價的檢查，或更保守：只要 `<root>/.git` 存在且該路徑被追蹤就拒。
2. 退一步：`repair -p` 在 dry-run 就把「會被 rename 進 quarantine 的路徑」明列出來，
   並在有 git 追蹤時要求額外的旗標（例如 `--migrate-tracked`）。
3. 最低限度：修 `AGENTS.md` 那句話，明說 `repair` **會**動它，D7 只保證 lazy 路徑不動。

### 這個形狀在兩台機器上有多普遍（掃描結果）

```
本機：aghub(40) · leetcode-video(73) · truthmark(24) · rebased(13) · open-notebook(5)
      · hermex(2) · codex-cli-best-practice(1) · honcho(1) · aghub/fixtures(1)
      · aghub 的 worktrees（各 40）
VM ：aghub(40) · aghub-wt/auto-deploy(40) · aghub/fixtures(1)
```

`leetcode-video` 本機有 73 個追蹤檔且 `skills-lock.json` 排了 3 個遷移 —— 差一步就踩第二次。
**所以這不是邊緣情況：任何把技能就地寫在 repo 裡的人，跑一次 bulk repair 就中。**
