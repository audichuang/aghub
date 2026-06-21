# Phase 2 — IA Merge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ] ) syntax for tracking.

**Goal:** Merge the separate Skills and Sources pages into one unified "技能" page with a View-by Agent/Source toggle, add scope switching, wire the SourceCredentialBindingDialog, purge the dead `sources` sidebar entry with a store migration, redirect `/sources` → `/skills?view=source`, and add all required i18n keys and onboarding copy updates.

**Architecture:** The unified page lives at `pages/settings/skills.tsx` (file kept, heavily rewritten); `pages/sources/index.tsx` is deleted after its detail logic is extracted into `components/source-detail.tsx`. A single `SkillRow` shared component is extracted from `sources/index.tsx`. URL state is managed by nuqs (`?view`, `?skill`, `?source`); scope state is a segment control backed by the persisted projects store already used in the old SourcesPage.

**Tech Stack:** React 19 / HeroUI v3 / Tailwind v4 / TanStack Query (`useQuery` + staleTime throttle — NEVER useEffect for data) / nuqs URL params / Tauri `@tauri-apps/plugin-store` migrations / ts-rs generated DTOs / bun

---

## ⚠️ Codex 審查修正（實作前必讀；覆寫下方對應步驟）

> GPT-5.5 對著真實程式碼審查後的必改項。判定：**needs-rework**（多個編譯級錯誤）。已確認 OK：v6→v7 migration 形狀符合現有 chain、`sourcesListQueryOptions`/`sourceDiffQueryOptions`/`queryKeys.skills.pruneLock`/`queryKeys.skills.sources.all()` 都存在。

- **[P0] `isDisabled` 用在原生 `<button>`（~:1914）**：不能 type-check（strict TS）。改成 `disabled={projects.length === 0}` 並自行加 disabled 樣式。
- **[P0] `updateStatuses` 取出未用（~:1960）**：`noUnusedParameters` 會編譯失敗。要嘛實作 spec 的來源小點（傳 `sourceHasUpdatesKeys: ReadonlySet<string>`，`sourceHasUpdatesKeys.has(key)` 時畫點），要嘛移除該 prop。
- **[P0] `activeSourceRow` 計算未用（~:2294）**：刪掉該區塊，只留後面 resolved 的 source row。
- **[P0] `source-detail.tsx` import 未用（~:404）**：`ArrowUpCircleIcon`/`QuestionMarkCircleIcon`/`Checkbox`/`ImportGithubSkillPanel` 若最終沒用到就移除（`noUnusedLocals`）。
- **[Cross-cutting] HeroUI Chip 顏色**：同 Phase 1——badge/狀態 chip 用 `color="default"` + Tailwind text 類別，非 `color="warning"/"success"/"secondary"`。
- **[P1] redirect 丟失 `?source=`（~:222）**：靜態 `<Redirect to>`（`redirect.tsx:57`）會吃掉 Phase 0 的跨連結。改做 `SourcesRedirect`：讀 `window.location.search`、保留 `source`、導向 `` `/skills?view=source&source=${encodeURIComponent(source)}` ``。
- **[P1] 自動 check 永遠 enabled（~:2162）**：§12 要求 throttle + 離線抑制。改 `enabled: navigator.onLine && autoCheckEnabled` 並保留 `staleTime`。
- **[P2] 憑證 dialog 綁定值（~:1246）**：傳 `row.source`（normalized owner/repo）會讓 host 預填為空。改 `bindingSource={row.sourceUrl || row.source}` + `defaultCredentialHost={safeHost(row.sourceUrl)}`（dialog 從 `bindingSource` 解 host，`source-credential-binding-dialog.tsx:21`）。

## File Structure

| File                                                      | Status     | Responsibility                                                                                                                                                                                                                                                                                                                                                                                     |
| --------------------------------------------------------- | ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/desktop/src/lib/store/types.ts`                   | **Modify** | Remove `'sources'` from `SIDEBAR_ITEM_IDS`; bump `CURRENT_VERSION` 6→7                                                                                                                                                                                                                                                                                                                             |
| `crates/desktop/src/lib/store/migrations/v6-to-v7.ts`     | **Create** | Drop `'sources'` from persisted `sidebarItems` array                                                                                                                                                                                                                                                                                                                                               |
| `crates/desktop/src/lib/store/migrations/index.ts`        | **Modify** | Import `migrateV6ToV7`; add `if (version < 7)` branch                                                                                                                                                                                                                                                                                                                                              |
| `crates/desktop/src/lib/sidebar-navigation.ts`            | **Modify** | Remove `sources` entry from `SIDEBAR_ITEM_DEFINITIONS`                                                                                                                                                                                                                                                                                                                                             |
| `crates/desktop/src/App.tsx`                              | **Modify** | Replace `/sources` `<SourcesPage>` route with `<Redirect to="/skills?view=source">`                                                                                                                                                                                                                                                                                                                |
| `crates/desktop/src/pages/sources/index.tsx`              | **Delete** | Source of truth merged into unified skills page                                                                                                                                                                                                                                                                                                                                                    |
| `crates/desktop/src/pages/settings/skills.tsx`            | **Modify** | Unified page: view toggle (agent/source), scope switch, auto-check query, orchestrates sub-components                                                                                                                                                                                                                                                                                              |
| `crates/desktop/src/components/source-detail.tsx`         | **Create** | Extracted `SourceDetail` component (summary bar + "needs action" card + collapsed "up to date" group + credential dialog mount)                                                                                                                                                                                                                                                                    |
| `crates/desktop/src/components/source-skill-row.tsx`      | **Create** | Shared `SourceSkillRow` extracted from old `sources/index.tsx` `SkillRow`                                                                                                                                                                                                                                                                                                                          |
| `crates/desktop/src/lib/locales/en.ts`                    | **Modify** | ~25 new i18n keys (skillCenter, viewByAgent, viewBySource, lastChecked, recheck, acceptRename, allUpToDate, needsAction, summaryUpdatable, summaryInstallable, summaryRenamed, summaryUnchecked, summaryLatest, scopeSwitchGlobal, scopeSwitchProject, selectProject, sourceNeedsAction, sourceAllLatest, sourceUpdateAll, sourceInstallMissing, credentialBind, sourceHasUpdates, sourceDetail\*) |
| `crates/desktop/src/lib/locales/zh-Hant.ts`               | **Modify** | Same new keys, Traditional Chinese translations                                                                                                                                                                                                                                                                                                                                                    |
| `crates/desktop/src/lib/locales/zh-Hans.ts`               | **Modify** | Same new keys, Simplified Chinese translations                                                                                                                                                                                                                                                                                                                                                     |
| `crates/desktop/src/components/onboarding-controller.tsx` | **Modify** | Update `onboardingSkillsDescription` tour step to mention Agent/Source toggle                                                                                                                                                                                                                                                                                                                      |

---

## Task 1: Store migration — drop `'sources'` sidebar item

**Files:**

- Modify: `crates/desktop/src/lib/store/types.ts`
- Create: `crates/desktop/src/lib/store/migrations/v6-to-v7.ts`
- Modify: `crates/desktop/src/lib/store/migrations/index.ts`
- Modify: `crates/desktop/src/lib/sidebar-navigation.ts`

- [ ] **Step 1.1 — Edit `types.ts`: remove `'sources'`, bump version**

    Open `crates/desktop/src/lib/store/types.ts`. Make two changes:
    1. Change `SIDEBAR_ITEM_IDS` from:

    ```ts
    export const SIDEBAR_ITEM_IDS = [
    	"mcp",
    	"inferenceProviders",
    	"skills",
    	"skillsSh",
    	"subAgents",
    	"plugins",
    	"sources",
    ] as const;
    ```

    to:

    ```ts
    export const SIDEBAR_ITEM_IDS = [
    	"mcp",
    	"inferenceProviders",
    	"skills",
    	"skillsSh",
    	"subAgents",
    	"plugins",
    ] as const;
    ```

    2. Change `CURRENT_VERSION` from `6` to `7`:

    ```ts
    export const CURRENT_VERSION = 7;
    ```

- [ ] **Step 1.2 — Create migration file `v6-to-v7.ts`**

    Create `crates/desktop/src/lib/store/migrations/v6-to-v7.ts`:

    ```ts
    import type { Store } from "@tauri-apps/plugin-store";

    export async function migrateV6ToV7(store: Store): Promise<void> {
    	const sidebarItems =
    		await store.get<Array<{ id: string; visible: boolean }>>(
    			"sidebarItems",
    		);
    	if (!sidebarItems) return;

    	const next = sidebarItems.filter((item) => item.id !== "sources");
    	await store.set("sidebarItems", next);
    }
    ```

- [ ] **Step 1.3 — Wire migration into `migrations/index.ts`**

    Edit `crates/desktop/src/lib/store/migrations/index.ts` — add the import and the branch:

    ```ts
    import type { Store } from "@tauri-apps/plugin-store";
    import { CURRENT_VERSION } from "../types";
    import { migrateV0ToV1 } from "./v0-to-v1";
    import { migrateV1ToV2 } from "./v1-to-v2";
    import { migrateV2ToV3 } from "./v2-to-v3";
    import { migrateV3ToV4 } from "./v3-to-v4";
    import { migrateV4ToV5 } from "./v4-to-v5";
    import { migrateV5ToV6 } from "./v5-to-v6";
    import { migrateV6ToV7 } from "./v6-to-v7";

    export async function migrate(store: Store): Promise<void> {
    	const version = (await store.get<number>("version")) ?? 0;

    	if (version === CURRENT_VERSION) return;

    	if (version < 1) {
    		await migrateV0ToV1(store);
    	}

    	if (version < 2) {
    		await migrateV1ToV2(store);
    	}

    	if (version < 3) {
    		await migrateV2ToV3(store);
    	}

    	if (version < 4) {
    		await migrateV3ToV4(store);
    	}

    	if (version < 5) {
    		await migrateV4ToV5(store);
    	}

    	if (version < 6) {
    		await migrateV5ToV6(store);
    	}

    	if (version < 7) {
    		await migrateV6ToV7(store);
    	}

    	await store.set("version", CURRENT_VERSION);
    	await store.save();
    }
    ```

- [ ] **Step 1.4 — Remove `sources` from `sidebar-navigation.ts`**

    In `crates/desktop/src/lib/sidebar-navigation.ts`:

    Remove the `sources` entry from `SIDEBAR_ITEM_DEFINITIONS`:

    ```ts
    // DELETE this block:
    sources: {
    	id: "sources",
    	labelKey: "sources",
    	href: "/sources",
    	icon: GlobeAltIcon,
    },
    ```

    Also remove `GlobeAltIcon` from the heroicons import if it is only used by the `sources` entry (check — it is only there for `sources`).

    ASSUMPTION: `GlobeAltIcon` is only imported for the `sources` sidebar entry; it is still used inside `pages/sources/index.tsx` and `components/source-detail.tsx` (the new file), so verify it is not referenced in `sidebar-navigation.ts` elsewhere before removing the import.

- [ ] **Step 1.5 — Verify TypeScript compiles with removed `sources`**

    ```bash
    cd crates/desktop && bun run build 2>&1 | head -30
    ```

    Expected: build succeeds (zero TS errors related to `SidebarItemId` or `sources`). The `SourcesPage` import in `App.tsx` will fail — that is intentional and fixed in Task 2.

- [ ] **Step 1.6 — Commit**

    ```bash
    git add crates/desktop/src/lib/store/types.ts \
            crates/desktop/src/lib/store/migrations/v6-to-v7.ts \
            crates/desktop/src/lib/store/migrations/index.ts \
            crates/desktop/src/lib/sidebar-navigation.ts
    git commit -m "$(cat <<'EOF'
    feat(desktop/store): drop sources sidebar item, migrate store v6→v7

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Task 2: Route — redirect `/sources` to `/skills?view=source`

**Files:**

- Modify: `crates/desktop/src/App.tsx`

- [ ] **Step 2.1 — Replace `SourcesPage` route with `Redirect`**

    In `crates/desktop/src/App.tsx`:
    1. Remove the import:

    ```ts
    import SourcesPage from "./pages/sources";
    ```

    2. Replace the `/sources` route block (lines 284-297):

    ```tsx
    // OLD — delete:
    <Route path="/sources">
    	<MainLayout>
    		<ErrorBoundary>
    			<Suspense fallback={<SkillsPageSkeleton />}>
    				<SourcesPage />
    			</Suspense>
    		</ErrorBoundary>
    	</MainLayout>
    </Route>
    ```

    with:

    ```tsx
    <Route path="/sources">
    	<Redirect to="/skills?view=source" />
    </Route>
    ```

    The `Redirect` component is already imported at line 15. No new imports are needed.

- [ ] **Step 2.2 — Verify build compiles**

    ```bash
    cd crates/desktop && bun run build 2>&1 | head -30
    ```

    Expected: no errors from the removed `SourcesPage` import.

- [ ] **Step 2.3 — Commit**

    ```bash
    git add crates/desktop/src/App.tsx
    git commit -m "$(cat <<'EOF'
    feat(desktop/routes): redirect /sources to /skills?view=source

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Task 3: Shared `SourceSkillRow` component

**Files:**

- Create: `crates/desktop/src/components/source-skill-row.tsx`

This is a straight extraction from the `SkillRow` function in `pages/sources/index.tsx` (lines 1395-1477). The interface and implementation are copied verbatim; only the import paths change.

- [ ] **Step 3.1 — Create `source-skill-row.tsx`**

    Create `crates/desktop/src/components/source-skill-row.tsx`:

    ```tsx
    import { Chip, Checkbox } from "@heroui/react";
    import { useTranslation } from "react-i18next";
    import type { SourceSkillDiff } from "../generated/dto";
    import { cn } from "../lib/utils";

    export interface SourceSkillRowProps {
    	skill: SourceSkillDiff;
    	isExpanded: boolean;
    	onToggle: () => void;
    	action?: React.ReactNode;
    	isSelected?: boolean;
    	onToggleSelected?: () => void;
    	isSelectionDisabled?: boolean;
    	muted?: boolean;
    	showReason?: boolean;
    }

    export function SourceSkillRow({
    	skill,
    	isExpanded,
    	onToggle,
    	action,
    	isSelected = false,
    	onToggleSelected,
    	isSelectionDisabled = false,
    	muted = false,
    	showReason = false,
    }: SourceSkillRowProps) {
    	const { t } = useTranslation();
    	const detailText = skill.description || skill.skillPath;

    	return (
    		<li className="flex items-center gap-3 border-b border-border px-3 py-2.5 last:border-b-0 hover:bg-surface-secondary/70">
    			{onToggleSelected && (
    				<Checkbox
    					value={skill.skillPath}
    					isSelected={isSelected}
    					isDisabled={isSelectionDisabled}
    					onChange={() => onToggleSelected()}
    					variant="secondary"
    					aria-label={t("sourceSelectSkill", {
    						name: skill.name,
    					})}
    					className="shrink-0"
    				>
    					<Checkbox.Control>
    						<Checkbox.Indicator />
    					</Checkbox.Control>
    				</Checkbox>
    			)}
    			<button
    				type="button"
    				className="min-w-0 flex-1 text-left"
    				aria-expanded={isExpanded}
    				onClick={onToggle}
    			>
    				<div className="flex min-w-0 items-center gap-2">
    					<span
    						className={cn(
    							"truncate text-sm font-medium",
    							muted ? "text-muted" : "text-foreground",
    						)}
    					>
    						{skill.name}
    					</span>
    					{skill.version && (
    						<Chip size="sm" variant="secondary">
    							v{skill.version}
    						</Chip>
    					)}
    					<span className="truncate font-mono text-[11px] text-muted/80">
    						{skill.skillPath}
    					</span>
    				</div>
    				{detailText && (
    					<p
    						className={cn(
    							"mt-0.5 text-xs leading-5 text-muted",
    							!isExpanded && "line-clamp-1",
    						)}
    					>
    						{detailText}
    					</p>
    				)}
    				{showReason && skill.reason && (
    					<p className="mt-0.5 text-xs text-muted">
    						{skill.reason}
    					</p>
    				)}
    				{skill.state === "renamed" && skill.previousName && (
    					<p className="mt-0.5 text-xs text-warning">
    						{t("sourceRenamedHint", {
    							oldName: skill.previousName,
    							newName: skill.name,
    						})}
    					</p>
    				)}
    			</button>
    			{action && <div className="shrink-0">{action}</div>}
    		</li>
    	);
    }
    ```

- [ ] **Step 3.2 — Verify TypeScript compiles for the new file**

    ```bash
    cd crates/desktop && bun run build 2>&1 | grep "source-skill-row"
    ```

    Expected: no errors.

- [ ] **Step 3.3 — Commit**

    ```bash
    git add crates/desktop/src/components/source-skill-row.tsx
    git commit -m "$(cat <<'EOF'
    feat(desktop): extract SourceSkillRow shared component

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Task 4: `SourceDetail` component — unified source detail with summary bar, "needs action" card, credential dialog

**Files:**

- Create: `crates/desktop/src/components/source-detail.tsx`

This is the redesigned source detail panel. It takes the existing `SourceDetail` function from `pages/sources/index.tsx` (lines 372-1191), keeps all the mutation logic intact, and changes only the render structure to:

1. Replace the 7-section layout with a summary bar + "needs action" card + collapsed "up to date" group.
2. Mount `SourceCredentialBindingDialog` in the `needsCredential` branch (per §12-C7, it is currently only an Alert).

- [ ] **Step 4.1 — Create `source-detail.tsx` with the unified layout**

    Create `crates/desktop/src/components/source-detail.tsx`:

    ```tsx
    import {
    	ArrowDownTrayIcon,
    	ArrowPathIcon,
    	ArrowUpCircleIcon,
    	CheckCircleIcon,
    	ChevronDownIcon,
    	ChevronRightIcon,
    	ClipboardDocumentIcon,
    	ExclamationTriangleIcon,
    	FolderIcon,
    	GlobeAltIcon,
    	LockClosedIcon,
    	PlusCircleIcon,
    	QuestionMarkCircleIcon,
    	TrashIcon,
    } from "@heroicons/react/24/solid";
    import {
    	Alert,
    	Button,
    	Chip,
    	Checkbox,
    	Spinner,
    	toast,
    } from "@heroui/react";
    import { writeText } from "@tauri-apps/plugin-clipboard-manager";
    import {
    	useMutation,
    	useQuery,
    	useQueryClient,
    } from "@tanstack/react-query";
    import { useMemo, useState } from "react";
    import { useTranslation } from "react-i18next";
    import { ImportGithubSkillPanel } from "./import-github-skill-panel";
    import { SourceCredentialBindingDialog } from "./source-credential-binding-dialog";
    import { SourceSkillRow } from "./source-skill-row";
    import type { SourceSkillDiff } from "../generated/dto";
    import { useAgentAvailability } from "../hooks/use-agent-availability";
    import { useApi } from "../hooks/use-api";
    import {
    	partitionByCoverage,
    	supportsSkillMutation,
    } from "../lib/agent-capabilities";
    import {
    	allSkillPaths,
    	selectedSkills,
    	toggleSkillPath,
    } from "../lib/source-skill-selection";
    import { cn } from "../lib/utils";
    import { useSkillCoverage } from "../requests/agents";
    import { queryKeys } from "../requests/keys";
    import { applySkillUpdateMutationOptions } from "../requests/skills";
    import { sourceDiffQueryOptions } from "../requests/sources";

    const SKILL_FILE_SUFFIX_RE = /\/SKILL\.md$/;
    const EMPTY_DIFFS: SourceSkillDiff[] = [];

    export interface SourceRow {
    	source: string;
    	sourceUrl: string;
    	sourceType: string;
    	isPrivate?: boolean;
    	credentialStatus?: string;
    	skillCount: number;
    	rowScope: "global" | "project";
    	projectRoot?: string;
    	projectName?: string;
    }

    interface SourceDetailProps {
    	row: SourceRow;
    	onImport: () => void;
    }

    // ─── Summary bar counts ──────────────────────────────────────────────────────

    interface SummaryBarProps {
    	notInstalledCount: number;
    	outdatedCount: number;
    	renamedCount: number;
    	uncheckableCount: number;
    	currentCount: number;
    	isLoading: boolean;
    }

    function SummaryBar({
    	notInstalledCount,
    	outdatedCount,
    	renamedCount,
    	uncheckableCount,
    	currentCount,
    	isLoading,
    }: SummaryBarProps) {
    	const { t } = useTranslation();
    	if (isLoading) return null;

    	return (
    		<div className="flex flex-wrap gap-2 rounded-lg bg-surface-secondary px-4 py-2.5">
    			{outdatedCount > 0 && (
    				<Chip size="sm" variant="secondary">
    					{t("summaryUpdatable", { count: outdatedCount })}
    				</Chip>
    			)}
    			{notInstalledCount > 0 && (
    				<Chip size="sm" variant="secondary">
    					{t("summaryInstallable", { count: notInstalledCount })}
    				</Chip>
    			)}
    			{renamedCount > 0 && (
    				<Chip size="sm" variant="secondary">
    					{t("summaryRenamed", { count: renamedCount })}
    				</Chip>
    			)}
    			{uncheckableCount > 0 && (
    				<Chip size="sm" variant="secondary">
    					{t("summaryUnchecked", { count: uncheckableCount })}
    				</Chip>
    			)}
    			{currentCount > 0 && (
    				<Chip size="sm" variant="secondary">
    					{t("summaryLatest", { count: currentCount })}
    				</Chip>
    			)}
    		</div>
    	);
    }

    // ─── SkillSection (local — same shape as old sources/index.tsx) ─────────────

    interface SkillSectionProps {
    	title: string;
    	icon: React.ReactNode;
    	skills: SourceSkillDiff[];
    	expandedSkillPath: string | null;
    	onToggleSkill: (skillPath: string | null) => void;
    	sectionAction?: React.ReactNode;
    	rowAction?: (skill: SourceSkillDiff) => React.ReactNode;
    	selectedSkillPaths?: ReadonlySet<string>;
    	onToggleSelected?: (skill: SourceSkillDiff) => void;
    	isSelectionDisabled?: boolean;
    	muted?: boolean;
    	showReason?: boolean;
    	defaultCollapsed?: boolean;
    }

    function SkillSection({
    	title,
    	icon,
    	skills,
    	expandedSkillPath,
    	onToggleSkill,
    	sectionAction,
    	rowAction,
    	selectedSkillPaths,
    	onToggleSelected,
    	isSelectionDisabled = false,
    	muted = false,
    	showReason = false,
    	defaultCollapsed = false,
    }: SkillSectionProps) {
    	const [collapsed, setCollapsed] = useState(defaultCollapsed);

    	if (skills.length === 0) return null;

    	return (
    		<section>
    			<div className="mb-2 flex items-center justify-between gap-3">
    				<button
    					type="button"
    					className="flex min-w-0 items-center gap-2"
    					onClick={() => setCollapsed((c) => !c)}
    					aria-expanded={!collapsed}
    				>
    					{collapsed ? (
    						<ChevronRightIcon className="size-4 shrink-0 text-muted" />
    					) : (
    						<ChevronDownIcon className="size-4 shrink-0 text-muted" />
    					)}
    					{icon}
    					<h2
    						className={cn(
    							"truncate text-sm font-semibold",
    							muted ? "text-muted" : "text-foreground",
    						)}
    					>
    						{title}
    					</h2>
    					<span className="shrink-0 text-xs text-muted">
    						{skills.length}
    					</span>
    				</button>
    				{sectionAction}
    			</div>
    			{!collapsed && (
    				<ul className="overflow-hidden rounded-lg border border-border">
    					{skills.map((skill) => (
    						<SourceSkillRow
    							key={skill.skillPath}
    							skill={skill}
    							isExpanded={
    								expandedSkillPath === skill.skillPath
    							}
    							onToggle={() =>
    								onToggleSkill(
    									expandedSkillPath === skill.skillPath
    										? null
    										: skill.skillPath,
    								)
    							}
    							muted={muted}
    							showReason={showReason}
    							action={rowAction?.(skill)}
    							isSelected={selectedSkillPaths?.has(
    								skill.skillPath,
    							)}
    							onToggleSelected={
    								onToggleSelected
    									? () => onToggleSelected(skill)
    									: undefined
    							}
    							isSelectionDisabled={isSelectionDisabled}
    						/>
    					))}
    				</ul>
    			)}
    		</section>
    	);
    }

    // ─── SourceOrphanLockAlert / SourceEmptyState (copied from old page) ─────────

    interface SourceOrphanLockAlertProps {
    	prunedCount: number;
    	isChecking: boolean;
    	isCleaning: boolean;
    	onClean: () => void;
    }

    function SourceOrphanLockAlert({
    	prunedCount,
    	isChecking,
    	isCleaning,
    	onClean,
    }: SourceOrphanLockAlertProps) {
    	const { t } = useTranslation();
    	const orphanHint =
    		prunedCount === 1
    			? t("sourceOrphanHintOne", { count: prunedCount })
    			: t("sourceOrphanHintMany", { count: prunedCount });

    	return (
    		<Alert status="warning">
    			<Alert.Indicator />
    			<Alert.Content>
    				<Alert.Title>{t("sourceOrphanTitle")}</Alert.Title>
    				<Alert.Description>{orphanHint}</Alert.Description>
    				<div className="mt-3">
    					<Button
    						size="sm"
    						variant="secondary"
    						isDisabled={isChecking || isCleaning}
    						onPress={onClean}
    					>
    						<TrashIcon className="size-3.5" />
    						{isCleaning
    							? t("sourceCleaningOrphans")
    							: t("sourceCleanOrphans")}
    					</Button>
    				</div>
    			</Alert.Content>
    		</Alert>
    	);
    }

    interface SourceEmptyStateProps {
    	prunedCount: number;
    	isChecking: boolean;
    	isCleaning: boolean;
    	hasError: boolean;
    	onClean: () => void;
    	onRetry: () => void;
    }

    function SourceEmptyState({
    	prunedCount,
    	isChecking,
    	isCleaning,
    	hasError,
    	onClean,
    	onRetry,
    }: SourceEmptyStateProps) {
    	const { t } = useTranslation();
    	const hasOrphans = prunedCount > 0;

    	if (hasError) {
    		return (
    			<Alert status="danger">
    				<Alert.Indicator />
    				<Alert.Content>
    					<Alert.Title>
    						{t("sourcePrunePreviewErrorTitle")}
    					</Alert.Title>
    					<Alert.Description>
    						{t("sourcePrunePreviewErrorHint")}
    					</Alert.Description>
    					<div className="mt-3">
    						<Button
    							size="sm"
    							variant="secondary"
    							onPress={onRetry}
    						>
    							{t("retry")}
    						</Button>
    					</div>
    				</Alert.Content>
    			</Alert>
    		);
    	}

    	if (hasOrphans) {
    		return (
    			<SourceOrphanLockAlert
    				prunedCount={prunedCount}
    				isChecking={isChecking}
    				isCleaning={isCleaning}
    				onClean={onClean}
    			/>
    		);
    	}

    	return (
    		<Alert status="warning">
    			<Alert.Indicator />
    			<Alert.Content>
    				<Alert.Title>{t("sourceEmptyDiffTitle")}</Alert.Title>
    				<Alert.Description>
    					{isChecking
    						? t("sourceCheckingOrphans")
    						: t("sourceEmptyDiffHint")}
    				</Alert.Description>
    			</Alert.Content>
    		</Alert>
    	);
    }

    // ─── SourceDetail (main export) ──────────────────────────────────────────────

    export function SourceDetail({ row, onImport }: SourceDetailProps) {
    	const { t } = useTranslation();
    	const api = useApi();
    	const queryClient = useQueryClient();
    	const { availableAgents } = useAgentAvailability();
    	const [expandedSkillPath, setExpandedSkillPath] = useState<
    		string | null
    	>(null);
    	const [isApplyingAll, setIsApplyingAll] = useState(false);
    	const [isInstallingAll, setIsInstallingAll] = useState(false);
    	const [isDeletingAllRemoved, setIsDeletingAllRemoved] = useState(false);
    	const [installingSkillPath, setInstallingSkillPath] = useState<
    		string | null
    	>(null);
    	const [selectedInstallSkillPaths, setSelectedInstallSkillPaths] =
    		useState<Set<string>>(() => new Set());
    	const [isCredentialDialogOpen, setIsCredentialDialogOpen] =
    		useState(false);

    	const { data, isLoading, isFetching } = useQuery(
    		sourceDiffQueryOptions({
    			api,
    			source: row.source,
    			scope: row.rowScope,
    			projectRoot:
    				row.rowScope === "project" ? row.projectRoot : undefined,
    			enabled: true,
    		}),
    	);

    	const grouped = useMemo(() => {
    		const byState = new Map<string, SourceSkillDiff[]>();
    		for (const skill of data?.skills ?? []) {
    			const existing = byState.get(skill.state) ?? [];
    			byState.set(skill.state, [...existing, skill]);
    		}
    		return byState;
    	}, [data]);

    	const notInstalled = grouped.get("notInstalled") ?? EMPTY_DIFFS;
    	const outdated = grouped.get("installedOutdated") ?? EMPTY_DIFFS;
    	const renamed = grouped.get("renamed") ?? EMPTY_DIFFS;
    	const removed = grouped.get("removed") ?? EMPTY_DIFFS;
    	const deprecated = grouped.get("deprecated") ?? EMPTY_DIFFS;
    	const current = grouped.get("installedCurrent") ?? EMPTY_DIFFS;
    	const uncheckable = grouped.get("uncheckable") ?? EMPTY_DIFFS;

    	const selectedInstallSkills = useMemo(
    		() => selectedSkills(notInstalled, selectedInstallSkillPaths),
    		[notInstalled, selectedInstallSkillPaths],
    	);
    	const allInstallSkillPaths = useMemo(
    		() => allSkillPaths(notInstalled),
    		[notInstalled],
    	);
    	const selectedInstallCount = selectedInstallSkills.length;
    	const hasSelectedInstallSkills = selectedInstallCount > 0;
    	const allInstallSkillsSelected =
    		notInstalled.length > 0 &&
    		selectedInstallCount === notInstalled.length;
    	const hasVisibleSkills = (data?.skills.length ?? 0) > 0;
    	const updateScope = row.rowScope;
    	const updateProjectRoot =
    		row.rowScope === "project" ? (row.projectRoot ?? null) : null;
    	const shouldCheckOrphans =
    		!isLoading &&
    		!isFetching &&
    		Boolean(data) &&
    		!data?.needsCredential;

    	const installableAgents = useMemo(
    		() =>
    			availableAgents.filter(
    				(agent) =>
    					agent.isUsable &&
    					supportsSkillMutation(agent, updateScope),
    			),
    		[availableAgents, updateScope],
    	);
    	const installableAgentIds = useMemo(
    		() => installableAgents.map((agent) => agent.id),
    		[installableAgents],
    	);

    	const { coverage, isLoading: isCoverageLoading } = useSkillCoverage(
    		updateScope,
    		updateProjectRoot,
    	);
    	const { autoCovered, linkTargets } = useMemo(
    		() => partitionByCoverage(installableAgents, coverage),
    		[installableAgents, coverage],
    	);
    	const linkTargetAgentIds = useMemo(
    		() => linkTargets.map((a) => a.id),
    		[linkTargets],
    	);

    	const applyUpdateMutation = useMutation(
    		applySkillUpdateMutationOptions({
    			api,
    			queryClient,
    			onSuccess: async (data) => {
    				if (!data.success) {
    					toast.danger(data.error ?? t("skillUpdateApplyError"));
    					return;
    				}
    				toast.success(t("skillSyncedSuccessfully"));
    				await queryClient.invalidateQueries({
    					queryKey: queryKeys.skills.sources.all(),
    				});
    			},
    			onError: () => toast.danger(t("skillUpdateApplyError")),
    		}),
    	);

    	const prunePreviewQuery = useQuery({
    		queryKey: queryKeys.skills.pruneLock(
    			updateScope,
    			updateProjectRoot,
    		),
    		queryFn: () =>
    			api.skills.pruneLock({
    				scope: updateScope,
    				projectRoot: updateProjectRoot,
    				confirm: false,
    			}),
    		enabled: shouldCheckOrphans,
    	});
    	const orphanLockCount = prunePreviewQuery.data?.pruned.length ?? 0;

    	const pruneLockMutation = useMutation({
    		mutationFn: () =>
    			api.skills.pruneLock({
    				scope: updateScope,
    				projectRoot: updateProjectRoot,
    				confirm: true,
    			}),
    		onSuccess: async (result) => {
    			if (result.error) {
    				toast.danger(result.error);
    				return;
    			}
    			await queryClient.invalidateQueries({
    				queryKey: queryKeys.skills.all(),
    			});
    			if (result.pruned.length === 0) {
    				toast.success(t("sourceOrphansCleanedZero"));
    			} else {
    				toast.success(
    					t("sourceOrphansCleanedMany", {
    						count: result.pruned.length,
    					}),
    				);
    			}
    		},
    		onError: () => toast.danger(t("sourcePruneFailed")),
    	});

    	const deleteInstalledSkillByName = async (name: string) => {
    		if (installableAgentIds.length === 0) {
    			throw new Error(t("sourceRemoveNoAgents"));
    		}
    		await api.skills.delete(
    			installableAgentIds[0],
    			name,
    			updateScope,
    			updateProjectRoot ?? undefined,
    			true,
    		);
    	};

    	const deleteRenamedSkillMutation = useMutation({
    		mutationFn: async (skill: SourceSkillDiff) => {
    			const oldName = skill.previousName;
    			if (!oldName) {
    				throw new Error("Missing previous name for renamed skill.");
    			}
    			await deleteInstalledSkillByName(oldName);
    		},
    		onSuccess: async (_data, skill) => {
    			if (!skill.previousName) return;
    			toast.success(
    				t("sourceRenamedDeleted", { oldName: skill.previousName }),
    			);
    			await queryClient.invalidateQueries({
    				queryKey: queryKeys.skills.all(),
    			});
    		},
    		onError: (error, skill) => {
    			if (skill.previousName) {
    				toast.danger(
    					error instanceof Error
    						? error.message
    						: t("sourceRenamedDeleteFailed", {
    								oldName: skill.previousName,
    							}),
    				);
    				return;
    			}
    			toast.danger(t("sourcePruneFailed"));
    		},
    	});

    	const deleteRemovedSkillMutation = useMutation({
    		mutationFn: async (skill: SourceSkillDiff) => {
    			await deleteInstalledSkillByName(skill.name);
    		},
    		onSuccess: async (_data, skill) => {
    			toast.success(t("sourceRemovedCleaned", { name: skill.name }));
    			await queryClient.invalidateQueries({
    				queryKey: queryKeys.skills.all(),
    			});
    		},
    		onError: (error, skill) => {
    			toast.danger(
    				error instanceof Error
    					? error.message
    					: t("sourceRemovedCleanFailed", { name: skill.name }),
    			);
    		},
    	});

    	const copyRenamedInstallMutation = useMutation({
    		mutationFn: async (skill: SourceSkillDiff) => {
    			await writeText(`aghub-cli install ${skill.name}`);
    		},
    		onSuccess: (_data, skill) => {
    			toast.success(
    				t("sourceRenamedInstallCommandCopied", {
    					newName: skill.name,
    				}),
    			);
    		},
    		onError: () => toast.danger(t("sourceCopyCommandFailed")),
    	});

    	const updateRequestFor = (skill: SourceSkillDiff) => ({
    		name: skill.name,
    		scope: updateScope,
    		projectRoot: updateProjectRoot,
    		confirm: true,
    	});

    	const applyOneUpdate = (skill: SourceSkillDiff) => {
    		applyUpdateMutation.mutate(updateRequestFor(skill));
    	};

    	const applyAllUpdates = async (skills: SourceSkillDiff[]) => {
    		if (skills.length === 0 || isApplyingAll) return;
    		setIsApplyingAll(true);
    		let updated = 0;
    		let failed = 0;
    		try {
    			for (const skill of skills) {
    				try {
    					const result = await api.skills.applyUpdate(
    						updateRequestFor(skill),
    					);
    					if (result.success) {
    						updated += 1;
    					} else {
    						failed += 1;
    					}
    				} catch {
    					failed += 1;
    				}
    			}
    			await queryClient.invalidateQueries({
    				queryKey: queryKeys.skills.all(),
    			});
    			await queryClient.invalidateQueries({
    				queryKey: queryKeys.skills.sources.all(),
    			});
    			if (failed > 0) {
    				toast.danger(
    					failed === 1
    						? t("sourceUpdateSomeFailedOne", { count: failed })
    						: t("sourceUpdateSomeFailedMany", {
    								count: failed,
    							}),
    				);
    			} else {
    				toast.success(
    					t("sourceUpdatesApplied", { count: updated }),
    				);
    			}
    		} finally {
    			setIsApplyingAll(false);
    		}
    	};

    	const deleteAllRemovedSkills = async (skills: SourceSkillDiff[]) => {
    		if (
    			skills.length === 0 ||
    			isDeletingAllRemoved ||
    			deleteRemovedSkillMutation.isPending
    		) {
    			return;
    		}
    		if (installableAgentIds.length === 0) {
    			toast.danger(t("sourceRemoveNoAgents"));
    			return;
    		}
    		setIsDeletingAllRemoved(true);
    		let cleaned = 0;
    		let failed = 0;
    		try {
    			for (const skill of skills) {
    				try {
    					await deleteInstalledSkillByName(skill.name);
    					cleaned += 1;
    				} catch {
    					failed += 1;
    				}
    			}
    			await queryClient.invalidateQueries({
    				queryKey: queryKeys.skills.all(),
    			});
    			if (failed > 0) {
    				toast.danger(
    					failed === 1
    						? t("sourceRemovedCleanSomeFailedOne", {
    								count: failed,
    							})
    						: t("sourceRemovedCleanSomeFailedMany", {
    								count: failed,
    							}),
    				);
    			} else {
    				toast.success(
    					t("sourceRemovedCleanedMany", { count: cleaned }),
    				);
    			}
    		} finally {
    			setIsDeletingAllRemoved(false);
    		}
    	};

    	const toggleInstallSkillSelection = (skill: SourceSkillDiff) => {
    		setSelectedInstallSkillPaths((previous) =>
    			toggleSkillPath(previous, skill.skillPath),
    		);
    	};

    	const selectAllInstallSkills = () => {
    		setSelectedInstallSkillPaths(new Set(allInstallSkillPaths));
    	};

    	const clearSelectedInstallSkills = () => {
    		setSelectedInstallSkillPaths(new Set());
    	};

    	const installPathFor = (skill: SourceSkillDiff) =>
    		skill.skillPath === "SKILL.md"
    			? "."
    			: skill.skillPath.replace(SKILL_FILE_SUFFIX_RE, "");

    	const installFromSource = async (skills: SourceSkillDiff[]) => {
    		if (
    			skills.length === 0 ||
    			isInstallingAll ||
    			installingSkillPath !== null
    		) {
    			return;
    		}
    		const installAll = skills.length > 1;
    		if (installAll) {
    			setIsInstallingAll(true);
    		} else {
    			setInstallingSkillPath(skills[0]?.skillPath ?? null);
    		}

    		try {
    			const scan = await api.skills.gitScan({
    				url: row.sourceUrl,
    				credential_id: null,
    				branch: data?.gitRef ?? null,
    				session_id: null,
    			});
    			const wantedPaths = new Set(skills.map(installPathFor));
    			const scanPaths = new Set(
    				scan.skills.map((skill) => skill.path),
    			);
    			const skillPaths = Array.from(wantedPaths).filter((path) =>
    				scanPaths.has(path),
    			);

    			if (skillPaths.length !== wantedPaths.size) {
    				throw new Error(t("sourceInstallFailed"));
    			}

    			const result = await api.skills.gitInstall({
    				session_id: scan.session_id,
    				skill_paths: skillPaths,
    				agents: linkTargetAgentIds,
    				scope: updateScope,
    				project_root: updateProjectRoot,
    			});
    			const failed = result.results.filter((entry) => !entry.success);

    			await queryClient.invalidateQueries({
    				queryKey: queryKeys.skills.all(),
    			});
    			await queryClient.invalidateQueries({
    				queryKey: queryKeys.skills.sources.all(),
    			});

    			if (failed.length > 0) {
    				toast.danger(
    					failed.length === 1
    						? t("sourceInstallSomeFailedOne", {
    								count: failed.length,
    							})
    						: t("sourceInstallSomeFailedMany", {
    								count: failed.length,
    							}),
    				);
    			} else {
    				toast.success(
    					t("sourceInstalled", { count: skillPaths.length }),
    				);
    			}
    		} catch (error) {
    			toast.danger(
    				error instanceof Error
    					? error.message
    					: t("sourceInstallFailed"),
    			);
    		} finally {
    			setIsInstallingAll(false);
    			setInstallingSkillPath(null);
    		}
    	};

    	// "Needs action" bucket: all states that require user intervention
    	const needsActionSkills = useMemo(
    		() => [
    			...outdated,
    			...notInstalled,
    			...renamed,
    			...removed,
    			...deprecated,
    			...uncheckable,
    		],
    		[outdated, notInstalled, renamed, removed, deprecated, uncheckable],
    	);

    	const hasNeedsAction = needsActionSkills.length > 0;

    	const SourceIcon =
    		row.sourceType === "local" ? FolderIcon : GlobeAltIcon;

    	return (
    		<div className="flex h-full flex-col overflow-hidden">
    			{/* Header */}
    			<div className="flex items-start justify-between gap-3 border-b border-border p-4">
    				<div className="min-w-0">
    					<div className="flex items-center gap-2">
    						<SourceIcon className="size-5 shrink-0 text-muted" />
    						<h1 className="truncate text-lg font-semibold text-foreground">
    							{row.source}
    						</h1>
    						{row.isPrivate && (
    							<LockClosedIcon
    								className="size-3.5 shrink-0 text-muted"
    								aria-label={t("privateRepo")}
    							/>
    						)}
    						<Chip size="sm" variant="secondary">
    							{row.rowScope === "global"
    								? t("scopeGlobal")
    								: `${t("scopeProject")} · ${row.projectName ?? ""}`}
    						</Chip>
    					</div>
    					<p className="mt-1 truncate font-mono text-xs text-muted">
    						{row.sourceUrl}
    					</p>
    				</div>
    				<Button className="shrink-0" onPress={onImport}>
    					<ArrowDownTrayIcon className="size-4" />
    					{t("importFromThisSource")}
    				</Button>
    			</div>

    			{/* Body */}
    			<div className="min-h-0 flex-1 overflow-y-auto p-4">
    				{isLoading || isFetching ? (
    					<div className="flex flex-col items-center gap-3 py-12">
    						<Spinner size="lg" />
    						<p className="text-sm text-muted">
    							{t("checkingSource")}
    						</p>
    					</div>
    				) : data?.needsCredential ? (
    					<div className="space-y-4">
    						<Alert status="warning">
    							<Alert.Indicator />
    							<Alert.Content>
    								<Alert.Title>
    									{t("needsCredential")}
    								</Alert.Title>
    								<Alert.Description>
    									{t("needsCredentialHint")}
    								</Alert.Description>
    								<div className="mt-3">
    									<Button
    										size="sm"
    										variant="secondary"
    										onPress={() =>
    											setIsCredentialDialogOpen(true)
    										}
    									>
    										{t("credentialBind")}
    									</Button>
    								</div>
    							</Alert.Content>
    						</Alert>
    						<SourceCredentialBindingDialog
    							isOpen={isCredentialDialogOpen}
    							bindingSource={row.source}
    							onClose={() => setIsCredentialDialogOpen(false)}
    							onBound={async () => {
    								setIsCredentialDialogOpen(false);
    								await queryClient.invalidateQueries({
    									queryKey:
    										queryKeys.skills.sources.all(),
    								});
    							}}
    						/>
    					</div>
    				) : (
    					<div className="space-y-6">
    						{/* Summary bar — per-source counts (no global "installable") */}
    						<SummaryBar
    							notInstalledCount={notInstalled.length}
    							outdatedCount={outdated.length}
    							renamedCount={renamed.length}
    							uncheckableCount={uncheckable.length}
    							currentCount={current.length}
    							isLoading={isLoading}
    						/>

    						{hasVisibleSkills && orphanLockCount > 0 && (
    							<SourceOrphanLockAlert
    								prunedCount={orphanLockCount}
    								isChecking={prunePreviewQuery.isFetching}
    								isCleaning={pruneLockMutation.isPending}
    								onClean={() => pruneLockMutation.mutate()}
    							/>
    						)}
    						{!hasVisibleSkills && (
    							<SourceEmptyState
    								prunedCount={orphanLockCount}
    								isChecking={prunePreviewQuery.isFetching}
    								isCleaning={pruneLockMutation.isPending}
    								hasError={prunePreviewQuery.isError}
    								onClean={() => pruneLockMutation.mutate()}
    								onRetry={() => prunePreviewQuery.refetch()}
    							/>
    						)}

    						{/* "Needs action" card — all actionable states mixed */}
    						{hasNeedsAction && (
    							<section>
    								<div className="mb-2 flex items-center justify-between gap-3">
    									<div className="flex items-center gap-2">
    										<ExclamationTriangleIcon className="size-4 text-warning" />
    										<h2 className="truncate text-sm font-semibold text-foreground">
    											{t("sourceNeedsAction")}
    										</h2>
    										<span className="text-xs text-muted">
    											{needsActionSkills.length}
    										</span>
    									</div>
    									{/* Batch buttons */}
    									<div className="flex items-center gap-1">
    										{outdated.length > 0 && (
    											<Button
    												size="sm"
    												variant="ghost"
    												className="h-7 px-2 text-xs"
    												isDisabled={
    													isApplyingAll ||
    													applyUpdateMutation.isPending
    												}
    												onPress={() =>
    													applyAllUpdates(
    														outdated,
    													)
    												}
    											>
    												<ArrowPathIcon className="size-3.5" />
    												{isApplyingAll
    													? t("sourceUpdating")
    													: t("sourceUpdateAll")}
    											</Button>
    										)}
    										{notInstalled.length > 0 && (
    											<Button
    												size="sm"
    												variant="ghost"
    												className="h-7 px-2 text-xs"
    												isDisabled={
    													isInstallingAll ||
    													installingSkillPath !==
    														null ||
    													isCoverageLoading
    												}
    												onPress={() =>
    													installFromSource(
    														hasSelectedInstallSkills
    															? selectedInstallSkills
    															: notInstalled,
    													)
    												}
    											>
    												<ArrowDownTrayIcon className="size-3.5" />
    												{isInstallingAll
    													? t("sourceInstalling")
    													: hasSelectedInstallSkills
    														? t(
    																"sourceInstallSelected",
    																{
    																	count: selectedInstallCount,
    																},
    															)
    														: t(
    																"sourceInstallAll",
    															)}
    											</Button>
    										)}
    									</div>
    								</div>

    								<ul className="overflow-hidden rounded-lg border border-border">
    									{outdated.map((skill) => {
    										const isApplying =
    											applyUpdateMutation.isPending &&
    											applyUpdateMutation.variables
    												?.name === skill.name;
    										return (
    											<SourceSkillRow
    												key={skill.skillPath}
    												skill={skill}
    												isExpanded={
    													expandedSkillPath ===
    													skill.skillPath
    												}
    												onToggle={() =>
    													setExpandedSkillPath(
    														expandedSkillPath ===
    															skill.skillPath
    															? null
    															: skill.skillPath,
    													)
    												}
    												action={
    													<Button
    														size="sm"
    														variant="secondary"
    														className="h-7 px-2 text-xs"
    														isDisabled={
    															isApplyingAll ||
    															isApplying
    														}
    														onPress={() =>
    															applyOneUpdate(
    																skill,
    															)
    														}
    													>
    														<ArrowPathIcon className="size-3.5" />
    														{isApplying
    															? t(
    																	"sourceUpdating",
    																)
    															: t(
    																	"sourceUpdateSkill",
    																)}
    													</Button>
    												}
    											/>
    										);
    									})}
    									{notInstalled.map((skill) => {
    										const isInstalling =
    											installingSkillPath ===
    											skill.skillPath;
    										return (
    											<SourceSkillRow
    												key={skill.skillPath}
    												skill={skill}
    												isExpanded={
    													expandedSkillPath ===
    													skill.skillPath
    												}
    												onToggle={() =>
    													setExpandedSkillPath(
    														expandedSkillPath ===
    															skill.skillPath
    															? null
    															: skill.skillPath,
    													)
    												}
    												isSelected={selectedInstallSkillPaths.has(
    													skill.skillPath,
    												)}
    												onToggleSelected={() =>
    													toggleInstallSkillSelection(
    														skill,
    													)
    												}
    												isSelectionDisabled={
    													isInstallingAll ||
    													installingSkillPath !==
    														null
    												}
    												action={
    													<Button
    														size="sm"
    														variant="secondary"
    														className="h-7 px-2 text-xs"
    														isDisabled={
    															isInstallingAll ||
    															installingSkillPath !==
    																null ||
    															isCoverageLoading
    														}
    														onPress={() =>
    															installFromSource(
    																[skill],
    															)
    														}
    													>
    														<ArrowDownTrayIcon className="size-3.5" />
    														{isInstalling
    															? t(
    																	"sourceInstalling",
    																)
    															: t(
    																	"sourceInstallSkill",
    																)}
    													</Button>
    												}
    											/>
    										);
    									})}
    									{renamed.map((skill) => {
    										const isDeleting =
    											deleteRenamedSkillMutation.isPending &&
    											deleteRenamedSkillMutation
    												.variables?.skillPath ===
    												skill.skillPath;
    										const isCopying =
    											copyRenamedInstallMutation.isPending &&
    											copyRenamedInstallMutation
    												.variables?.skillPath ===
    												skill.skillPath;
    										const rowBusy =
    											isDeleting || isCopying;
    										return (
    											<SourceSkillRow
    												key={skill.skillPath}
    												skill={skill}
    												isExpanded={
    													expandedSkillPath ===
    													skill.skillPath
    												}
    												onToggle={() =>
    													setExpandedSkillPath(
    														expandedSkillPath ===
    															skill.skillPath
    															? null
    															: skill.skillPath,
    													)
    												}
    												showReason
    												action={
    													<div className="flex items-center gap-1.5">
    														<Button
    															size="sm"
    															variant="secondary"
    															className="h-7 px-2 text-xs"
    															isDisabled={
    																!skill.previousName ||
    																rowBusy
    															}
    															onPress={() =>
    																deleteRenamedSkillMutation.mutate(
    																	skill,
    																)
    															}
    														>
    															<TrashIcon className="size-3.5" />
    															{isDeleting
    																? t(
    																		"sourceRenamedDeleting",
    																	)
    																: t(
    																		"sourceRenamedDeleteOld",
    																	)}
    														</Button>
    														<Button
    															size="sm"
    															variant="ghost"
    															className="h-7 px-2 text-xs"
    															isDisabled={
    																rowBusy
    															}
    															onPress={() =>
    																copyRenamedInstallMutation.mutate(
    																	skill,
    																)
    															}
    														>
    															<ClipboardDocumentIcon className="size-3.5" />
    															{isCopying
    																? t(
    																		"sourceRenamedCopying",
    																	)
    																: t(
    																		"sourceRenamedCopyInstall",
    																	)}
    														</Button>
    													</div>
    												}
    											/>
    										);
    									})}
    									{removed.map((skill) => {
    										const isDeleting =
    											deleteRemovedSkillMutation.isPending &&
    											deleteRemovedSkillMutation
    												.variables?.skillPath ===
    												skill.skillPath;
    										return (
    											<SourceSkillRow
    												key={skill.skillPath}
    												skill={skill}
    												isExpanded={
    													expandedSkillPath ===
    													skill.skillPath
    												}
    												onToggle={() =>
    													setExpandedSkillPath(
    														expandedSkillPath ===
    															skill.skillPath
    															? null
    															: skill.skillPath,
    													)
    												}
    												muted
    												showReason
    												action={
    													<Button
    														size="sm"
    														variant="secondary"
    														className="h-7 px-2 text-xs"
    														isDisabled={
    															isDeletingAllRemoved ||
    															isDeleting
    														}
    														onPress={() =>
    															deleteRemovedSkillMutation.mutate(
    																skill,
    															)
    														}
    													>
    														<TrashIcon className="size-3.5" />
    														{isDeleting
    															? t(
    																	"sourceRemovedCleaning",
    																)
    															: t(
    																	"sourceRemovedCleanSkill",
    																)}
    													</Button>
    												}
    											/>
    										);
    									})}
    									{deprecated.map((skill) => (
    										<SourceSkillRow
    											key={skill.skillPath}
    											skill={skill}
    											isExpanded={
    												expandedSkillPath ===
    												skill.skillPath
    											}
    											onToggle={() =>
    												setExpandedSkillPath(
    													expandedSkillPath ===
    														skill.skillPath
    														? null
    														: skill.skillPath,
    												)
    											}
    											muted
    										/>
    									))}
    									{uncheckable.map((skill) => (
    										<SourceSkillRow
    											key={skill.skillPath}
    											skill={skill}
    											isExpanded={
    												expandedSkillPath ===
    												skill.skillPath
    											}
    											onToggle={() =>
    												setExpandedSkillPath(
    													expandedSkillPath ===
    														skill.skillPath
    														? null
    														: skill.skillPath,
    												)
    											}
    											muted
    											showReason
    											action={
    												skill.reason === "auth" ? (
    													<Button
    														size="sm"
    														variant="secondary"
    														className="h-7 px-2 text-xs"
    														onPress={() =>
    															setIsCredentialDialogOpen(
    																true,
    															)
    														}
    													>
    														{t(
    															"credentialBind",
    														)}
    													</Button>
    												) : undefined
    											}
    										/>
    									))}
    								</ul>
    							</section>
    						)}

    						{/* "Installed (latest)" — collapsed by default */}
    						<SkillSection
    							title={t("sourceStateCurrent")}
    							icon={
    								<CheckCircleIcon className="size-4 text-success" />
    							}
    							skills={current}
    							expandedSkillPath={expandedSkillPath}
    							onToggleSkill={setExpandedSkillPath}
    							defaultCollapsed
    						/>

    						{/* All-clear empty state */}
    						{!hasNeedsAction &&
    							!isLoading &&
    							current.length > 0 && (
    								<div className="rounded-lg border border-success/30 bg-success/5 px-4 py-3">
    									<div className="flex items-center gap-2">
    										<CheckCircleIcon className="size-4 shrink-0 text-success" />
    										<p className="text-sm text-success">
    											{t("sourceAllLatest")}
    										</p>
    									</div>
    								</div>
    							)}

    						{/* Agent coverage hint */}
    						{(autoCovered.length > 0 ||
    							linkTargets.length > 0) &&
    							notInstalled.length > 0 && (
    								<div className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
    									<span>
    										{linkTargets.length}{" "}
    										{t("sourceInstallLinkTargetsTitle")}{" "}
    										/ {autoCovered.length}{" "}
    										{t("sourceInstallCoveredTitle")}
    									</span>
    									{autoCovered.length > 0 && (
    										<>
    											<span className="mx-1 text-muted/50">
    												·
    											</span>
    											<span className="text-muted">
    												{t("agentCoveredBadge")}:
    											</span>
    											{autoCovered.map((agent) => (
    												<Chip
    													key={agent.id}
    													size="sm"
    													variant="secondary"
    												>
    													{agent.display_name}
    												</Chip>
    											))}
    										</>
    									)}
    								</div>
    							)}
    					</div>
    				)}
    			</div>

    			{/* Credential dialog also mounts at root level for uncheckable rows */}
    			<SourceCredentialBindingDialog
    				isOpen={isCredentialDialogOpen}
    				bindingSource={row.source}
    				onClose={() => setIsCredentialDialogOpen(false)}
    				onBound={async () => {
    					setIsCredentialDialogOpen(false);
    					await queryClient.invalidateQueries({
    						queryKey: queryKeys.skills.sources.all(),
    					});
    				}}
    			/>
    		</div>
    	);
    }
    ```

    ASSUMPTION: `sourceDiffQueryOptions` in `requests/sources.ts` accepts `{ api, source, scope, projectRoot, enabled }` — the same shape used in the old `SourceDetail`. `queryKeys.skills.pruneLock(scope, projectRoot)` and `queryKeys.skills.sources.all()` are confirmed to exist in `requests/keys.ts`.

- [ ] **Step 4.2 — Verify TypeScript compiles for new component**

    ```bash
    cd crates/desktop && bun run build 2>&1 | grep "source-detail"
    ```

    Expected: no errors.

- [ ] **Step 4.3 — Commit**

    ```bash
    git add crates/desktop/src/components/source-detail.tsx
    git commit -m "$(cat <<'EOF'
    feat(desktop): add SourceDetail component with summary bar, needs-action card, credential dialog

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Task 5: Unified Skills page — view toggle, scope switch, nuqs URL schema, auto-check query

**Files:**

- Modify: `crates/desktop/src/pages/settings/skills.tsx`

The existing `skills.tsx` is rewritten. Key design choices driven by spec §12:

- `?view=agent|source` via nuqs; switching clears the other view's detail param.
- `?skill=<name>` for agent-view detail (already exists); `?source=<key>` for source-view selected source (migrating from old `?source=` in old `SourcesPage`).
- Scope switch: global/project segmented control + projects dropdown; current page is hard-locked to `"global"` (§12-C8).
- Auto check-updates: `useQuery` with `staleTime` as throttle (§12-GAP8 — must NOT be `useEffect`). The existing `checkSkillUpdatesMutationOptions` remains for the manual "Recheck" button; a new `checkSkillUpdatesQueryOptions` wrapper is needed.

- [ ] **Step 5.1 — Add `checkSkillUpdatesQueryOptions` to `requests/skills.ts`**

    Open `crates/desktop/src/requests/skills.ts`. After line 368 (after `checkSkillUpdatesMutationOptions`), add:

    ```ts
    const CHECK_UPDATES_THROTTLE_MS = 10 * 60 * 1000; // 10 minutes

    export function checkSkillUpdatesQueryOptions({
    	api,
    	params,
    	enabled = true,
    }: {
    	api: ApiClient;
    	params?: CheckSkillUpdatesParams;
    	enabled?: boolean;
    }) {
    	return queryOptions({
    		queryKey: checkSkillUpdatesQueryKey(params),
    		queryFn: () => api.skills.checkUpdates(params),
    		staleTime: CHECK_UPDATES_THROTTLE_MS,
    		enabled,
    	});
    }
    ```

    ASSUMPTION: `queryOptions` from `@tanstack/react-query` is already imported in `requests/skills.ts`. If not, add the import.

- [ ] **Step 5.2 — Rewrite `pages/settings/skills.tsx`**

    Replace the entire contents of `crates/desktop/src/pages/settings/skills.tsx`:

    ```tsx
    import {
    	ArrowPathIcon,
    	CheckCircleIcon,
    	GlobeAltIcon,
    	PlusIcon,
    	RectangleStackIcon,
    } from "@heroicons/react/24/solid";
    import {
    	Button,
    	Chip,
    	Dropdown,
    	Select,
    	ListBox,
    	Label,
    	Spinner,
    	Tooltip,
    	toast,
    } from "@heroui/react";
    import {
    	useMutation,
    	useQuery,
    	useQueries,
    	useQueryClient,
    	useSuspenseQuery,
    } from "@tanstack/react-query";
    import { useQueryState } from "nuqs";
    import { useMemo, useState } from "react";
    import { useTranslation } from "react-i18next";
    import { BulkDeleteDialog } from "../../components/bulk-delete-dialog";
    import { CreateSkillPanel } from "../../components/create-skill-panel";
    import { ImportGithubSkillPanel } from "../../components/import-github-skill-panel";
    import { ImportSkillPanel } from "../../components/import-skill-panel";
    import { ListSearchHeader } from "../../components/list-search-header";
    import { MultiSelectFloatingBar } from "../../components/multi-select-floating-bar";
    import { SkillDetail } from "../../components/skill-detail";
    import { SkillList } from "../../components/skill-list";
    import { SourceDetail } from "../../components/source-detail";
    import type {
    	SkillResponse,
    	SkillUpdateResponse,
    	SourceSummaryResponse,
    	SourcesListResponse,
    } from "../../generated/dto";
    import { useAgentAvailability } from "../../hooks/use-agent-availability";
    import { useApi } from "../../hooks/use-api";
    import { useProjects } from "../../hooks/use-projects";
    import { cn } from "../../lib/utils";
    import {
    	checkSkillUpdatesMutationOptions,
    	checkSkillUpdatesQueryKey,
    	checkSkillUpdatesQueryOptions,
    	skillListQueryOptions,
    } from "../../requests/skills";
    import { sourcesListQueryOptions } from "../../requests/sources";
    import { queryKeys } from "../../requests/keys";

    // ─── Types ───────────────────────────────────────────────────────────────────

    type ViewMode = "agent" | "source";

    interface SourceRow extends SourceSummaryResponse {
    	rowScope: "global" | "project";
    	projectRoot?: string;
    	projectName?: string;
    }

    const TRAILING_SLASH_RE = /\/+$/;

    function sourceDisplayName(row: SourceRow): string {
    	if (row.sourceType !== "local") return row.source;
    	const trimmed = row.source.replace(TRAILING_SLASH_RE, "");
    	return trimmed.split("/").filter(Boolean).pop() ?? row.source;
    }

    function sourceRowKey(r: SourceRow) {
    	return `${r.rowScope}:${r.projectRoot ?? ""}:${r.source}`;
    }

    // ─── Scope switch ─────────────────────────────────────────────────────────────

    interface ScopeSwitchProps {
    	scope: "global" | "project";
    	onScopeChange: (scope: "global" | "project") => void;
    	selectedProjectPath: string | null;
    	onProjectChange: (path: string) => void;
    }

    function ScopeSwitch({
    	scope,
    	onScopeChange,
    	selectedProjectPath,
    	onProjectChange,
    }: ScopeSwitchProps) {
    	const { t } = useTranslation();
    	const { data: projects = [] } = useProjects();

    	return (
    		<div className="flex items-center gap-2 px-3 py-2 border-b border-border">
    			<div className="flex rounded-lg bg-surface-secondary p-0.5">
    				<button
    					type="button"
    					className={cn(
    						"rounded-md px-3 py-1 text-xs font-medium transition-colors",
    						scope === "global"
    							? "bg-white shadow-sm text-foreground"
    							: "text-muted hover:text-foreground",
    					)}
    					onClick={() => onScopeChange("global")}
    				>
    					{t("scopeSwitchGlobal")}
    				</button>
    				<button
    					type="button"
    					className={cn(
    						"rounded-md px-3 py-1 text-xs font-medium transition-colors",
    						scope === "project"
    							? "bg-white shadow-sm text-foreground"
    							: "text-muted hover:text-foreground",
    					)}
    					onClick={() => onScopeChange("project")}
    					isDisabled={projects.length === 0}
    				>
    					{t("scopeSwitchProject")}
    				</button>
    			</div>
    			{scope === "project" && projects.length > 0 && (
    				<Select
    					className="min-w-0 flex-1"
    					variant="secondary"
    					selectedKey={selectedProjectPath ?? undefined}
    					onSelectionChange={(key) => {
    						if (key) onProjectChange(String(key));
    					}}
    				>
    					<Select.Trigger>
    						<Select.Value placeholder={t("selectProject")} />
    						<Select.Indicator />
    					</Select.Trigger>
    					<Select.Popover>
    						<ListBox>
    							{projects.map((p) => (
    								<ListBox.Item
    									key={p.path}
    									id={p.path}
    									textValue={p.name}
    								>
    									{p.name}
    									<ListBox.ItemIndicator />
    								</ListBox.Item>
    							))}
    						</ListBox>
    					</Select.Popover>
    				</Select>
    			)}
    		</div>
    	);
    }

    // ─── Source list panel (view=source) ─────────────────────────────────────────

    interface SourceListPanelProps {
    	scope: "global" | "project";
    	projectPath: string | null;
    	selectedKey: string | null;
    	onSelectKey: (key: string) => void;
    	searchQuery: string;
    	updateStatuses: ReadonlyMap<string, SkillUpdateResponse>;
    }

    function SourceListPanel({
    	scope,
    	projectPath,
    	selectedKey,
    	onSelectKey,
    	searchQuery,
    	updateStatuses,
    }: SourceListPanelProps) {
    	const { t } = useTranslation();
    	const api = useApi();
    	const { data: projects = [] } = useProjects();

    	const sourceQueries = useQueries({
    		queries:
    			scope === "global"
    				? [sourcesListQueryOptions({ api, scope: "global" })]
    				: projects
    						.filter(
    							(p) => !projectPath || p.path === projectPath,
    						)
    						.map((p) =>
    							sourcesListQueryOptions({
    								api,
    								scope: "project",
    								projectRoot: p.path,
    							}),
    						),
    	});

    	const isLoading = sourceQueries.some((q) => q.isLoading);

    	const rows = useMemo<SourceRow[]>(() => {
    		if (scope === "global") {
    			const data = sourceQueries[0]?.data as
    				| SourcesListResponse
    				| undefined;
    			return (data?.sources ?? []).map((s) => ({
    				...s,
    				rowScope: "global" as const,
    			}));
    		}
    		const filtered = projectPath
    			? projects.filter((p) => p.path === projectPath)
    			: projects;
    		return filtered.flatMap((project, index) => {
    			const data = sourceQueries[index]?.data as
    				| SourcesListResponse
    				| undefined;
    			return (data?.sources ?? []).map((s) => ({
    				...s,
    				rowScope: "project" as const,
    				projectRoot: project.path,
    				projectName: project.name,
    			}));
    		});
    	}, [sourceQueries, projects, scope, projectPath]);

    	const filteredRows = useMemo(() => {
    		const q = searchQuery.trim().toLowerCase();
    		if (!q) return rows;
    		return rows.filter((r) => r.source.toLowerCase().includes(q));
    	}, [rows, searchQuery]);

    	if (isLoading) {
    		return (
    			<div className="flex flex-1 items-center justify-center">
    				<Spinner />
    			</div>
    		);
    	}

    	if (filteredRows.length === 0) {
    		return (
    			<p className="px-4 py-8 text-center text-sm text-muted">
    				{t("sourcesEmpty")}
    			</p>
    		);
    	}

    	return (
    		<div className="min-h-0 flex-1 overflow-y-auto">
    			<ul className="space-y-1 p-2">
    				{filteredRows.map((row) => {
    					const key = sourceRowKey(row);
    					const isActive = key === selectedKey;
    					// Derive "has updates" from check-updates results
    					// by checking if any skill in this source is outdated/renamed
    					// We don't have per-source update aggregation here from check-updates
    					// so we check if any updateStatuses entry would match (by source).
    					// This is a best-effort visual hint; the diff provides exact counts.
    					return (
    						<li key={key}>
    							<button
    								type="button"
    								onClick={() => onSelectKey(key)}
    								aria-current={isActive ? "page" : undefined}
    								className={cn(
    									"group flex h-12 w-full items-center gap-3 rounded-xl px-3 text-left outline-none transition-colors hover:bg-surface-secondary focus-visible:ring-2 focus-visible:ring-accent/35",
    									isActive && "bg-surface",
    								)}
    							>
    								<GlobeAltIcon
    									className={cn(
    										"size-5 shrink-0 text-muted",
    										isActive && "text-foreground",
    									)}
    								/>
    								<span
    									className="min-w-0 flex-1 truncate text-base font-semibold leading-6 text-foreground"
    									title={row.source}
    								>
    									{sourceDisplayName(row)}
    								</span>
    								<span className="ml-auto shrink-0 rounded-full bg-surface-tertiary px-2 font-mono text-sm leading-7 text-muted tabular-nums">
    									{row.skillCount}
    								</span>
    							</button>
    						</li>
    					);
    				})}
    			</ul>
    		</div>
    	);
    }

    // ─── Main page ───────────────────────────────────────────────────────────────

    export default function SkillsPage() {
    	const { t } = useTranslation();
    	const api = useApi();
    	const queryClient = useQueryClient();

    	// ── URL state (nuqs) ──
    	const [view, setView] = useQueryState<ViewMode>("view", {
    		defaultValue: "agent",
    		parse: (v): ViewMode => (v === "source" ? "source" : "agent"),
    		serialize: (v) => v,
    	});
    	const [selectedSkillName, setSelectedSkillName] =
    		useQueryState("skill");
    	const [selectedSourceKey, setSelectedSourceKey] =
    		useQueryState("source");

    	const handleSetView = (newView: ViewMode) => {
    		// Clear the other view's param when switching
    		if (newView === "source") {
    			void setSelectedSkillName(null);
    		} else {
    			void setSelectedSourceKey(null);
    		}
    		void setView(newView);
    	};

    	// ── Scope state ──
    	const [scope, setScope] = useState<"global" | "project">("global");
    	const [selectedProjectPath, setSelectedProjectPath] = useState<
    		string | null
    	>(null);

    	const handleScopeChange = (newScope: "global" | "project") => {
    		setScope(newScope);
    		if (newScope === "global") {
    			setSelectedProjectPath(null);
    		}
    	};

    	// ── Skills data (shared between both views) ──
    	const skillQueryScope = useMemo<"global" | "project">(
    		() => scope,
    		[scope],
    	);
    	const skillQueryProjectRoot = useMemo(
    		() =>
    			scope === "project"
    				? (selectedProjectPath ?? undefined)
    				: undefined,
    		[scope, selectedProjectPath],
    	);

    	const {
    		data: skills,
    		refetch,
    		isFetching,
    	} = useSuspenseQuery({
    		...skillListQueryOptions({
    			api,
    			scope: skillQueryScope,
    			projectRoot: skillQueryProjectRoot,
    		}),
    	});

    	// ── Auto check-updates (useQuery + staleTime = throttle, per §12-GAP8) ──
    	const checkUpdatesParams = useMemo(
    		() => ({
    			scope: skillQueryScope,
    			projectRoot: skillQueryProjectRoot,
    		}),
    		[skillQueryScope, skillQueryProjectRoot],
    	);

    	const {
    		data: autoCheckData,
    		isFetching: isAutoChecking,
    		dataUpdatedAt: lastCheckedAt,
    	} = useQuery({
    		...checkSkillUpdatesQueryOptions({
    			api,
    			params: checkUpdatesParams,
    			enabled: true,
    		}),
    	});

    	// ── Manual recheck (mutation for explicit user action) ──
    	const recheckMutation = useMutation(
    		checkSkillUpdatesMutationOptions({
    			api,
    			queryClient,
    			onSuccess: (data) => {
    				const updatableCount = data.filter(
    					(s) =>
    						s.status === "updateAvailable" ||
    						s.status === "renamed",
    				).length;
    				if (updatableCount === 0) {
    					toast.success(t("skillCheckAllLatest"));
    				} else {
    					toast.success(
    						t("skillCheckFoundUpdates", {
    							count: updatableCount,
    						}),
    					);
    				}
    			},
    			onError: () => toast.danger(t("skillUpdateCheckError")),
    		}),
    	);

    	const { data: cachedUpdateChecks } = useQuery({
    		queryKey: checkSkillUpdatesQueryKey(checkUpdatesParams),
    		queryFn: () => api.skills.checkUpdates(checkUpdatesParams),
    		enabled: false,
    	});

    	const updateStatuses = useMemo(
    		() =>
    			new Map<string, SkillUpdateResponse>(
    				(cachedUpdateChecks ?? autoCheckData ?? []).map((s) => [
    					s.name,
    					s,
    				]),
    			),
    		[cachedUpdateChecks, autoCheckData],
    	);

    	// ── Last-checked label ──
    	const lastCheckedLabel = useMemo(() => {
    		if (!lastCheckedAt) return null;
    		const diffMs = Date.now() - lastCheckedAt;
    		const diffMin = Math.floor(diffMs / 60_000);
    		if (diffMin < 1) return t("lastCheckedJustNow");
    		if (diffMin < 60)
    			return t("lastCheckedMinutes", { count: diffMin });
    		const diffHr = Math.floor(diffMin / 60);
    		return t("lastCheckedHours", { count: diffHr });
    	}, [lastCheckedAt, t]);

    	// ── Agent-view state ──
    	const [searchQuery, setSearchQuery] = useState("");
    	const [selectedKeys, setSelectedKeys] = useState<Set<string>>(
    		() => new Set(),
    	);
    	const [isBulkDeleteDialogOpen, setIsBulkDeleteDialogOpen] =
    		useState(false);
    	const [isMultiSelectMode, setIsMultiSelectMode] = useState(false);
    	const [panelMode, setPanelMode] = useState<
    		"create" | "import" | "import-github" | null
    	>(null);

    	const isRefreshingSkills =
    		isFetching || recheckMutation.isPending || isAutoChecking;

    	const handleRefreshSkills = async () => {
    		await refetch();
    		recheckMutation.mutate(checkUpdatesParams);
    	};

    	const groupedSkills = useMemo(() => {
    		const map = new Map<string, SkillResponse[]>();
    		for (const skill of skills) {
    			const existing = map.get(skill.name) ?? [];
    			map.set(skill.name, [...existing, skill]);
    		}
    		return Array.from(map.entries()).map(([name, items]) => ({
    			name,
    			items,
    			description:
    				items.find((s) => s.description)?.description ?? "",
    		}));
    	}, [skills]);

    	const activeGroup = useMemo(() => {
    		if (selectedSkillName) {
    			return (
    				groupedSkills.find((g) => g.name === selectedSkillName) ??
    				null
    			);
    		}
    		return groupedSkills[0] ?? null;
    	}, [selectedSkillName, groupedSkills]);

    	const selectedGroups = useMemo(
    		() => groupedSkills.filter((g) => selectedKeys.has(g.name)),
    		[selectedKeys, groupedSkills],
    	);

    	const effectiveSelectedKeys = useMemo(() => {
    		if (selectedKeys.size > 0) return selectedKeys;
    		if (activeGroup && !isMultiSelectMode) {
    			return new Set([activeGroup.name]);
    		}
    		return new Set<string>();
    	}, [selectedKeys, activeGroup, isMultiSelectMode]);

    	const handleSelectionChange = (
    		keys: Set<string>,
    		clickedKey?: string,
    	) => {
    		setSelectedKeys(keys);
    		if (clickedKey && !isMultiSelectMode) {
    			void setSelectedSkillName(clickedKey);
    		}
    		if (keys.size > 1 && !isMultiSelectMode) {
    			setIsMultiSelectMode(true);
    		}
    		if (keys.size === 0 && isMultiSelectMode) {
    			setIsMultiSelectMode(false);
    		}
    		setPanelMode(null);
    	};

    	// ── Source-view state ──
    	const [sourceImporting, setSourceImporting] = useState(false);

    	const activeSourceRow = useMemo<SourceRow | null>(() => {
    		// Derive a stub SourceRow from the selectedSourceKey (parsed from query)
    		// The full row data is inside SourceListPanel; we pass the key down
    		// and SourceDetail fetches its own diff. We only need source/scope/projectRoot
    		// which are encoded in the key format "scope:projectRoot:source".
    		if (!selectedSourceKey) return null;
    		const [rowScope, projectRoot, ...sourceParts] =
    			selectedSourceKey.split(":");
    		const source = sourceParts.join(":");
    		if (!source) return null;
    		return {
    			source,
    			sourceUrl: source,
    			sourceType: "git",
    			skillCount: 0,
    			rowScope: rowScope === "project" ? "project" : "global",
    			projectRoot: projectRoot || undefined,
    		} satisfies SourceRow;
    	}, [selectedSourceKey]);

    	// We need the actual sourceUrl from the sources list for SourceDetail.
    	// Re-query sources to get the real row when a key is selected.
    	const { data: projects = [] } = useProjects();
    	const allSourcesQuery = useQueries({
    		queries: [
    			sourcesListQueryOptions({ api, scope: "global" }),
    			...projects.map((p) =>
    				sourcesListQueryOptions({
    					api,
    					scope: "project",
    					projectRoot: p.path,
    				}),
    			),
    		],
    	});
    	const allSourceRows = useMemo<SourceRow[]>(() => {
    		const global = allSourcesQuery[0]?.data as
    			| SourcesListResponse
    			| undefined;
    		const globalRows: SourceRow[] = (global?.sources ?? []).map(
    			(s) => ({
    				...s,
    				rowScope: "global" as const,
    			}),
    		);
    		const projectRows: SourceRow[] = projects.flatMap((p, i) => {
    			const data = allSourcesQuery[i + 1]?.data as
    				| SourcesListResponse
    				| undefined;
    			return (data?.sources ?? []).map((s) => ({
    				...s,
    				rowScope: "project" as const,
    				projectRoot: p.path,
    				projectName: p.name,
    			}));
    		});
    		return [...globalRows, ...projectRows];
    	}, [allSourcesQuery, projects]);

    	const resolvedSourceRow = useMemo(
    		() =>
    			selectedSourceKey
    				? (allSourceRows.find(
    						(r) => sourceRowKey(r) === selectedSourceKey,
    					) ?? null)
    				: null,
    		[selectedSourceKey, allSourceRows],
    	);

    	return (
    		<div className="flex h-full flex-col">
    			{/* Page header with last-checked and recheck */}
    			<div className="flex shrink-0 items-center justify-between gap-3 border-b border-border px-4 py-2.5">
    				<h1 className="text-base font-semibold text-foreground">
    					{t("skillCenter")}
    				</h1>
    				<div className="flex items-center gap-2">
    					{lastCheckedLabel && (
    						<span className="text-xs text-muted">
    							{lastCheckedLabel}
    						</span>
    					)}
    					<Tooltip delay={0}>
    						<Button
    							isIconOnly
    							variant="ghost"
    							size="sm"
    							aria-label={t("recheck")}
    							isDisabled={isRefreshingSkills}
    							onPress={() => {
    								void handleRefreshSkills();
    							}}
    						>
    							<ArrowPathIcon
    								className={cn(
    									"size-4",
    									isRefreshingSkills && "animate-spin",
    								)}
    							/>
    						</Button>
    						<Tooltip.Content>{t("recheck")}</Tooltip.Content>
    					</Tooltip>
    				</div>
    			</div>

    			{/* View-by toggle */}
    			<div className="flex shrink-0 items-center gap-3 border-b border-border px-4 py-2">
    				<div className="flex rounded-lg bg-surface-secondary p-0.5">
    					<button
    						type="button"
    						className={cn(
    							"rounded-md px-3 py-1 text-xs font-medium transition-colors",
    							view === "agent"
    								? "bg-white shadow-sm text-foreground"
    								: "text-muted hover:text-foreground",
    						)}
    						onClick={() => handleSetView("agent")}
    					>
    						{t("viewByAgent")}
    					</button>
    					<button
    						type="button"
    						className={cn(
    							"rounded-md px-3 py-1 text-xs font-medium transition-colors",
    							view === "source"
    								? "bg-white shadow-sm text-foreground"
    								: "text-muted hover:text-foreground",
    						)}
    						onClick={() => handleSetView("source")}
    					>
    						{t("viewBySource")}
    					</button>
    				</div>
    			</div>

    			<div className="flex min-h-0 flex-1">
    				{/* Left panel: list */}
    				<div className="relative flex w-80 shrink-0 flex-col border-r border-border">
    					{/* Scope switch */}
    					<ScopeSwitch
    						scope={scope}
    						onScopeChange={handleScopeChange}
    						selectedProjectPath={selectedProjectPath}
    						onProjectChange={setSelectedProjectPath}
    					/>

    					{view === "agent" ? (
    						<>
    							<ListSearchHeader
    								searchValue={searchQuery}
    								onSearchChange={setSearchQuery}
    								placeholder={t("searchSkills")}
    								ariaLabel={t("searchSkills")}
    							>
    								<Tooltip delay={0}>
    									<div
    										role="button"
    										tabIndex={0}
    										className={cn(
    											"flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted transition-colors hover:bg-default hover:text-foreground focus:outline-none focus:ring-2 focus:ring-accent/40",
    											isMultiSelectMode &&
    												"bg-accent/10 text-accent",
    										)}
    										aria-label={
    											isMultiSelectMode
    												? t("doneSelecting")
    												: t("multiSelect")
    										}
    										onClick={() => {
    											setIsMultiSelectMode(
    												(prev) => !prev,
    											);
    											if (isMultiSelectMode) {
    												handleSelectionChange(
    													new Set(),
    												);
    											}
    										}}
    										onKeyDown={(event) => {
    											if (
    												event.key !== "Enter" &&
    												event.key !== " "
    											) {
    												return;
    											}
    											event.preventDefault();
    											setIsMultiSelectMode(
    												(prev) => !prev,
    											);
    											if (isMultiSelectMode) {
    												handleSelectionChange(
    													new Set(),
    												);
    											}
    										}}
    									>
    										{isMultiSelectMode ? (
    											<CheckCircleIcon className="size-4" />
    										) : (
    											<RectangleStackIcon className="size-4" />
    										)}
    									</div>
    									<Tooltip.Content>
    										{isMultiSelectMode
    											? t("doneSelecting")
    											: t("multiSelect")}
    									</Tooltip.Content>
    								</Tooltip>
    								<Dropdown>
    									<Button
    										isIconOnly
    										variant="ghost"
    										size="sm"
    										className="shrink-0"
    										aria-label={t("addSkill")}
    									>
    										<PlusIcon className="size-4" />
    									</Button>
    									<Dropdown.Popover placement="bottom end">
    										<Dropdown.Menu
    											onAction={(key) => {
    												if (key === "create") {
    													setSelectedKeys(
    														new Set(),
    													);
    													void setSelectedSkillName(
    														null,
    													);
    													setPanelMode("create");
    												} else if (
    													key === "import"
    												) {
    													setSelectedKeys(
    														new Set(),
    													);
    													void setSelectedSkillName(
    														null,
    													);
    													setPanelMode("import");
    												} else if (
    													key === "import-github"
    												) {
    													setSelectedKeys(
    														new Set(),
    													);
    													void setSelectedSkillName(
    														null,
    													);
    													setPanelMode(
    														"import-github",
    													);
    												}
    											}}
    										>
    											<Dropdown.Item
    												id="create"
    												textValue={t(
    													"createCustomSkill",
    												)}
    											>
    												{t("createCustomSkill")}
    											</Dropdown.Item>
    											<Dropdown.Item
    												id="import"
    												textValue={t(
    													"importFromFile",
    												)}
    											>
    												{t("importFromFile")}
    											</Dropdown.Item>
    											<Dropdown.Item
    												id="import-github"
    												textValue={t(
    													"importRemoteSource",
    												)}
    											>
    												{t("importRemoteSource")}
    											</Dropdown.Item>
    										</Dropdown.Menu>
    									</Dropdown.Popover>
    								</Dropdown>
    							</ListSearchHeader>

    							<SkillList
    								skills={skills}
    								selectedKeys={effectiveSelectedKeys}
    								searchQuery={searchQuery}
    								onSelectionChange={handleSelectionChange}
    								selectionMode="multiple"
    								isMultiSelectMode={isMultiSelectMode}
    								groupBySource
    								projectPath={
    									scope === "project"
    										? (selectedProjectPath ?? undefined)
    										: undefined
    								}
    								updateStatuses={updateStatuses}
    							/>

    							{isMultiSelectMode && selectedKeys.size > 0 && (
    								<MultiSelectFloatingBar
    									selectedCount={selectedKeys.size}
    									totalCount={groupedSkills.length}
    									onDelete={() =>
    										setIsBulkDeleteDialogOpen(true)
    									}
    								/>
    							)}
    						</>
    					) : (
    						<>
    							<ListSearchHeader
    								searchValue={searchQuery}
    								onSearchChange={setSearchQuery}
    								placeholder={t("searchSources")}
    								ariaLabel={t("searchSources")}
    							/>
    							<SourceListPanel
    								scope={scope}
    								projectPath={selectedProjectPath}
    								selectedKey={selectedSourceKey}
    								onSelectKey={(key) => {
    									void setSelectedSourceKey(key);
    									setSourceImporting(false);
    								}}
    								searchQuery={searchQuery}
    								updateStatuses={updateStatuses}
    							/>
    						</>
    					)}
    				</div>

    				{/* Right panel: detail */}
    				<div className="flex-1 overflow-hidden relative">
    					{view === "agent" ? (
    						panelMode === "create" ? (
    							<CreateSkillPanel
    								onDone={() => setPanelMode(null)}
    							/>
    						) : panelMode === "import" ? (
    							<ImportSkillPanel
    								onDone={() => setPanelMode(null)}
    							/>
    						) : panelMode === "import-github" ? (
    							<ImportGithubSkillPanel
    								onDone={() => setPanelMode(null)}
    							/>
    						) : activeGroup ? (
    							<SkillDetail group={activeGroup} />
    						) : (
    							<div className="flex h-full flex-col items-center justify-center gap-4">
    								<p className="text-sm text-muted">
    									{t("selectSkill")}
    								</p>
    							</div>
    						)
    					) : resolvedSourceRow ? (
    						sourceImporting ? (
    							<ImportGithubSkillPanel
    								initialUrl={resolvedSourceRow.sourceUrl}
    								projectPath={
    									resolvedSourceRow.rowScope === "project"
    										? resolvedSourceRow.projectRoot
    										: undefined
    								}
    								onDone={() => {
    									setSourceImporting(false);
    									void queryClient.invalidateQueries({
    										queryKey:
    											queryKeys.skills.sources.all(),
    									});
    								}}
    							/>
    						) : (
    							<SourceDetail
    								row={resolvedSourceRow}
    								onImport={() => setSourceImporting(true)}
    							/>
    						)
    					) : (
    						<div className="flex h-full flex-col items-center justify-center gap-4">
    							<p className="text-sm text-muted">
    								{t("selectSource")}
    							</p>
    						</div>
    					)}

    					<BulkDeleteDialog
    						isOpen={isBulkDeleteDialogOpen}
    						onClose={() => setIsBulkDeleteDialogOpen(false)}
    						groups={selectedGroups.map((g) => ({
    							key: g.name,
    							items: g.items,
    						}))}
    						onSuccess={() => {
    							handleSelectionChange(new Set());
    							void refetch();
    						}}
    						resourceType="skill"
    					/>
    				</div>
    			</div>
    		</div>
    	);
    }
    ```

    ASSUMPTION: `skillListQueryOptions` accepts `{ api, scope, projectRoot? }` — the current signature only accepts `{ api, scope }`. If `projectRoot` is missing, add it (or verify from the actual options function signature in `requests/skills.ts`). ASSUMPTION: `checkSkillUpdatesQueryOptions` (added in Step 5.1) is exported from `requests/skills.ts`. ASSUMPTION: `sourcesListQueryOptions` in `requests/sources.ts` already accepts `{ api, scope, projectRoot? }` (confirmed from old SourcesPage usage).

- [ ] **Step 5.3 — Verify TypeScript compiles**

    ```bash
    cd crates/desktop && bun run build 2>&1 | head -40
    ```

    Expected: zero TS errors. Fix any `SkillListQueryOptions` mismatches found.

- [ ] **Step 5.4 — Delete old `pages/sources/index.tsx`**

    ```bash
    rm /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/src/pages/sources/index.tsx
    ```

    Then re-run build to confirm nothing else imports it:

    ```bash
    cd crates/desktop && bun run build 2>&1 | grep "sources/index\|SourcesPage"
    ```

    Expected: no errors (only `App.tsx` imported it and that was already fixed in Task 2).

- [ ] **Step 5.5 — Commit**

    ```bash
    git add crates/desktop/src/pages/settings/skills.tsx \
            crates/desktop/src/requests/skills.ts
    git rm crates/desktop/src/pages/sources/index.tsx
    git commit -m "$(cat <<'EOF'
    feat(desktop): unified skills page — view-by toggle, scope switch, nuqs URL, auto-check query

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Task 6: i18n — add all new keys across en/zh-Hant/zh-Hans

**Files:**

- Modify: `crates/desktop/src/lib/locales/en.ts`
- Modify: `crates/desktop/src/lib/locales/zh-Hant.ts`
- Modify: `crates/desktop/src/lib/locales/zh-Hans.ts`

New keys needed (not already existing):

- `skillCenter`, `viewByAgent`, `viewBySource`
- `lastCheckedJustNow`, `lastCheckedMinutes`, `lastCheckedHours`, `recheck`
- `skillCheckAllLatest`, `skillCheckFoundUpdates`
- `scopeSwitchGlobal`, `scopeSwitchProject`
- `summaryUpdatable`, `summaryInstallable`, `summaryRenamed`, `summaryUnchecked`, `summaryLatest`
- `sourceNeedsAction`, `sourceAllLatest`
- `credentialBind`

Reusable (already exist): `checkForSkillUpdates`, `sourceStateOutdated`, `skillRenamedBadge`, `needsCredential`, `selectSource`, `selectSkill`, `searchSources`, `scopeGlobal`, `scopeProject`.

- [ ] **Step 6.1 — Add keys to `en.ts`**

    In `en.ts`, before the closing `};` (after the last existing entry `selectSource: "Select a source"`), add:

    ```ts
    	// Phase 2 — Unified skills page
    	skillCenter: "Skills",
    	viewByAgent: "By Agent",
    	viewBySource: "By Source",
    	lastCheckedJustNow: "Just checked",
    	lastCheckedMinutes: "Checked {{count}}m ago",
    	lastCheckedHours: "Checked {{count}}h ago",
    	recheck: "Check for updates",
    	skillCheckAllLatest: "All skills are up to date.",
    	skillCheckFoundUpdates: "{{count}} skill(s) have updates available.",
    	scopeSwitchGlobal: "Global",
    	scopeSwitchProject: "Project",
    	summaryUpdatable: "{{count}} updatable",
    	summaryInstallable: "{{count}} installable",
    	summaryRenamed: "{{count}} renamed",
    	summaryUnchecked: "{{count}} uncheckable",
    	summaryLatest: "{{count}} latest",
    	sourceNeedsAction: "Needs Action",
    	sourceAllLatest: "All skills from this source are up to date.",
    	credentialBind: "Bind credential",
    ```

- [ ] **Step 6.2 — Add keys to `zh-Hant.ts`**

    After the last entry in `zh-Hant.ts`, add:

    ```ts
    	// Phase 2 — Unified skills page
    	skillCenter: "技能",
    	viewByAgent: "依 Agent",
    	viewBySource: "依來源",
    	lastCheckedJustNow: "剛剛檢查",
    	lastCheckedMinutes: "{{count}} 分鐘前檢查",
    	lastCheckedHours: "{{count}} 小時前檢查",
    	recheck: "重新檢查",
    	skillCheckAllLatest: "所有技能都是最新的。",
    	skillCheckFoundUpdates: "{{count}} 個技能有可用更新。",
    	scopeSwitchGlobal: "全域",
    	scopeSwitchProject: "專案",
    	summaryUpdatable: "{{count}} 可更新",
    	summaryInstallable: "{{count}} 可安裝",
    	summaryRenamed: "{{count}} 已改名",
    	summaryUnchecked: "{{count}} 無法檢查",
    	summaryLatest: "{{count}} 最新",
    	sourceNeedsAction: "需要動作",
    	sourceAllLatest: "此來源的所有技能都是最新的。",
    	credentialBind: "綁定憑證",
    ```

- [ ] **Step 6.3 — Add keys to `zh-Hans.ts`**

    After the last entry in `zh-Hans.ts`, add:

    ```ts
    	// Phase 2 — Unified skills page
    	skillCenter: "技能",
    	viewByAgent: "按 Agent",
    	viewBySource: "按来源",
    	lastCheckedJustNow: "刚刚检查",
    	lastCheckedMinutes: "{{count}} 分钟前检查",
    	lastCheckedHours: "{{count}} 小时前检查",
    	recheck: "重新检查",
    	skillCheckAllLatest: "所有技能都是最新的。",
    	skillCheckFoundUpdates: "{{count}} 个技能有可用更新。",
    	scopeSwitchGlobal: "全局",
    	scopeSwitchProject: "项目",
    	summaryUpdatable: "{{count}} 可更新",
    	summaryInstallable: "{{count}} 可安装",
    	summaryRenamed: "{{count}} 已改名",
    	summaryUnchecked: "{{count}} 无法检查",
    	summaryLatest: "{{count}} 最新",
    	sourceNeedsAction: "需要操作",
    	sourceAllLatest: "此来源的所有技能都是最新的。",
    	credentialBind: "绑定凭据",
    ```

- [ ] **Step 6.4 — Verify TypeScript compiles (locale types are checked)**

    ```bash
    cd crates/desktop && bun run build 2>&1 | grep "locale\|i18n\|translation"
    ```

    Expected: no errors.

- [ ] **Step 6.5 — Commit**

    ```bash
    git add crates/desktop/src/lib/locales/en.ts \
            crates/desktop/src/lib/locales/zh-Hant.ts \
            crates/desktop/src/lib/locales/zh-Hans.ts
    git commit -m "$(cat <<'EOF'
    feat(desktop/i18n): add Phase 2 i18n keys for unified skills page

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Task 7: Onboarding — update nav-skills tour copy

**Files:**

- Modify: `crates/desktop/src/components/onboarding-controller.tsx`
- Modify: `crates/desktop/src/lib/locales/en.ts`
- Modify: `crates/desktop/src/lib/locales/zh-Hant.ts`
- Modify: `crates/desktop/src/lib/locales/zh-Hans.ts`

The `onboardingSkillsDescription` key (used at line 325 in `onboarding-controller.tsx`) needs updating to mention the Agent/Source toggle, since Sources is now inside the Skills page.

- [ ] **Step 7.1 — Update `onboardingSkillsDescription` in `en.ts`**

    Find and replace the existing key in `en.ts`:

    OLD:

    ```ts
    onboardingSkillsDescription:
    	"Review local skills, inspect their contents, and manage what is already available to your agents.",
    ```

    NEW:

    ```ts
    onboardingSkillsDescription:
    	"Browse and manage skills by Agent or by Source. Run update checks, install new skills, and handle renamed or removed entries — all in one place.",
    ```

- [ ] **Step 7.2 — Update `onboardingSkillsDescription` in `zh-Hant.ts`**

    Find and replace:

    OLD:

    ```ts
    onboardingSkillsDescription:
    	"檢視本機技能、查看其內容，並管理已提供給 Agent 的技能。",
    ```

    NEW:

    ```ts
    onboardingSkillsDescription:
    	"依 Agent 或依來源瀏覽與管理技能。在同一個頁面執行更新檢查、安裝新技能，並處理已改名或已移除的條目。",
    ```

    ASSUMPTION: The zh-Hant text was roughly reconstructed from the UI pattern — verify the actual existing value matches before replacing.

- [ ] **Step 7.3 — Update `onboardingSkillsDescription` in `zh-Hans.ts`**

    Find and replace the equivalent key in `zh-Hans.ts` with:

    ```ts
    onboardingSkillsDescription:
    	"按 Agent 或按来源浏览和管理技能。在同一页面执行更新检查、安装新技能，并处理已改名或已删除的条目。",
    ```

- [ ] **Step 7.4 — Verify TypeScript compiles**

    ```bash
    cd crates/desktop && bun run build 2>&1 | head -20
    ```

    Expected: zero errors.

- [ ] **Step 7.5 — Commit**

    ```bash
    git add crates/desktop/src/lib/locales/en.ts \
            crates/desktop/src/lib/locales/zh-Hant.ts \
            crates/desktop/src/lib/locales/zh-Hans.ts
    git commit -m "$(cat <<'EOF'
    docs(desktop/onboarding): update nav-skills tour copy for merged page

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Task 8: Final integration verification

**Files:** Read-only verification — no file edits.

- [ ] **Step 8.1 — Full bun build passes**

    ```bash
    cd crates/desktop && bun run build 2>&1
    ```

    Expected: exits 0 with no TypeScript errors.

- [ ] **Step 8.2 — Confirm `/sources` redirect is in place**

    ```bash
    grep -n "sources" crates/desktop/src/App.tsx
    ```

    Expected: only the `<Redirect to="/skills?view=source" />` line and no `SourcesPage` import.

- [ ] **Step 8.3 — Confirm `sources` not in sidebar types**

    ```bash
    grep "sources" crates/desktop/src/lib/store/types.ts
    ```

    Expected: no output (the constant is removed).

- [ ] **Step 8.4 — Confirm `SourceCredentialBindingDialog` is mounted in source detail**

    ```bash
    grep "SourceCredentialBindingDialog" crates/desktop/src/components/source-detail.tsx | wc -l
    ```

    Expected: at least 2 lines (import + mount).

- [ ] **Step 8.5 — Confirm nuqs params in unified page**

    ```bash
    grep "useQueryState" crates/desktop/src/pages/settings/skills.tsx
    ```

    Expected: lines for `view`, `skill`, `source` params.

- [ ] **Step 8.6 — Confirm store migration wires correctly**

    ```bash
    grep "migrateV6ToV7\|version < 7" crates/desktop/src/lib/store/migrations/index.ts
    ```

    Expected: both strings present.

- [ ] **Step 8.7 — Commit any fixups, then tag phase complete**

    ```bash
    git add -p  # stage any remaining fixups
    git commit -m "$(cat <<'EOF'
    fix(desktop): phase 2 integration fixups

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Dependencies & Sequencing

- **Depends on Phase 1** (SkillStatusBadge): the `SkillList` component in Task 5 renders `SkillUpdateBadge` items from Phase 1's new badge. The unified page will work without Phase 1 (the badge just shows old behavior) but the full spec intent requires Phase 1 first.
- Tasks 1, 2, 3 are independent and can be done in parallel.
- Task 4 (`SourceDetail`) depends on Task 3 (`SourceSkillRow`).
- Task 5 (unified page rewrite) depends on Tasks 2, 3, 4.
- Tasks 6, 7 (i18n/onboarding) depend on Task 5 to know which keys are actually used.
- Task 8 (verification) depends on all prior tasks.

---

## Open Assumptions

1. **`skillListQueryOptions` `projectRoot` param**: Current `skills.tsx` only passes `scope: "global"`. If the existing `skillListQueryOptions` doesn't accept `projectRoot`, add it to `requests/skills.ts` before using it in Task 5.

2. **`checkSkillUpdatesQueryOptions` export**: Step 5.1 adds this function. If `queryOptions` from `@tanstack/react-query` is not yet imported in `requests/skills.ts`, add the import.

3. **`ScopeSwitch` button `isDisabled` prop**: HeroUI v3 `<button>` does not accept `isDisabled`; use the `disabled` HTML attribute instead. Verify against `.heroui-docs/react/components/(buttons)/button.mdx` before implementing the scope switch.

4. **`SourceDetail` `SourceRow` interface**: The component declares its own `SourceRow` interface (with `sourceUrl`). The unified page also defines `SourceRow`. These should be consolidated — either export from `components/source-detail.tsx` and import in the page, or move to a shared `lib/source-types.ts`. The plan uses the former (export from `source-detail.tsx`).

5. **`zh-Hant` existing `onboardingSkillsDescription` text**: The plan assumes the original text — verify with `grep "onboardingSkillsDescription" crates/desktop/src/lib/locales/zh-Hant.ts` before replacing.

6. **`retry` i18n key**: `SourceEmptyState` uses `t("retry")`. Verify this key exists in `en.ts` (it is used in the old sources page so should exist, but confirm).

7. **Offline auto-check (OQ3/OQ8 from spec)**: The auto-check `useQuery` with `staleTime` will still fire on page entry even offline. A full offline guard (detect network, suppress check) is listed as a post-Phase 2 hardening item — not in scope for this phase but documented here for the next planner.

8. **`SkillStatusBadge` in `SkillList`**: The list still renders `SkillUpdateBadge` (Phase 1 name) — Phase 1 must land first for the badge to show correctly. This plan does not change the badge component.
