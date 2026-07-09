# Source Group 批次管理代理 Implementation Plan

> **For agentic workers:** 用 superpowers:executing-plans / subagent-driven-development 逐 task 實作。Steps 用 `- [ ]`。

**Goal:** 在「依 Agent」view 的 source group header 加一顆按鈕,一鍵批次管理該 source 底下**所有 skill** 的 agent 綁定——一次「加」或「移除」某(些) agent 到整組,不用逐個 skill 點「管理代理」。

**Architecture:** 純函式算「每個 agent 在這組的裝機三態(全/部分/無)」→ 新 dialog 用三態 checkbox 讓使用者選目標狀態 → 套用時對 group 內每個 skill 算 add/remove diff、逐 skill 呼叫既有 `reconcileSkillsMutationOptions`、顯示 X/Y 進度;有移除先確認。

**Tech Stack:** React 19 + HeroUI v3 + TanStack Query;測試 node:test(`bun run --cwd crates/desktop test`)。

## Global Constraints

- bun only;hard tab;className 用 `cn`。
- 移除是破壞性 → 套用前若有 remove 必須 AlertDialog 確認。
- 冪等:已裝的 agent 不重複 add、沒裝的不 remove。
- 複用既有 `reconcileSkillsMutationOptions`(`crates/desktop/src/requests/skills.ts`)——它已修成不卡(invalidate 不連帶等 source-diff)。
- 每 task 結尾 `bun run --cwd crates/desktop typecheck`;Task 1 另跑 `test`。

## 資料結構(已確認)

- `SourceGroup { source, sourceType, skills: SkillGroup[] }`(skill-list.tsx 內部型別)
- `SkillGroup { name, items: SkillResponse[] }`(`components/skill-detail-helpers.ts`);`items[i].agent` = 該 skill 裝在哪個 agent、`items[i].source` = "global"/"project"。
- 「agent X 在這組裝了幾個」= `skills.filter(sg => sg.items.some(it => it.agent === X)).length`。
- reconcile 單 skill 簽名(見 `reconcileSkillsMutationOptions` / `ReconcileRequest`):`{ source: { agent, scope, project_root, name }, added: string[]|null, removed: string[]|null }`。

---

## Task 1: 純函式 `groupAgentPlan` + 測試

**Files:**

- Create: `crates/desktop/src/lib/group-agent-plan.ts`
- Test: `crates/desktop/src/lib/group-agent-plan.test.ts`

**Interfaces (Produces):**

```ts
export type TriState = "all" | "some" | "none";
export interface GroupAgentStat {
	agentId: string;
	installed: number;
	total: number;
	state: TriState;
}
export interface SkillReconcilePlan {
	name: string;
	sourceAgent: string;
	scope: "global" | "project";
	added: string[];
	removed: string[];
}

// 每個 usable agent 對這組的裝機三態
export function computeGroupAgentStats(
	skills: { name: string; items: { agent: string }[] }[],
	usableAgentIds: string[],
): GroupAgentStat[];

// 依「使用者勾選的目標 agent 集合」算出每個 skill 要 add/remove 哪些 agent。
// desired = 勾選(想要整組都有);未在 desired 的 usable agent = 想要整組都沒有。
// 只回傳有變動(added 或 removed 非空)的 skill。
export function buildReconcilePlans(
	skills: { name: string; items: { agent: string; source: string }[] }[],
	usableAgentIds: string[],
	desired: Set<string>,
): SkillReconcilePlan[];
```

- [ ] **Step 1: 失敗測試**(`group-agent-plan.test.ts`,node:test)

```ts
import { test } from "node:test";
import assert from "node:assert/strict";
import {
	computeGroupAgentStats,
	buildReconcilePlans,
} from "./group-agent-plan";

const skills = [
	{
		name: "a",
		items: [
			{ agent: "claude", source: "global" },
			{ agent: "codex", source: "global" },
		],
	},
	{ name: "b", items: [{ agent: "claude", source: "global" }] },
];

test("computeGroupAgentStats: all/some/none", () => {
	const stats = computeGroupAgentStats(skills, [
		"claude",
		"codex",
		"antigravity",
	]);
	const by = Object.fromEntries(stats.map((s) => [s.agentId, s.state]));
	assert.equal(by.claude, "all"); // 2/2
	assert.equal(by.codex, "some"); // 1/2
	assert.equal(by.antigravity, "none"); // 0/2
});

test("buildReconcilePlans: add missing, remove deselected, skip idempotent", () => {
	// desired = {claude, antigravity}: codex 取消(從 a 移除)、antigravity 勾選(a、b 都加)、claude 已全裝(不動)
	const plans = buildReconcilePlans(
		skills,
		["claude", "codex", "antigravity"],
		new Set(["claude", "antigravity"]),
	);
	const a = plans.find((p) => p.name === "a");
	const b = plans.find((p) => p.name === "b");
	assert.deepEqual(a?.added?.sort(), ["antigravity"]);
	assert.deepEqual(a?.removed?.sort(), ["codex"]);
	assert.deepEqual(b?.added?.sort(), ["antigravity"]);
	assert.equal(b?.removed?.length ?? 0, 0);
});

test("buildReconcilePlans: no change → empty", () => {
	const plans = buildReconcilePlans(
		skills,
		["claude", "codex"],
		new Set(["claude", "codex"]),
	);
	// claude 全裝、codex desired 但 b 沒裝 → b 要加 codex;a 不變
	assert.ok(plans.every((p) => p.added.length + p.removed.length > 0));
});
```

- [ ] **Step 2: 跑確認 FAIL** — `bun run --cwd crates/desktop test`

- [ ] **Step 3: 實作** `group-agent-plan.ts`

```ts
export type TriState = "all" | "some" | "none";

export interface GroupAgentStat {
	agentId: string;
	installed: number;
	total: number;
	state: TriState;
}

export interface SkillReconcilePlan {
	name: string;
	sourceAgent: string;
	scope: "global" | "project";
	added: string[];
	removed: string[];
}

export function computeGroupAgentStats(
	skills: { name: string; items: { agent: string }[] }[],
	usableAgentIds: string[],
): GroupAgentStat[] {
	const total = skills.length;
	return usableAgentIds.map((agentId) => {
		const installed = skills.filter((s) =>
			s.items.some((it) => it.agent === agentId),
		).length;
		const state: TriState =
			installed === 0 ? "none" : installed === total ? "all" : "some";
		return { agentId, installed, total, state };
	});
}

export function buildReconcilePlans(
	skills: { name: string; items: { agent: string; source: string }[] }[],
	usableAgentIds: string[],
	desired: Set<string>,
): SkillReconcilePlan[] {
	const plans: SkillReconcilePlan[] = [];
	for (const skill of skills) {
		const installedAgents = new Set(skill.items.map((it) => it.agent));
		const added = usableAgentIds.filter(
			(id) => desired.has(id) && !installedAgents.has(id),
		);
		const removed = usableAgentIds.filter(
			(id) => !desired.has(id) && installedAgents.has(id),
		);
		if (added.length === 0 && removed.length === 0) continue;
		// reconcile 需要一個既有安裝當 source（拿其 agent/scope）；用第一個 item。
		const primary = skill.items[0];
		plans.push({
			name: skill.name,
			sourceAgent: primary?.agent ?? "claude",
			scope: primary?.source === "project" ? "project" : "global",
			added,
			removed,
		});
	}
	return plans;
}
```

- [ ] **Step 4: 跑確認 PASS** — `bun run --cwd crates/desktop test`
- [ ] **Step 5: typecheck + commit**

```bash
bun run --cwd crates/desktop typecheck
git add crates/desktop/src/lib/group-agent-plan.ts crates/desktop/src/lib/group-agent-plan.test.ts
git commit -m "feat(desktop): 加 group-agent-plan 純函式（批次代理三態 + reconcile diff）"
```

---

## Task 2: `BulkManageGroupAgentsDialog` 元件

**Files:**

- Create: `crates/desktop/src/components/bulk-manage-group-agents-dialog.tsx`

**Interfaces (Consumes):** Task 1 的 `computeGroupAgentStats`/`buildReconcilePlans`;既有 `useAgentAvailability`、`useApi`、`useQueryClient`、`reconcileSkillsMutationOptions`、`supportsSkillMutation`(`lib/agent-capabilities`)、HeroUI `Checkbox`(三態用 `isIndeterminate`)、`AlertDialog`、`toast`、`SharedSkillInstallModal`(可參考 `manage-skill-agents-dialog.tsx` 的 modal 殼)。

**Props:**

```ts
interface BulkManageGroupAgentsDialogProps {
	source: string;
	skills: { name: string; items: { agent: string; source: string }[] }[];
	scope: "global" | "project";
	projectPath?: string;
	isOpen: boolean;
	onClose: () => void;
}
```

- [ ] **Step 1: 建立元件**（要點,參考 `manage-skill-agents-dialog.tsx` 既有寫法）
    - `usableAgentIds` = `availableAgents.filter(a => a.isUsable && supportsSkillMutation(a, scope)).map(a => a.id)`。
    - `stats = computeGroupAgentStats(skills, usableAgentIds)`。
    - `desired: Set<string>` state,初始 = state==="all" 的 agent(其餘視為未勾;"some" 初始不勾,使用者可勾成 all 或不動)。**初始 desired 應等於目前狀態**,所以「some」初始要呈現 indeterminate 且不列入 desired、也不列入「要移除」——用一個 `initial` 快照:只有使用者「改動過」的 agent 才進 add/remove。
    - **更精確的模型**:維護 `desired: Map<agentId, boolean>`,初始只放使用者按過的;render 時 checkbox 的 `isSelected`/`isIndeterminate` 由「該 agent 是否在 desired 有明確值,否則看 stats.state」決定。套用時:`buildReconcilePlans` 的 `desired` set = 對每個 usable agent 決議最終目標(使用者按過的用其值;沒按過的:state==="all"→保留 true、否則 false 但**不觸發移除**——即沒按過的 "some"/"none" 維持原樣不動)。
    - 為避免誤刪:**只有使用者明確取消勾選(把一個 all/some 變成 unchecked)才 remove**。實作:`toucted: Set<agentId>`;`buildReconcilePlans` 只對 `touched` 的 agent 計 add/remove,未 touched 的 agent 完全不動(既不加也不移)。這比純 desired-set 安全。
    - 調整 Task 1 `buildReconcilePlans`:改吃 `touched: Set` + `desired: Set`,或在 dialog 端先算出「最終每個 skill 的 add/remove」——**以 dialog 端組 plan、Task 1 純函式只做 stats + 給定 desired 的 diff** 為準;dialog 只把 touched 的 agent 併入 desired/去除,未 touched 的 agent 用其目前 all→desired、其餘→not desired 但過濾掉「未 touched 且會造成 remove」的項。
    - 套用:`plans = buildReconcilePlans(...)`;若任何 plan.removed 非空 → 先 `AlertDialog` 確認。確認後 loop plans,每個 `reconcileMutation.mutateAsync({ source: { agent: p.sourceAgent, scope: p.scope, project_root: projectPath ?? null, name: p.name }, added: p.added.length?p.added:null, removed: p.removed.length?p.removed:null })`;累計成功/失敗 + 顯示 `X/Y`。
    - 進度 state `done`;confirm 鈕文字 `套用中… done/plans.length`(參考 source-detail 的批次進度寫法)。
    - 完成 toast(成功數/失敗數)後 `onClose`。
    - i18n:新增 key(zh-Hant/zh-Hans/en):`bulkManageGroupAgents`(標題)、`bulkAgentsApply`、`bulkAgentsApplying`、`bulkAgentsConfirmRemoveTitle`、`bulkAgentsConfirmRemoveBody`、`bulkAgentsDone`(已更新 {{count}} 個 skill)、`bulkAgentsSomeFailed`。

- [ ] **Step 2: typecheck** — `bun run --cwd crates/desktop typecheck`
- [ ] **Step 3: commit** — `git commit -m "feat(desktop): 新增 BulkManageGroupAgentsDialog 批次管理整組 skill 的代理"`

---

## Task 3: skill-list group header 加按鈕 + callback

**Files:** Modify `crates/desktop/src/components/skill-list.tsx`

- [ ] **Step 1:** `SkillListProps` 加 `onManageGroupAgents?: (group: SourceGroup) => void;`。
- [ ] **Step 2:** group header(skill-list.tsx ~336 那顆展開 button 所在的 row)——把展開 button 與一顆新的 icon button 併排(header 外層改成 `flex items-center`,展開 button `flex-1`,右側加 icon button)。icon 用 `@heroicons/react/24/outline` 的 `UserGroupIcon` 或 `UsersIcon`;`onClick`(stopPropagation,避免觸發展開)呼叫 `onManageGroupAgents?.(sg)`。只在 `onManageGroupAgents` 有傳時 render。保留既有 `[transform:translateZ(0)]` 滾動容器不動。
- [ ] **Step 3: typecheck + commit** — `git commit -m "feat(desktop): skill-list source group header 加批次管理代理按鈕"`

---

## Task 4: SkillsPage 接線

**Files:** Modify `crates/desktop/src/pages/settings/skills.tsx`

- [ ] **Step 1:** import `BulkManageGroupAgentsDialog` + `SourceGroup` 型別(若未 export 則從 skill-list export)。
- [ ] **Step 2:** state `const [bulkAgentsGroup, setBulkAgentsGroup] = useState<SourceGroup | null>(null);`。
- [ ] **Step 3:** 傳 `onManageGroupAgents={setBulkAgentsGroup}` 給 agent view 的 `<SkillList ... selectionMode="multiple" ... />`(802 那個)。
- [ ] **Step 4:** render dialog:

```tsx
{
	bulkAgentsGroup && (
		<BulkManageGroupAgentsDialog
			isOpen={!!bulkAgentsGroup}
			source={bulkAgentsGroup.source}
			skills={bulkAgentsGroup.skills.map((sg) => ({
				name: sg.name,
				items: sg.items.map((it) => ({
					agent: it.agent,
					source: it.source,
				})),
			}))}
			scope={scope}
			projectPath={
				scope === "project"
					? (selectedProjectPath ?? undefined)
					: undefined
			}
			onClose={() => setBulkAgentsGroup(null)}
		/>
	);
}
```

(`it.source`/`it.agent` 欄位名以 `SkillResponse` 實際型別為準,typecheck 會抓。)

- [ ] **Step 5: typecheck + commit** — `git commit -m "feat(desktop): skills 頁接線 source group 批次管理代理"`

---

## Task 5: 全量驗證

- [ ] `bun run --cwd crates/desktop typecheck`
- [ ] `bun run --cwd crates/desktop test`
- [ ] `(cd crates/desktop && bunx eslint src --max-warnings=0)`
- [ ] `bunx prettier --check "crates/desktop/src/**/*.{ts,tsx}"`
- [ ] `bun run --cwd crates/desktop build`
- [ ] 手動煙測(使用者):展開 mattpocock/skills → 點 group header 批次按鈕 → 勾 Antigravity → 套用 → 整組都加上;取消某 agent → 確認 → 整組移除;過程有 X/Y 進度、不卡。

## Self-Review

- 覆蓋:group 一鍵(Task 3/4)、加+移除(Task 1/2 diff + 確認)、三態(computeGroupAgentStats)、冪等(buildReconcilePlans 跳過無變動)、進度(Task 2)、誤刪保護(touched 模型 + 移除確認)。
- 型別一致:`SkillReconcilePlan`/`computeGroupAgentStats`/`buildReconcilePlans` Task 1 定義、Task 2 消費。
- 風險:reconcile 的 `source.agent` 需為該 skill「已存在的一個安裝」;buildReconcilePlans 用 `items[0]` 當 source primary,若某 skill 的 items 為空(理論不會,group 至少一個安裝)則跳過該 skill。
