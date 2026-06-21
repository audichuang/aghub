# Spec: Skills / Sources 合併與更新顯示優化

- **Status**: Draft — 已對抗性驗證並修正（2026-06-20，主迴圈自證；subagent 批次因 session 限額未跑，見 §11）
- **Date**: 2026-06-20
- **Surfaces**: Desktop (`crates/desktop`), API (`crates/api`), domain (`crates/skill-update`, `crates/skill`, `crates/core`)
- **Related**: `docs/specs/2026-06-02-sources-and-universal-install.md`（Sources 頁原始設計）、`CONTEXT.md`、skills `npx-skills-contract` / `aghub-skills` / `upstream-skills-flow`

## 1. 問題

桌面端的「技能」與「來源」是兩個分開的分頁，但它們其實是**同一份 lock 資料的兩種投影**：後端 `skill_update::sources::list_sources` 字面上就是「installed skills GROUP BY source」。加上 `skills.sh`（registry 搜尋），同一條「發現 → 安裝 → 管理 → 更新」生命週期被切在三個入口，彼此**只靠 React Query cache 失效鬆散相連**，沒有任何 UI breadcrumb。

具體痛點：

1. 同一個技能在「技能」detail 與「來源」detail 都能做 install/update/delete，語意重複。
2. 更新走兩套狀態機、兩套詞彙：`GET /skills/check-updates`（4 態 `upToDate/updateAvailable/renamed/uncheckable`，且 GET 會 auto-heal lock）vs `GET /skills/sources/diff`（7 態，多 `notInstalled/removed/deprecated`，唯讀）。`2026-06-02` spec 明寫 diff 應「吸收」check-updates，但兩條路由都還在。
3. 更新狀態預設**隱形**：badge 在 status `undefined` 時不畫，使用者不按 Refresh 完全沒有「有更新」訊號，也沒有「上次檢查時間」。
4. **滿屏綠勾**：`upToDate` 對每個技能畫綠色「已是最新」Chip（`skill-update-badge.tsx:56-74`），視覺噪音大。
5. detail 卡上有兩顆近乎雙胞胎的更新鈕（`Apply` = apply-update 自動 swap；`Sync from source` = 手動 scan/branch/preview），圖示都是 `ArrowPathIcon`，無從分辨。
6. `renamed` 是死路：badge 顯示「→ 新名」但不給按鈕，apply-update 會以 `SKILL_RENAMED_CODE` 硬拒，唯一指引藏在使用者看不到的 server error string；Sources 頁的「處理」是叫使用者複製 `aghub-cli install <new>` 去終端機跑。
7. `uncheckable` 把 7 種原因塌成同一顆灰問號；可修的 auth 情況只有在 detail（有 `onResolveAuth`）才變按鈕，list 永遠是惰性問號。
8. 更新身分用 8 字元 content-hash 呈現，對非開發者無意義。
9. 來源 detail 把 7 種狀態各切一個區塊，視覺很雜，`removed/deprecated/uncheckable` 差異細微。

## 2. 已鎖定的決策（與使用者確認）

- **D1 — IA**：「技能」與「來源」合併成單一「技能中心」頁，左上 `View by: Agent / 來源` 分段切換；`skills.sh` **維持獨立的「市集 / 探索」入口**，但結果列要接上「已安裝」狀態（本 spec 只做接狀態，不併入）。
- **D2 — 自動檢查**：進頁**靜默自動跑** `check_updates`（走 ls-refs preflight 便宜路），標題列顯示「上次檢查 X 前」+ 手動重新檢查；重的 `diff_source`（full fetch）維持**點某個來源才觸發**。
- **D3 — 更新顯示**：去掉綠色「已是最新」徽章；徽章只在「可更新 / 已改名 / 需要憑證」等**要動手**時出現；檢查完成用 toast 摘要回饋；hash 換成人話（版本 / 日期 / 改了什麼）。

## 3. Non-goals（本 spec 不做）

- **不**把 `skills.sh` 併進技能中心（維持獨立入口）。
- **不**改動 lock 檔 schema、`.agents` Master + symlink 佈局、folder-content hash 演算法——這些受 `npx-skills-contract` 凍結，round-trip 相容性不能破。後端「兩個 hash key（`content_hash` vs `computed_hash`）」「project lock 缺 `source_url`」的儲存不對稱**維持現狀**；統一只發生在 read-model / API / FE 層。
- **不**動 inference / MCP / plugin 等其他分頁。

## 4. 設計

### 4.1 資訊架構（D1）

- Sidebar：`技能`（合併後）、`市集 (skills.sh)`、其餘不動。
- `技能` 頁頂部：標題 + 右側「上次檢查 X 前」+「重新檢查」+ 全域摘要 chips。
    - ⚠️ **全域摘要只放 check-updates 算得出的數**：`N 可更新`（outdated）+ `N 需處理`（renamed + uncheckable）+ `N 最新`。**不放全域「可安裝」**——`notInstalled` 只能靠 `diff_source` 逐來源 fetch（`list_sources` 只回 `skill_count`），全域算它會違反 D2。「可安裝」只在**單一來源 detail 的 summary bar** 出現（見 4.5，那時已 diff 過該來源）。詳見 §11 驗證發現。
- 其下一排分段切換 `依 Agent | 依 來源`，兩個視角**共用同一份查詢結果**（installed skills + lock + update state），差別只在前端分組：
    - **依 Agent**：沿用現 `skills.tsx` 清單（同名聚合成 `SkillGroup`，列上顯示 agent 圖示）。
    - **依 來源**：沿用現 `sources/index.tsx` 的 scope→source 樹；列上若該來源有更新顯示一個小點（由 check-updates 結果推導，不需先 diff）。
- 兩視角的 detail panel 與「列」元件**抽成共用 component**（單一 `SkillRow` + 單一 `SkillStatusBadge` + 單一 install/agent-select dialog），避免把割裂搬進同一頁。
- **Scope**：合併後頁面需支援 global + project scope 切換（現 `skills.tsx` 硬鎖 `scope:"global"`，project skills 只在 project 頁 `UnifiedResourceList`）。本 spec 把 scope 切換納入。

### 4.2 統一狀態詞彙（前端層）

前端定義單一 7 態 union（diff 的 `SourceSkillState` 為超集）。Rust enum 的序列化字串為：
`notInstalled | installedCurrent | installedOutdated | renamed | removed | deprecated | uncheckable`（見 `crates/skill-update/src/sources.rs:37-56`）。FE union 直接沿用這組字串值（不要自創 `current/outdated`，以免與後端漂移）。

- `check-updates` 的 4 態映射進去：`upToDate→installedCurrent`、`updateAvailable→installedOutdated`、`renamed→renamed`、`uncheckable→uncheckable`（`notInstalled/removed/deprecated` 在 check-updates 不會出現，只有 diff 會給）。**字串必須用 `installedCurrent/installedOutdated`，不可用 `current/outdated`**（見 §12-C3）。
- `SkillStatusBadge` 規則（取代現 `skill-update-badge.tsx`）：
    - `installedCurrent` → **不畫任何東西**（與「還沒檢查」一致）。
    - `installedOutdated` → 黃色「可更新」+ 一鍵「更新」。
    - `renamed` → 紫色「已改名」+ 一鍵「採用新名」（見 4.5）。
    - `removed / deprecated` → 低調標記 + 「清除 / 移除」動作。
    - `uncheckable` → 灰標記；`reason==="auth"` 時**一律**渲染「綁定憑證」按鈕（list 也要傳 `onResolveAuth`）。
    - `notInstalled` → 「可安裝」+ 「安裝」。

> **DTO 收斂**：把 `SourceSkillDiff.state` 從 `string` 收成真正的 Rust enum（`SourceSkillState` 已存在），讓 ts-rs 產出 union 而非 `string`，避免 stringly-typed 漂移。**不改 lock**。

### 4.3 自動更新檢查（D2）

- 進入「技能」頁時，若距上次檢查 > **throttle 門檻**（預設 10 分鐘，可調），靜默觸發 `GET /skills/check-updates`（目前是手動 mutation；改為進頁自動、節流）。
- 仰賴 `check_updates` 既有的 **ls-refs preflight**（`crates/skill-update/src/lib.rs` `preflight_decision`）：當每個 `(source, ref)` 群組的 `ref_commit == tip_oid` 時跳過 fetch、不下載 object，穩定態幾乎零成本。
- **接受副作用**：check 會 auto-heal lock 的 hash / `ref_commit`（這正是讓下次能 preflight 跳過的機制）。spec 明確記載「進頁自動跑 = 接受這個寫入」。
- 標題列顯示「上次檢查 X 前」(從 check 完成時間推導；存於 query cache / store)。
- toast 在 check 完成時回饋摘要：`檢查完成，N 個可更新`（無更新時「全部都是最新」）。
- 重的 `diff_source`（full fetch，120s timeout）**維持點來源才跑**（Source 視角 detail 進入時），不在進頁自動跑。
- 「依 來源」列上的「有更新」小點：由 check-updates 的 per-skill 結果按 source 聚合即可，**不需** diff。

### 4.4 更新顯示（D3）

- 去綠勾：`current` 不畫徽章。
- detail 卡的兩條更新路徑收斂：預設只露一顆**「更新」**（走 apply-update）；「換 branch / 手動 sync」收進「進階 ▾」次選（仍走 `git/sync` 的 `SyncGithubSkillDialog`）。
- 人話化：用 diff/lock 已能拿到的 `version`/`author`/`ref`/時間呈現「v1.4 → v1.5、更新於 3 天前」，hash 收進 tooltip / 進階。
    - ⚠️ **待驗證**：`SourceSkillDiff` 已帶 `version`，但 `check-updates` 的 `SkillUpdateStatusResponse::UpdateAvailable` 目前只帶 `current/available` 兩個 hash，**沒有版本/日期**。要在 Agent 視角顯示人話版本，需在 check-updates 結果補上 available 版本與上游時間（或 detail 顯示時 fallback 用 diff）。本點列為 backend 工作項與 open question。

### 4.5 來源顯示統一（重點）

把現「7 個狀態各一區塊」改成**按要不要動手分兩組**：

- 頂部一條 **summary bar**：「這個來源：N 可更新 · N 可安裝 · N 已改名 · N 無法檢查 · N 最新」。
- **「需要動作」一張卡**：所有待辦（outdated / notInstalled / renamed / removed / deprecated / uncheckable-actionable）混排，每列右側給**對應單一動作鈕**（更新 / 安裝 / 採用新名 / 清除 / 綁定憑證）；卡頂給批次鈕（全部更新 N、安裝缺少的 N）。
- **「已是最新」收合成一行**，預設收起，點開才看（取代長長的 current 區塊）。
- **全清空狀態**：無待辦時顯示安靜的「全部最新 + 對賬時間」，而非滿屏綠勾。
- **死路變一鍵**：
    - `renamed` →「採用新名」：需新增**原子的 accept-rename 流程**（刪舊 + 裝新，單一交易、失敗回滾），取代現「複製 CLI 指令去終端機」。見 5.2。
    - `uncheckable(auth)` →「綁定憑證」：`SourceCredentialBindingDialog` 已存在，需在 diff 面板**實際 mount + wire**（現 index.tsx 只渲染警告 Alert，沒掛 dialog）。
- 「N link targets / M covered」這種把 `partitionByCoverage` 內部語意暴露給使用者的字樣，改成人話（例如「將安裝到 3 個 Agent」+ hover 細節）。

## 5. 後端工作項

### 5.1 狀態與路由

- **不**強行合併兩條路由（風險高）。維持 `check-updates`（cheap path，Agent 視角 + source 小點）與 `sources/diff`（full enumeration，Source 視角 detail）；**統一只在 FE 的 union 與 `SkillStatusBadge`**。`diff ABSORBS check-updates` 列為後續清理，非本 spec day-1。
- `SourceSkillState` enum 經 ts-rs 導出為 FE union（`SourceSkillDiff.state` 去 stringly-typed）。

### 5.2 Accept-rename 原子流程（新功能，最大塊）

- 新增一個 server 操作（route + core），語意 = 「以新名安裝上游當前版本 + 刪除舊名技能 + 更新 lock」，全程交易化、任一步失敗回滾，且尊重既有 guards（containment、plugin、universal-master-referrer）。
- 取代 apply-update 對 rename 的硬拒；apply-update 維持原樣（仍 reject rename），accept-rename 是獨立明確的操作。
- ⚠️ 需確認可重用 `stage_and_swap_dir` / `install_fetched` / removal 既有元件，以及 rename 偵測來源（`detect_rename` / CHANGELOG 映射）在 apply 當下仍可取得新名。
- **CLI 對應**：`source` 子命令的 renamed 目前被 sync 排除；評估是否同步加一個 CLI accept-rename（保持 app/CLI 平價，見 `releasing-aghub` 的同步原則）。列為 open question。

### 5.3 check-updates 結果補版本/日期（4.4 人話化所需）

- 在 `UpdateAvailable` 補 available 版本與上游 commit 時間（若成本可接受）；否則 FE 在 detail 顯示時以 diff 結果 fallback。

### 5.4 憑證綁定

- diff 面板掛上 `SourceCredentialBindingDialog`（component 已存在），`onBound` 後重跑該 source 的 diff/check。

## 6. 前端工作項

- 合併 `pages/settings/skills.tsx` 與 `pages/sources/index.tsx` 成單一頁 + `View-by` 分段切換 state（URL 同步，沿用 nuqs）。
- 抽共用元件：`SkillRow`、`SkillStatusBadge`（取代 `skill-update-badge.tsx`）、install/agent-select dialog（現 `ManageSkillAgentsDialog` / Sources install path / skills.sh InstallModal 三份合一）。
- 進頁自動 check + throttle + 「上次檢查」時戳 + toast 摘要（用 React Query，**不可用 `useEffect` 取數**，依 desktop AGENTS.md）。
- Source detail 改 summary bar + 「需要動作 / 已是最新」兩組 + 全清空狀態。
- list 的 `SkillStatusBadge` 補傳 `onResolveAuth`。
- scope 切換（global/project）。
- `skills.sh` 結果列接「已安裝 / 已是某來源」狀態（讀現有 lock 查詢即可判斷）。

## 7. DTO / codegen

- 任何新 Rust DTO（accept-rename req/resp、`state` enum、check-updates 補欄位）→ ts-rs 重新產生 `crates/desktop/src/generated/dto`，並**跑 prettier 再 diff**（產生的 DTO 會被 prettier 格式化，否則會看到假的大量 diff——見專案慣例）。

## 8. 測試

- 單元：4態→7態 union 映射；`SkillStatusBadge` 各態渲染（特別是 `current`/未檢查都不畫）；throttle 邏輯。
- 整合（`crates/core/tests` / `crates/api`）：accept-rename 原子性 + 失敗回滾（用 `testing-fs-failures` 強制 fs 失敗驗證 rollback）；auto-heal 副作用；diff 憑證綁定後重算。
- CLI（若加 accept-rename）：`crates/cli/tests`。
- 回歸：npx round-trip 契約測試必須仍綠（確認沒動 lock/hash/Master 佈局）。

## 9. 風險與 Open Questions

1. **OQ1**：check-updates 要不要補版本/日期（5.3），還是 Agent 視角人話化只在 detail 用 diff fallback？（影響 4.4）
2. **OQ2**：accept-rename 要不要同步做 CLI 版（app/CLI 平價）？
3. **OQ3**：throttle 門檻（10 分鐘？）與「進頁自動連網對賬」對純離線使用者的體感——是否要一個全域開關「進頁自動檢查更新」讓使用者關掉。
4. **OQ4**：「需要動作」是否該把「清理類（removed/deprecated/renamed）」與「取得類（outdated/notInstalled）」拆兩張卡？（mockup 目前混排，使用者已認可，但保留為可調。）
5. **風險**：合併頁同時承載 Agent/Source 兩種分組 + scope 切換，狀態管理複雜度上升；務必先抽共用元件再合併，否則只是把割裂搬進同一頁。

## 10. Rollout（增量、低風險先行）

- **Phase 0**（最小、零資料模型改動）：三處 install dialog 抽共用元件；`skills.sh` 列接「已安裝」狀態；技能 detail ↔ 來源 cross-link。
- **Phase 1**（更新顯示）：去綠勾 + `SkillStatusBadge` 重寫 + 進頁自動 check + throttle + 時戳 + toast；list 補 `onResolveAuth`。
- **Phase 2**（IA 合併）：`View-by` 切換把 Sources 收成一個視角；Source detail 改 summary bar + 兩組顯示 + 全清空狀態；scope 切換。
- **Phase 3**（死路變一鍵）：accept-rename 原子流程（+ 可選 CLI）；check-updates 補版本/日期人話化；diff 面板掛憑證 dialog。

每個 Phase 可獨立出貨、獨立驗證。

## 11. 對抗性驗證結果（2026-06-20）

主迴圈直接讀碼自證（subagent 批次因 session 限額全數失敗，11pm 台北重置後可重跑驗證以加冗餘）。

### 判決總覽

| 宣稱                                                                | 判決                        | 證據                                                                                                                                                                                                                                            |
| ------------------------------------------------------------------- | --------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1 `check_updates` ls-refs preflight 便宜路；`diff_source` 才是重路 | ✅ 確認                     | `skill-update/src/lib.rs` `preflight_decision`→`PreflightResult::Skip`（tip 未動不下載 object）；`sources.rs:821 diff_source` 需 fetch tree                                                                                                     |
| C2 GET check-updates 會 auto-heal 寫 lock hash/ref_commit           | ✅ 確認                     | `skills_update.rs` check handler 寫回；lib.rs 註解「fresh fetch, so the next check can preflight」                                                                                                                                              |
| C3 4態→7態映射、`notInstalled/removed/deprecated` 只來自 diff       | ✅ 確認（命名修正）         | `sources.rs:37-56` 七變體；正確字串是 `installedCurrent/installedOutdated`（非 `current/outdated`）                                                                                                                                             |
| C4 `SourceSkillDiff.state` 為 stringly-typed，可改 union            | ✅ 確認                     | `generated/dto/SourceSkillDiff.ts:19` `state: string`；Rust `SourceSkillState` enum 已存在                                                                                                                                                      |
| C5 check-updates 無版本/日期                                        | ✅ 確認                     | `dto/skill.rs:402` `UpdateAvailable { current, available }` 僅兩 hash；`SourceSkillDiff` 才有 `version`                                                                                                                                         |
| C6 accept-rename 原子流程：元件可重用、為新工作                     | ✅ 確認                     | `core/skills/update.rs` `detect_rename`/`stage_and_swap_dir`/`sanitize_skill_path`；`core/skills/install_fetched.rs:144 install_fetched_skill_and_lock`；`skills_update.rs:584` 現以 `SKILL_RENAMED_CODE` 硬拒；已有 `api/src/skills/rename.rs` |
| C7 憑證 dialog 未掛上 sources 頁                                    | ✅ 確認                     | `pages/sources/index.tsx:839-848` 僅渲染 `Alert`(needsCredentialHint)；全檔無 `SourceCredentialBindingDialog` 參照                                                                                                                              |
| C8 skills.tsx 硬鎖 global                                           | ✅ 確認                     | `skills.tsx:38,44` `scope: "global" as const`                                                                                                                                                                                                   |
| C9 三套 install UX 為獨立元件                                       | ✅ 確認（沿用先前理解報告） | `manage-skill-agents-dialog.tsx` / sources install handler / `skills-sh/.../install-modal.tsx` 三份                                                                                                                                             |
| C10 不需動 npx 凍結契約                                             | ✅ 確認                     | 變更皆在 read-model/API/FE 層；未觸及 `lock/types.rs`/`local.rs`/`hash.rs` 的 schema 或演算法                                                                                                                                                   |
| C11 skills.sh 可判斷「已安裝」                                      | ⚠️ 部分                     | 資料可得(既有 lock + skill list 查詢)但 `skills-sh/search.tsx` 目前未載，需新增前端查詢；非後端工作                                                                                                                                             |

### 必須修正（已套入本 spec）

1. **全域「可安裝」不可行**（最關鍵）：`list_sources` 只回 `skill_count`，`notInstalled` 須逐來源 `diff_source`。全域標題摘要拿掉「可安裝」，只留可更新/需處理/最新；「可安裝」僅在單一來源 detail 的 summary bar（已修 §4.1、§4.5）。**mockup 頭部那顆「2 可安裝」要移除**。
2. **狀態字串命名**：FE union 用 `installedCurrent/installedOutdated`，非 `current/outdated`（已修 §4.2）。
3. **C11 降級**：skills.sh 接「已安裝」狀態需在 `search.tsx` 新增 installed/lock 查詢並以 (source,name) 比對——列為明確前端工作，非零成本。

### 完整性補洞（新增 Open Question / 工作項）

- **OQ5 — 與專案頁 `UnifiedResourceList` 的關係**：合併頁加 project scope 後，與專案頁同時列出 skills 的區塊重疊。需決定：合併頁的 project 視角是否取代專案頁 skills 區塊，或兩者並存（專案頁仍混合 MCP+Skill+SubAgent）。本 spec 預設**並存、不動專案頁**，僅讓合併頁能讀 project scope。
- **OQ6 — Sources 路由/深連結**：現 Sources 有獨立 route + nuqs `?source=`。合併後 `View-by=來源` + 選定來源狀態要遷到 `技能` route 下；舊 `/sources` 深連結需 redirect，避免破。
- **OQ7 — i18n**：新字串（採用新名 / 上次檢查 X 前 / summary 標籤 / 全清空狀態…）需新增 locale keys（`crates/desktop/src/lib/locales`）；盤點可重用既有 key。
- **OQ8 — 離線自動檢查**：`check_updates` 有 offline 旗標會把全部短路成 `Uncheckable{network}`。進頁自動 check 在離線時會讓全部變「無法檢查」——需偵測離線時抑制自動 check 或優雅降級（呼應 OQ3 的全域開關）。
- **跨 crate 接線**：accept-rename 新 route 需在 `crates/api` 註冊；新 DTO（accept-rename req/resp、`SourceSkillState` 導出、check-updates 補欄位）→ ts-rs 重新產生 + prettier。

### 整體裁決

**Spec 在套入上述修正後可行。** 後端宣稱的可重用元件（preflight、stage_and_swap_dir、install_fetched、rename primitives）全部存在，npx 凍結契約不受影響。唯一需要重新思考的是**全域摘要的「可安裝」**——已修正為「不在全域層呈現」。其餘為增量前端工作 + 一個中等規模的後端新功能（accept-rename 原子流程，Phase 3）。

> 註：上為 §11（主迴圈自證）裁決。後續以 sonnet ×13 subagent 重跑得到更細修正，整理於 §12；**與 §11/§4 衝突處以 §12 為準**。

## 12. 對抗性驗證 v2（subagent 重跑，2026-06-20）

### 12.1 宣稱修正

- **§12-C1（preflight 三條件）**：`check_updates` 跳過 fetch 需**三條件同時**（`skill-update/src/lib.rs:349-374`）：(1) `ref_commit==Some(tip_oid)`，(2) 已知非佔位 `stored_hash`，(3) `local_hash==stored_hash`（本地未漂移）；且 `ref_resolver: Option<...>`（`:252-258`），`None` 時一律 full fetch。→ §4.3 的「幾乎零成本」降為「**穩定態** near-zero-cost（ref_resolver 已接線、ref_commit 已 populate、本地無漂移）」。首次安裝後或本地編輯過會 full fetch，故 throttle + 離線抑制 + 全域開關（OQ3/OQ8）為**必要**而非可選。
- **§12-C3（wire 字串）**：實際是 `installedCurrent/installedOutdated`（`sources.rs:48-58`），§4.2 已修。
- **§12-C4 / GAP5（ts-rs 跨 crate）**：`SourceSkillState` 在 `skill-update`（無 ts-rs 依賴）；`api/dto/sources.rs:77` 用 `pub state: String`。導出 TS union 須二擇一，**推薦 (B) 在 `crates/api/src/dto/sources.rs` 宣告平行 enum 加 `#[derive(TS)]`**（不污染 domain crate）。
- **§12-C5（diff 也無日期）**：`SourceSkillDiff` 有 `version/author` 但**無 date**。「更新於 X 前」需在 `UpdateAvailable`（`dto/skill.rs:402`）與 `SourceSkillDiff` 兩者加 git commit-timestamp。→ **Phase 1 只做去綠勾 + 不依賴日期的「可更新」徽章；版本/日期人話化延到 Phase 3**。
- **§12-C6**：Sources rename 是兩顆按鈕（刪舊 + `writeText` 複製 install 指令到剪貼簿，`sources/index.tsx:538-601,1059-1096`），措辭精確化。
- **§12-C9（兩個非三個）**：Sources install 為 headless（自動選 agent、無 UI、單 toast，`sources/index.tsx:459-462,761-768`）。可抽共用 modal 為 `ManageSkillAgentsDialog`（CheckboxGroup）+ skills.sh `InstallModal`（TagGroup）**兩個**；本 spec 維持 Sources headless install 不改。
- **§12-C11**：`search.tsx` 目前未載 lock/skill-list（`:16-21,193-200`）；接「已安裝」徽章須新增 lock/skill-list query + `(source,name)` 比對，列為 Phase 0 工作項（非零成本）。
- **確認無誤**：C2、C7、C8、C10。

### 12.2 完整性補洞（併入工作項 / 關閉 OQ）

前端（§6）：

- **GAP1**：移除 `'sources'`（`lib/store/types.ts:43`）須加 v6→v7 store migration（`migrations/v6-to-v7.ts` + 接 `migrations/index.ts` + `CURRENT_VERSION` 6→7）。
- **GAP2**：`/sources`（`App.tsx:284`）改用既有 `components/redirect.tsx` `<Redirect to="/skills?view=source">`。
- **GAP3**：URL schema 定為 `?view=agent|source&skill=<name>&source=<key>`；切 view 清另一參數。
- **GAP4**：~40 新 i18n key 跨 **en/zh-Hant/zh-Hans**。可重用 `sourceStateOutdated`/`skillRenamedBadge`/`checkForSkillUpdates`/`needsCredential`；需新增 `skillCenter`/`viewByAgent`/`viewBySource`/`lastChecked`/`recheck`/`acceptRename`/`allUpToDate`/summary 計數。
- **GAP7**：scope widget 決定為 **global/project segment + 持久化 projects store 的 project 下拉**。
- **GAP8（重要）**：auto-check 現為 `useMutation`（`skills.ts:351`）**無法自觸發**；決定改 **`useQuery` + `staleTime`(=throttle) + 進頁 `enabled`**，不可用 `useEffect`。
- **GAP9**：`onboarding-controller.tsx:322` `nav-skills` tour 文案（`en.ts:464`）合併後更新。

後端（§5）：

- **GAP5**：ts-rs 路徑（見 §12-C4，推薦 B）。
- **GAP6**：accept-rename 新 route 須在 `crates/api/src/lib.rs` `build_rocket()` `routes![...]` 掛載（`:216-217`）。

關閉 OQ：

- **OQ5 → 已決**：專案頁 `UnifiedResourceList` 不改；check-update 徽章**只**出現在合併 `/skills` 頁，不出現在專案頁（避免兩處顯示不一致）。

### 12.3 整體裁決（v2）

套入修正後可行、無 phase 需重做。三項出貨前釘死：(1) §12-C1 零成本依賴穩定態 → throttle/離線抑制/全域開關必要；(2) §12-C5 人話化日期延到 Phase 3 並需兩個 DTO 加欄位；(3) Phase 2 IA 合併附帶 GAP1-4（store migration / redirect / nuqs schema / i18n）為原本沒寫的必做項。
