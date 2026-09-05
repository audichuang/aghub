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
