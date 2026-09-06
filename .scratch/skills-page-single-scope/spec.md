# 技能頁重構：範圍單一主人、來源群組即入口

Status: approved（使用者已在互動原型上確認）
Date: 2026-09-06
Prototype: `/tmp/claude-1000/-home-audichuang-research-aghub/00660886-65e2-4d0c-8612-1cd813a93abc/scratchpad/skills-page.html`（可直接用瀏覧器開；也在 http://192.168.31.65:8765/）

## 為什麼

`pages/settings/skills.tsx` 現況有三個結構性問題：

1. **scope 沒有單一主人。** 頁面用 `useState` 持有 scope／projectPath，開頁時又從來源 key（`scope:projectRoot:source`）反解；點來源會反寫 scope 下拉，改 scope 下拉會清掉來源。遷移橫幅寫死 `scope="global"`（skills.tsx:726），專案範圍下顯示的是全域的遷移狀態。
2. **「依 Agent」不是依 Agent。** `groupBySource={true}` 寫死（skills.tsx:944），兩個視圖都以來源為軸；「依來源」視圖只是換一份清單看同一批來源的 diff。
3. **三條橫幅 + 兩個外觀相同的下拉疊在 324px 內。** 遷移橫幅、背景檢查橫幅、全部更新橫幅，加上 scope Select 與 agent Select。

另有一個已確認的 bug：`components/skill-detail.tsx:666` 手刻來源 key 用 `source`，清單自 5c7d6361 起用 `sourceUrl` 當 key，所以「在來源中檢視」會落到「選擇一個來源」空面板。

## 目標行為（對照原型）

### URL 狀態（nuqs）

| 參數     | 值                                                                                                             | 規則                                                                                                                                                           |
| -------- | -------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `scope`  | `global`（預設，為 global 時不寫進 URL；nuqs 用 `clearOnDefault: true` 即可，不要手刻）或 `project:<絕對路徑>` | 唯一的資料根。解析失敗→`global`。`project:` 路徑不在已登錄專案清單→維持 project scope，ScopeControl 顯示 placeholder，清單與右側顯示既有的「選擇專案」空狀態。 |
| `skill`  | 技能名                                                                                                         | 與 `source` 互斥：設定其一必清另一個。                                                                                                                         |
| `source` | 來源的 `sourceUrl`（clone URL）**或** lock 的 source id（`owner/repo`）                                        | 不再內嵌 scope。在**目前 scope** 的來源清單裡解析：`r.sourceUrl === v \|\| r.source === v`。找不到→右側顯示「這個範圍裡沒有這個來源」。                        |
| `view`   | **刪除**                                                                                                       | 不再有視圖切換。                                                                                                                                               |

切換 scope：保留 `agent` 篩選（本地 state 即可）與搜尋字串，清掉 `skill` 與 `source`。

### 左欄（由上而下，最多這些）

1. **Row A：`ScopeControl` + agent 下拉並排**，grid `42fr / 58fr`，gap 6px。agent 下拉用圖示當視覺標籤（`aria-label` 保留文字），選項為「全部（N）」+ 每個有技能的可用 agent「名稱（N）」，依數量降冪。有技能的 agent 少於 2 個且沒有啟用篩選時，不渲染 agent 下拉，ScopeControl 佔滿整列。`ScopeControl` 加一個可選的 `className` prop 讓呼叫端覆寫；**預設值必須維持 `max-w-[48%]`**，因為 `pages/settings/coverage.tsx` 也在用它，那頁不能變。
2. **token 估算行**：只在選了某個 agent 時出現，一行、muted、`tabular-nums`；Claude 超預算時整行 `text-warning`。沿用 `SkillAgentFilterRow` 現有算法與文案，只是版面拆開。
3. **`ListSearchHeader`**：搜尋 + 多選 + 新增 + 重新整理，維持現狀。
4. **一條狀態列**（新元件，建議 `components/skill-status-strip.tsx`）：一個容器，每個事實一列、按鈕靠右，沒有事實時不渲染。列的優先序：
    - 有快取的待更新數 → 「N 個技能可更新」+「全部更新」（沿用 `handleUpdateAll`）
    - 否則若 `backgroundNews !== null` → 「背景檢查發現 N 個更新」+「重新整理」（沿用 `handleRefreshSkills`）
    - 目前 scope 的 repair 預覽有 `migrated` 項 → 「N 個技能仍用舊版面」+「預覽遷移」，按下開啟 `SkillLayoutMigrationBanner` 既有的 Modal。**scope 與 projectPath 跟著頁面 scope 走**，不再寫死 global。做法固定：`SkillLayoutMigrationBanner` **保留現在的 Alert 為預設呈現**（`pages/project/detail.tsx:300` 還在用，那頁不動），另加一個 compact 列模式（例如 `variant="row"`）給狀態列用；資料查詢與 Modal 兩種模式共用。頁面上不要再出現第二個 Alert。
      容器要有 `role="status"` + `aria-live="polite"`（這是持續為真的狀態，不是 toast；見 `crates/desktop/AGENTS.md`）。
5. **標籤 chips 列**（`SkillTagFilterRow`）：維持現狀。
6. **`SkillList`**（見下）。

### `SkillList`（`components/skill-list.tsx`，`groupBySource` 路徑）

- **每個來源都是群組**，包含只有 1 個技能的來源（刪掉現在把 single-item 攤平到「Ungrouped」的邏輯）。標題就是進入來源面板的入口。
- **沒有 lock 記錄的技能**收成一個群組，標題文字 i18n key `skillsUnrecordedSource`（zh-Hant「本機自建（未記錄來源）」），有數量、有收合，**沒有** ↗ 與管理代理按鈕。
- 群組標題：點名稱區 → `onOpenSourceView(sg.source)`；chevron 是獨立按鈕，只負責收合。新增 prop `selectedSource?: string | null`，值是 **lock 的 source id**（與 `sg.source` 同一種東西）；頁面先把 URL 的 `source` 值解析成 row，再把 `row.source` 傳進來。直接拿 URL 的 sourceUrl 比 `sg.source` 永遠不會相等，選取態會靜默失效。符合時標題呈現選取態（accent 文字）。保留 ↗ 與 Users 兩顆按鈕。
- **共同狀態上提**：若群組內所有可見技能的 `updateStatuses` 都是 `uncheckable` 且 `reason` 相同，標題只顯示一次（`auth` → 一顆「綁定憑證」按鈕呼叫 `onResolveAuth(第一個技能名)`；其他 reason → muted 文字「全部無法檢查」帶 tooltip），該群組的列**不再**各掛 `SkillStatusBadge`。
- 技能列：**移除** `BookOpenIcon` 與 `AgentIcons`；右側改為 muted、`tabular-nums` 的 `items.length`，`title` 說明「安裝於 N 個 agent」；`agentFilter` 啟用時不顯示這個數字。星號、標籤 chips、狀態徽章維持。
- 純邏輯抽到 `lib/`（例如 `lib/skill-group-status.ts`：`sharedUncheckableReason(names, statuses)`），配單元測試。

### 右側面板

- `source` 參數有值且解析到 row → `SourceDetail`（keyed by sourceUrl）；`sourceImporting` 流程維持。
- 否則 `skill` 參數 → 既有 `activeGroup` 邏輯（含「選不到就退回第一個可見群組」）→ `SkillDetail`。
- 都沒有 → 既有「選擇一個技能檢視詳情」。
- `panelMode`（create / import / import-github）維持，仍只在沒有選來源時有效。

### 深連結修正

- `components/skill-detail.tsx` 「在來源中檢視」：改成導向 `/skills?…`，帶 `scope`（`group.items[0].source === "project"` 時為 `project:<projectPath>`，否則不帶）與 `source`（`currentSkillSource.sourceUrl ?? currentSkillSource.source`）。不再手刻複合 key。
- `App.tsx` 的 `/sources` route 與 `SourcesRedirect` 刪除（唯一呼叫者就是上面那顆按鈕）。
- `components/unified-resource-list.tsx` 的 `SkillList` 補 `onOpenSourceView`，導向 `/skills?scope=project:<projectPath>&source=<source id>`，讓專案頁也能進來源面板。
- `pages/settings/skills.tsx` 現有 `onOpenSourceView` 把 source id 對到目前 scope 的 row，改寫 `source=row.sourceUrl`。

### 刪除

`ViewMode`、`handleSetView`、`ToggleButtonGroup` 視圖切換、`SourceListPanel`、`sourceRowKey` 的複合 key 解析（`parseScopeFromKey` / `parseProjectFromKey`）、抓全部專案來源的 `allSourcesQuery`（只需要目前 scope 一份 `sourcesListQueryOptions`；注意 `useCredentialSpeedHint({ sources })` 目前吃的就是那份全量 rows，改成餵目前 scope 的 rows，不能把這個 hook 拔掉）、`PROJECT_KEY_PREFIX` 若不再需要、i18n 死鍵（`viewByAgent` / `viewBySource` 等，三個語系一起）。

## 非目標

- 多選模式、標籤篩選：**保留現有功能**，只是位置不動。
- `pages/project/detail.tsx` 與 `UnifiedResourceList` 的整體結構不動（只補 `onOpenSourceView`）。
- `pages/settings/coverage.tsx` 不動。
- 不改 Rust、不改 DTO、不改 API。
- 不做視覺主題調整；沿用現有 Tailwind token（`bg-surface`、`text-muted`、`border-separator`…）。

## 實作規範

- 讀 `crates/desktop/AGENTS.md` 與 `crates/desktop/src/AGENTS.md`。HeroUI v3 元件**不要憑記憶寫**，用 `heroui-react` skill 或 v3 文件確認 `Select` / `ListBox` / `Button` / `Chip` 的 compound 寫法（現有 `ScopeControl` 就是可抄的範本）。
- 純邏輯進 `lib/`，配 `*.test.ts`（`node --test`，純 TS，不能 import React/DOM）。至少：URL 參數解析／序列化與互斥、`source` 解析（sourceUrl 或 id）、共同狀態上提。測試要能在退回修正時變紅（可以先寫測試看它紅）。
- 程式碼註解英文；使用者可見文案走 i18n，三個語系同步。
- `bun` only。**不要 commit**：在 `feat/skills-page-single-scope` 分支（從 `main` 建）上留工作樹變更即可。
- 不用 `useEffect` 同步狀態或抓資料。
- 動到本規格沒列出的檔案時，要在回報裡說明為什麼。

## 驗收（全部要綠，貼輸出）

從 `crates/desktop`：

```
bun run typecheck
bun run lint:check
bun run test
```

從 repo root：

```
bun run format:check
```

另外手動走一遍這些情境並在回報中逐條確認（可用 `bun run dev` 搭配本機 `aghub-api`，或至少依程式碼路徑說明）：

1. 開頁無參數 → 全域、無選取、第一個可見群組的詳情。
2. 選專案 aghub → URL 出現 `scope=project:/home/audichuang/research/aghub`，狀態列顯示該專案的舊版面數（本機實測為 22），不是全域的 42。
3. 點某來源群組標題 → 右側 `SourceDetail`，URL 只多 `source=<url>`，scope 下拉不變。
4. 在來源面板點回某技能 → `skill=` 取代 `source=`。
5. 技能詳情「在來源中檢視」→ 右側正確落到該來源面板，含專案範圍的技能。
6. 選 agent → 每列右側數字消失，token 估算行出現；清掉篩選則反之。
7. audi-skill 群組（私有 repo，17 個 auth uncheckable）標題只出現一顆「綁定憑證」，列上無徽章。
8. 重新整理瀏覽器 → scope／選取都從 URL 還原。

## 回報格式

- 改了哪些檔案、各自做什麼（一行一檔）。
- 規格裡沒寫、你自己拍板的決定（逐條）。
- 沒做到或拿不準的地方（逐條，不要藏）。
- 四個驗收指令的輸出摘要 + 八個情境的逐條結果。
