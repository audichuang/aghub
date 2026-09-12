# 01 — 匯入遠端技能時不檢查同名碰撞，遷移畫面事後要使用者在兩支無關的技能之間二選一

**Status:** open · 影響：中 · 證據等級：程式碼已驗證，現場狀態未取證

**回報**（2026-09-12，使用者的 Mac，`/Users/audi`）：匯入一支遠端技能、選好
agent 之後，遷移畫面跳出「有 1 個技能需要你處理」：

```
agent-reach  需要你處理
/Users/audi/.openclaw/skills/agent-reach holds content that differs from the
master at /Users/audi/.aghub/agent-reach
compare them, then keep the one you want:
  diff -r /Users/audi/.openclaw/skills/agent-reach /Users/audi/.aghub/agent-reach
Move the copy you do not want aside and re-run `aghub skills repair agent-reach`
```

回報者的推論是「匯入沒走正規的路」。**那個推論可以排除**（見下），但他指到的
地方確實有缺口。

---

## 已排除：匯入不會寫實體副本

- `Linker::link`（`crates/core/src/skills/linker/mod.rs:456`）在寫入前先
  `symlink_metadata` 探測槽位，遇到實體目錄或外來連結一律回
  `LinkOutcome::Conflict`，**從不覆蓋**。
- `install_fetched.rs:772` 把 conflict 那一列報成
  `installed: false` ＋ 明確訊息（"A real directory or a foreign link already
  occupies this skill slot; it was not overwritten"）。
- API 的 `success = agent_result.error.is_none()`
  （`crates/api/src/routes/skills.rs:1936`）因此是 `false`，匯入面板
  （`import-github-skill-panel.tsx`）會畫紅叉並顯示該訊息。

所以 `~/.openclaw/skills/agent-reach` 這個**內容不同的實體目錄**，幾乎可以確定
不是這次匯入放進去的——最可能是 openclaw 本來就自帶／使用者自己裝的同名技能。

## 缺口一：匯入前完全沒有同名碰撞檢查

`install_fetched.rs` 裡沒有任何 already-exists / collision 的前置檢查
（grep `already_exists|collision|existing skill` → 無命中）。流程是「先抓、先
建 Master、再逐個 agent 連結」，碰撞只在**連結那一刻**才被發現。

後果：

- 使用者在選 agent 的畫面上，看不到「openclaw 已經有一支同名但不同內容的技能」。
- 如果他**沒有**勾選 openclaw，匯入全綠，但 `.aghub` 裡已經多了一個與 openclaw
  自有技能撞名的 Master——問題被推遲到遷移畫面才爆。

## 缺口二：`readers_of` 把「碰巧同名的別人的技能」算成這個 Master 的讀者

`shape::readers_of` 判定「讀得到」的依據是該 agent 的 read dir 底下有沒有一個
同名資料夾帶根 `SKILL.md`。openclaw 自有的 `agent-reach` 完全符合，於是：

1. openclaw 被算成 Master 的讀者 → 進 `grant_to`；
2. `classify_shape` 看到槽位是實體目錄、Master 也存在 → `ForkedCopy`；
3. `plan_repair` → `CompareThenQuarantine` → **整份 plan 被 Refuse**。

也就是說：aghub 從來沒有把技能裝到 openclaw，卻要求使用者對 openclaw 自己的
技能做處置，而且**一支擋住整個 repair**。

這與 `aghub-skill-name-collision-false-fork` 是同一類（把不相干的東西當成分身），
差別在那次撞的是分類資料夾，這次撞的是一支真的同名技能。

## 缺口三：文案把「別人的技能」講成「你的技能的分身」

`crates/core/src/skills/repair.rs:392` 的
`compare them, then keep the one you want` 預設兩邊是同一支技能的兩個版本。
在碰撞情境下照做（把 openclaw 那份搬走）＝**弄掉使用者本來就在用的另一支技能**。

---

## 現場證據已滅失（2026-09-12 更新）

原本這一節要回報者用目錄 mtime 判斷來源。**那條路已經走不通了**：
`~/.openclaw/skills/agent-reach` 現在是一條指向 Master 的 symlink，時間戳
`2026-09-12 13:44:23`——那是後來 repair／重裝留下的，不是原本那個實體目錄的
時間。原始的分歧副本沒有留存，**不能再用 mtime 反推當初是誰寫的，也不能據此
升 P0**。

所以「匯入是否曾經寫過實體副本」這個分歧點，目前**無法從那台機器收斂**。
程式碼側的排除仍然成立（`Linker::link` 遇到實體目錄一律回 `Conflict` 且不覆蓋，
`install_fetched.rs:772` 報 `installed: false`，API `success: false`），但那是
推論不是現場佐證。

還可能discriminate的證據，都與那台機器的當下狀態無關：

- **openclaw 上游本身有沒有一支叫 `agent-reach` 的技能**。有 → 撞名成立，本
  issue 如下文；沒有 → 那個實體目錄只可能來自別處，排除推論要重驗。這是目前
  最乾淨、且可離線查證的一條。
- lock 檔（global 與 project 的 `skills-lock.json`）裡 `agent-reach` 的
  entry 與 source。
- repair 若曾 quarantine 過，store 旁會留下被搬開的目錄。

在這之前，**本 issue 的三個缺口與撞名情境無關地都成立**：匯入前沒有同名碰撞
檢查、`readers_of` 會把同名的別人技能算成 Master 的讀者、文案二選一會叫人搬走
別人的東西。這三點不需要先釐清來源就能修。

## 修法方向（建議，不是結論）

1. **匯入前做碰撞檢查並在選 agent 的畫面上呈現**：對每個候選 agent 的 read dir
   探同名項目，區分「已經是指向本 Master 的 Referrer」（無事）、「同名但不同內容的
   實體目錄」（要警告）、「外來連結」（要警告）。這是唯一能在使用者**做決定之前**
   告訴他的時機。
2. **讓 repair 能分辨「撞名」與「分身」**。可用訊號：該路徑在不在 lock 裡、
   `SKILL.md` 的 frontmatter `name` 與 source 是否對得上。撞名不該進 `grant_to`，
   也不該擋住整份 plan——它應該是一列 informational，不是 refusal。
3. **文案分兩套**：真分身講「比對後二選一」；撞名要講「這是 <agent> 自己的技能，
   aghub 不會動它；你匯入的那支要換個名字，或明確覆蓋」。
4. 順帶：`success = error.is_none()` 沒看 `installed`。今天不可達
   （`symlink_dirs` 與 `referrer_dir` 是同一批 `PathBuf`，拼法一致），但這個耦合
   只要哪天 dirs 被正規化過就會變成靜默假成功，值得順手釘一條測試。

## 不在範圍內

`readers_of` 的三態語意（`Present`/`Absent`/`Unknown`）與 `ForkedCopy` 排在
`ForeignDir` 之後的順序都是刻意的，有各自的回歸測試，別動。

## Comments
