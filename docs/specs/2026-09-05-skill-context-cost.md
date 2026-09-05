# 技能啟動 context 成本:各 agent 的載入原理

**日期**: 2026-09-05
**證據等級**: 直接反編譯已安裝的二進位,非文件推測

## 結論(先看這段)

三家主流 agent **完全收斂**在同一個模型:

> 啟動時進入 context 的只有每個 skill 的 **name + description 一行**。
> SKILL.md 的 body **只有在該 skill 被實際呼叫時才載入**。
> 三家都有一個 **字元預算**,超過時會把 description 降級或整個拿掉。

所以「裝一堆 skill 會吃多少 token」是**可以純前端算出來的** —— aghub 的
`SkillResponse` 已經同時有 `name` 和 `description`,不需要讀 SKILL.md body,
不需要新的 API。

## Claude Code 2.1.261(完整演算法)

從 `/home/linuxbrew/.linuxbrew/Caskroom/claude-code@latest/2.1.261/claude`
(bun 編譯的 ELF)抽出的 JS chunk。

### 每個 skill 的 listing 行

```js
descText   = whenToUse ? `${description} - ${whenToUse}` : description
entryLen   = name.length + 4 + min(descText.length, skillListingMaxDescChars)
             // "- " (2) + ": " (2) = 4
nameOnly   = name.length + 2          // 被降級時只剩 "- name"
totalChars = Σ entryLen + (n - 1)     // 換行
```

`skillListingMaxDescChars` 預設 **1536**(settings 可調)。

### 預算

```js
budget =
	SLASH_COMMAND_TOOL_CHAR_BUDGET ?? // 環境變數,若 > 0 直接用
	floor(contextWindow * bytesPerToken * skillListingBudgetFraction);
// 預設 fraction = 0.01, bytesPerToken = 4, contextWindow 預設 200000
// → 200k 模型 = 8000 字元;1M 模型 = 40000 字元
```

`SLASH_COMMAND_TOOL_CHAR_BUDGET` **確實存在**(不是傳說)。

### 超過預算時的降級順序

不是截斷字串,是**整個 description 被拿掉,只留名字**。誰先被犧牲由使用分數決定:

```js
usageScore(skill) = usageCount * max(0.5 ^ (daysSinceLastUse / 7), 0.1)
```

分數高的先保住 description(貪婪配置),分數低的降級成 `- name`。
bundled skill 永遠保留。狀態欄位:`budgetMode: "fits" | "priority"`。

### Claude Code 自己就有這個功能

`/skills` 的報告已經會印:

- `context` 欄 = 「this skill's one-line listing in the system prompt, included every turn」
- 註解原文:`(dash = not in the current listing, costs nothing; full SKILL.md loads only when it runs)`
- `7d tokens` = 過去 7 天歸因到該 skill 的 token
- 警告:「N skills loaded but never invoked. Each one adds to the system prompt every turn.」

使用紀錄存在本機 settings 的 `skillUsage: { [name]: { usageCount, lastUsedAt } }`。

## Codex 0.153.4

同一個模型,`ext/skills/` 這個 extension crate。遙測欄位直接把設計講白:

```
codex.thread.skills.enabled_total / kept_total / truncated
codex.thread.skills.description_truncated_chars
budget_limit, total_skills, included_skills, omitted_skills,
truncated_description_chars_per_skill, truncated_skill_descriptions
```

使用者可見訊息:

> Skill descriptions were shortened to fit the skills context budget. Codex can
> still see every skill, but some descriptions are shorter. Disable unused
> skills or plugins to leave more room for the rest.

> Host skills are available but omitted from the model-visible skills list
> because the skills context budget was exceeded.

差別:Claude Code 是**整條 description 拿掉**,Codex 是**把 description 縮短**
(`truncated_description_chars_per_skill`),兩者都保留 skill 可見。
系統提示用 `<skills_instructions>` 包住,條目含 name / description / location。

## Grok 1.0.13

模板直接寫在二進位裡(minijinja):

```xml
<available_skills>
  <skill>
    <name>{{ skill.name }}</name>
    <description>{{ skill.description|e }}</description>
    <location>{{ skill.location }}</location>
  </skill>
</available_skills>
```

一樣是 description-only,但**每個 skill 的固定開銷最大**:XML 標籤約 60 字元
再加一個 `<location>` 絕對路徑。Grok 還會讀 `~/.claude/skills/`。

## 其餘 22 個 agent

平行查了 `AgentType::ALL` 剩下的每一個(amp / antigravity / augmentcode /
cline / copilot / cursor / factory / gemini / hermes / jetbrains_ai /
kilocode / kimi / kiro / mistral / openclaw / opencode / pi / roocode /
trae / warp / windsurf / zed)。**全部都是 descriptionOnly**,沒有一個
把 SKILL.md body 灌進啟動 context。

結論:**不需要**在 descriptor 上加 `startup_load` 之類的欄位。沒有變異
要編碼,加了就是一個永遠只有一種值的欄位。

## Tokenizer:一個常數會錯四倍

在本機真實語料上量過(146 個真的 SKILL.md description + 使用者自己的
繁體中文技術筆記,o200k_base):

| 文字     | 字元/token |
| -------- | ---------- |
| 英文散文 | 4.75       |
| 純漢字   | 1.03       |

也就是漢字幾乎**一字一 token**。用單一 chars/4 常數,中文描述會被低估
約四倍 —— 而這個專案的技能描述常常是中文。所以 token 估算分兩桶算。

**但預算判定不受影響**:Claude Code 的預算是**字元**制,不是 token 制。
所以「有沒有超出預算、幾個描述會被丟掉」是精確的,只有顯示的 token
數字是估計值。

## 對 aghub 的意義

| 項目                          | 結論                                                                  |
| ----------------------------- | --------------------------------------------------------------------- |
| 要不要讀 SKILL.md body 來估算 | **不要**。body 不進啟動 context                                       |
| 資料夠不夠                    | 夠。`SkillResponse.name` + `.description` 就是全部                    |
| 需要後端改動嗎                | 不需要。純前端可算                                                    |
| tokenizer                     | 不用裝。Claude Code 自己就用 **4 bytes/token** 當常數                 |
| 比 token 數更有用的訊息       | **「哪幾個 skill 會被降級成只剩名字」** —— 那是使用者真的會失去的東西 |
| MCP                           | **算不出來**。aghub 只存連線設定,tool schema 要連上 server 才知道     |

### 各 agent 的 per-skill 開銷差異

| agent  | 格式                                     | 固定開銷/skill   |
| ------ | ---------------------------------------- | ---------------- |
| claude | `- name: description`                    | 4 字元           |
| codex  | `name` / `description` / `location` 條目 | 中等             |
| grok   | `<skill><name>…</name>…</skill>`         | 最大(含絕對路徑) |

aghub 目前用 claude 的公式當共同基準,並在 UI 標明是估計值。

## MCP:查完了,結論是「這不是你的問題」

使用者要的是「MCP 也算一下 token」。查了啟動載入方式之後,答案跟直覺相反。

### 啟動時 MCP 到底放什麼進 context

- **tool 名稱** + 每個 server 的 **`instructions`**(initialize 回應裡的那段
  markdown,可以很長 —— 本機 codegraph 那份 650+ 字)→ 立刻進 context
- **完整的 inputSchema JSON → 預設不進去**。Claude Code 的 tool search 預設
  開著,schema 是 deferred,要用到那個 tool 才載入

也就是說:**閒置的 MCP tool 很便宜**。天真地把所有 schema 加總會嚴重高估。
這跟 skill 的情況剛好相反 —— skill 的描述是每回合都在的固定成本。

### 為什麼 aghub 算不出精確數字

`McpServer` 只有連線設定(command / args / env / url / transport),沒有
tool 清單。要拿到 `tools/list` 就得真的連上去。

找過所有離線來源:

| 來源                                                     | 有沒有                                   | 涵蓋率                                                     |
| -------------------------------------------------------- | ---------------------------------------- | ---------------------------------------------------------- |
| Cursor `~/.cursor/projects/*/mcps/<server>/tools/*.json` | **有**,含完整 schema + `INSTRUCTIONS.md` | 本機 **0/6** —— 只快取了 Cursor 自己的內建與 plugin server |
| Claude Code `~/.claude/`                                 | 沒有。只有 `~/.claude.json` 的連線設定   | 0                                                          |
| Claude Code transcript `prompt_snapshot`                 | 格式存在於 binary,但實際沒有落檔         | 0                                                          |
| Codex / Grok                                             | 只有 OAuth lock 與 log,無 schema         | 0                                                          |

Cursor 的快取格式是可用的,但只有 Cursor 連過的 server 才有。使用者實際在
用的 6 個(codegraph / firecrawl / hindsight / medium / notebooklm /
ticktick-ts)一個都不在裡面,所以照這條路做出來的面板會是全空的。

### 建議

不要為了湊一個數字去 spawn 使用者的 process。真要做,唯一可行的是
**每個 server 一顆明確 opt-in 的「連線並量測」按鈕**:

- 需要新增 Rust MCP client 相依(專案規定新 workspace 相依要先問)
- 需要 spawn 使用者的 process、帶他們的密鑰(對外行為,要先確認)
- 量到之後可以把結果快取起來,之後就離線可用 —— 也順便補上 Cursor 那條路
  的空白

在那之前,可以講給使用者聽的正確結論是:**MCP 閒置成本低,技能描述才是
每回合都在付的錢。**
