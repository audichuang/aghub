# Phase 1 — Update Display Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ] ) syntax for tracking.

**Goal:** Replace the "green-every-skill" update badge with `SkillStatusBadge` (null for current/unchecked), add auto-check-on-enter via `useQuery+staleTime`, a "last checked X ago" timestamp, toast summary on completion, and wire `onResolveAuth` through the skill list.

**Architecture:** `checkSkillUpdatesMutationOptions` (currently a `useMutation`, cannot self-trigger) is joined by a new `checkSkillUpdatesQueryOptions` that wraps the same API call as a `useQuery` with `staleTime=600_000` (10 min) and `enabled` conditional on `navigator.onLine`; `skills.tsx` switches to consume this query for auto-check while keeping the manual refresh button. `SkillUpdateBadge` is renamed to `SkillStatusBadge` in-place, with `upToDate`/`installedCurrent` and `undefined` both returning `null`; `SkillList` gains an `onResolveAuth` prop threaded to the badge. Check completion time is stored in a React `useState` and displayed as a relative timestamp in the list header.

**Tech Stack:** React 19 / HeroUI v3 / Tailwind v4 / TanStack Query (`useQuery`, `useMutation`) / TypeScript strict / bun

---

## ⚠️ Codex 審查修正（實作前必讀；覆寫下方對應步驟）

> GPT-5.5 對著真實程式碼審查後的必改項。判定：**fix-then-ship**。已確認 OK：check-updates 轉 `useQuery` 可行（`queryOptions` 已 import、`skillListQueryOptions` 已收 `projectRoot`、`checkSkillUpdatesQueryKey` 存在）、TanStack Query v5 `dataUpdatedAt` 為真。

- **[Cross-cutting] HeroUI Chip 顏色（重要）**：現有 `skill-update-badge.tsx` 慣例是 `<Chip variant="soft" color="default">` + 用 Tailwind **text 類別**上色（`text-warning`/`text-success`/`text-secondary`/`text-muted`），**不是** `color="warning"/"success"/"secondary"`。所有 badge 步驟改用既有慣例（`.heroui-docs/react` 在此 worktree 不存在，以現有程式碼為準）。
- **[P1] `onResolveAuth` 簽名（~:249）**：全程統一為 `onResolveAuth?: (skillName: string) => void`；在 `SkillList`（`skill-list.tsx:37,294`）傳 `onResolveAuth ? () => onResolveAuth(skillGroup.name) : undefined` 給 `SkillStatusBadge`。（注意：File Structure 表寫的 `() => void` 要一起改成帶 skillName。）
- **[P1] 手動 refresh 重複打網路（~:712）**：現步驟先 `invalidateQueries` 再跑 mutation 會雙重 check。改為 `const handleRefreshSkills = async () => { await refetch(); checkUpdatesMutation.mutate(updateCheckParams); };` 並移除 `invalidateQueries`（mutation onSuccess 已寫 query data，`requests/skills.ts:360`）。
- **[P1] i18n 漏 zh-Hans（~:455）**：新 key 必須同時加 `en.ts`、`zh-Hant.ts`、`zh-Hans.ts`（`i18n.ts:34` 有註冊 zh-Hans）。
- **[P2] toast 與「靜默自動跑」矛盾（~:5 Goal）**：定案「**手動 recheck** 顯示 toast（mutation onSuccess）；**進頁自動 check 靜默無 toast**」。Goal 改成這樣；自動 check 要跳 toast 需另加 QueryCache subscription（本 phase 不做）。

## File Structure

| File                                                   | Action     | Responsibility                                                                                                                                                                                             |
| ------------------------------------------------------ | ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/desktop/src/components/skill-update-badge.tsx` | **Modify** | Rename export to `SkillStatusBadge`; return `null` for `upToDate`/`installedCurrent`/undefined; keep `uncheckable(auth)` actionable; remove green chip; `renamed` → purple "已改名"                        |
| `crates/desktop/src/components/skill-list.tsx`         | **Modify** | Add `onResolveAuth?: () => void` to `SkillListProps`; pass it to `SkillStatusBadge` in `renderSkillItem`                                                                                                   |
| `crates/desktop/src/requests/skills.ts`                | **Modify** | Add `checkSkillUpdatesQueryOptions` (useQuery-compatible, `staleTime=600_000`); keep existing `checkSkillUpdatesMutationOptions` for manual refresh                                                        |
| `crates/desktop/src/pages/settings/skills.tsx`         | **Modify** | Switch auto-check to `useQuery` with `enabled=navigator.onLine`; add `lastCheckedAt: Date\|null` state; render "上次檢查 X 前" in header; fire toast on query success; pass `onResolveAuth` to `SkillList` |
| `crates/desktop/src/lib/locales/en.ts`                 | **Modify** | Add new i18n keys: `lastChecked`, `lastCheckedAgo`, `skillCheckComplete`, `skillCheckCompleteAllGood`, `recheck`                                                                                           |
| `crates/desktop/src/lib/locales/zh-Hant.ts`            | **Modify** | Add same keys in Traditional Chinese                                                                                                                                                                       |

---

### Task 1: Rename + rewrite `SkillStatusBadge` (no green, correct null semantics)

**Files:**

- Modify: `crates/desktop/src/components/skill-update-badge.tsx`

- [ ] **Step 1.1 — Read current file to lock exact import names.**
      Verify `SkillUpdateResponse` is imported from `../generated/dto` and `Chip`, `Tooltip` are from `@heroui/react`. (Already confirmed in reading above — proceed.)

- [ ] **Step 1.2 — Rewrite `skill-update-badge.tsx` in full.**
      Replace the entire file content. The new component is called `SkillStatusBadge`; the old export `SkillUpdateBadge` is re-exported as an alias so existing call sites compile while Task 3 updates them:

    ```tsx
    import {
    	ExclamationTriangleIcon,
    	LockClosedIcon,
    	QuestionMarkCircleIcon,
    } from "@heroicons/react/24/solid";
    import { Chip, Tooltip } from "@heroui/react";
    import { useTranslation } from "react-i18next";
    import type { SkillUpdateResponse } from "../generated/dto";

    interface SkillStatusBadgeProps {
    	/**
    	 * Per-skill status from `GET /skills/check-updates`, or undefined when
    	 * no check has run yet. `undefined` and `upToDate` both render nothing —
    	 * only actionable states render a badge (§12-C5: no version/date in
    	 * Phase 1; hash tooltip only).
    	 */
    	status?: SkillUpdateResponse;
    	/** Called when an `uncheckable { reason: "auth" }` badge is activated. */
    	onResolveAuth?: () => void;
    }

    /** Human-readable tooltip key for an `uncheckable` reason. */
    function uncheckableTooltipKey(reason: string): string {
    	switch (reason) {
    		case "auth":
    			return "skillUncheckableAuth";
    		case "network":
    			return "skillUncheckableNetwork";
    		case "local":
    			return "skillUncheckableLocal";
    		case "ssh":
    		case "unsupportedScheme":
    			return "skillUncheckableUnsupported";
    		case "noPath":
    			return "skillUncheckableNoPath";
    		case "timeout":
    			return "skillUncheckableTimeout";
    		default:
    			return "skillUncheckableGeneric";
    	}
    }

    /**
     * Renders a skill's update status as a compact HeroUI Chip + Tooltip.
     *
     * - `undefined` → null (check not run yet)
     * - `upToDate` → null (no visual noise for the common case; §spec D3)
     * - `updateAvailable` → yellow "可更新"
     * - `renamed` → purple "已改名"
     * - `uncheckable(auth)` → grey "綁定憑證" button (always, even in list)
     * - `uncheckable(other)` → grey "無法檢查"
     *
     * Phase 1 only shows hash in tooltip (no version/date — §12-C5).
     */
    export function SkillStatusBadge({
    	status,
    	onResolveAuth,
    }: SkillStatusBadgeProps) {
    	const { t } = useTranslation();

    	// undefined = no check run; upToDate = all good → both silent
    	if (!status || status.status === "upToDate") {
    		return null;
    	}

    	if (status.status === "updateAvailable") {
    		return (
    			<Tooltip delay={0}>
    				<Tooltip.Trigger>
    					<span className="inline-flex">
    						<Chip size="sm" variant="soft" color="warning">
    							<span className="flex items-center gap-1 text-xs">
    								{t("skillUpdateAvailableBadge")}
    							</span>
    						</Chip>
    					</span>
    				</Tooltip.Trigger>
    				<Tooltip.Content>
    					{t("skillUpdateAvailableTooltip", {
    						current: status.current.slice(0, 8),
    						available: status.available.slice(0, 8),
    					})}
    				</Tooltip.Content>
    			</Tooltip>
    		);
    	}

    	if (status.status === "renamed") {
    		return (
    			<Tooltip delay={0}>
    				<Tooltip.Trigger>
    					<span className="inline-flex">
    						<Chip size="sm" variant="soft" color="secondary">
    							<span className="flex items-center gap-1 text-xs">
    								{t("skillRenamedBadge")}
    							</span>
    						</Chip>
    					</span>
    				</Tooltip.Trigger>
    				<Tooltip.Content>
    					{t("skillRenamedTooltip", {
    						newName: status.newName,
    					})}
    				</Tooltip.Content>
    			</Tooltip>
    		);
    	}

    	// uncheckable
    	const reason = status.reason;
    	const tooltipText = t(uncheckableTooltipKey(reason));

    	// auth: ALWAYS show "綁定憑證" button — even in list (§12-C5 / §4.2)
    	if (reason === "auth") {
    		return (
    			<Tooltip delay={0}>
    				<Tooltip.Trigger>
    					<button
    						type="button"
    						onClick={onResolveAuth}
    						className="inline-flex cursor-pointer"
    						disabled={!onResolveAuth}
    					>
    						<Chip size="sm" variant="tertiary" color="default">
    							<span className="flex items-center gap-1 text-xs">
    								<LockClosedIcon className="size-3 text-muted" />
    								{t("skillNeedsCredential")}
    							</span>
    						</Chip>
    					</button>
    				</Tooltip.Trigger>
    				<Tooltip.Content>
    					{t("skillNeedsCredentialTooltip")}
    				</Tooltip.Content>
    			</Tooltip>
    		);
    	}

    	return (
    		<Tooltip delay={0}>
    			<Tooltip.Trigger>
    				<span className="inline-flex">
    					<Chip size="sm" variant="tertiary" color="default">
    						<span className="flex items-center gap-1 text-xs">
    							<QuestionMarkCircleIcon className="size-3 text-muted" />
    							<span className="text-muted">
    								{t("skillUncheckable")}
    							</span>
    						</span>
    					</Chip>
    				</span>
    			</Tooltip.Trigger>
    			<Tooltip.Content>{tooltipText}</Tooltip.Content>
    		</Tooltip>
    	);
    }

    /** @deprecated Use `SkillStatusBadge` instead. Kept for migration. */
    export const SkillUpdateBadge = SkillStatusBadge;
    ```

    ASSUMPTION: HeroUI v3 `<Chip color="warning">` and `<Chip color="secondary">` are valid color props (yellow for warning, purple for secondary). Verify in `.heroui-docs/react/components/(data-display)/chip.mdx` before running — adjust color tokens if needed. The `color="secondary"` renders with purple/violet accent in the default HeroUI v3 palette.

- [ ] **Step 1.3 — TypeScript compile check.**

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit 2>&1 | head -40
    ```

    Expected: zero errors related to `skill-update-badge.tsx`. (Other pre-existing errors are out of scope.)

- [ ] **Step 1.4 — Commit.**

    ```bash
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      add crates/desktop/src/components/skill-update-badge.tsx
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      commit -m "$(cat <<'EOF'
    feat(desktop): rewrite SkillStatusBadge — null for upToDate, no green noise

    upToDate and undefined both return null. updateAvailable → yellow chip,
    renamed → purple chip, uncheckable(auth) always shows credential button.
    Old SkillUpdateBadge kept as deprecated re-export for migration.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 2: Wire `onResolveAuth` through `SkillList`

**Files:**

- Modify: `crates/desktop/src/components/skill-list.tsx`

- [ ] **Step 2.1 — Update `SkillListProps` interface.**
      Add `onResolveAuth?: () => void` after `updateStatuses`:

    Old block (lines 37–48):

    ```tsx
    interface SkillListProps {
    	skills: SkillResponse[];
    	selectedKeys: Set<string>;
    	searchQuery: string;
    	onSelectionChange: (keys: Set<string>, clickedKey?: string) => void;
    	emptyMessage?: string;
    	groupBySource?: boolean;
    	projectPath?: string;
    	selectionMode?: "none" | "single" | "multiple";
    	isMultiSelectMode?: boolean;
    	updateStatuses?: ReadonlyMap<string, SkillUpdateResponse>;
    }
    ```

    New block:

    ```tsx
    interface SkillListProps {
    	skills: SkillResponse[];
    	selectedKeys: Set<string>;
    	searchQuery: string;
    	onSelectionChange: (keys: Set<string>, clickedKey?: string) => void;
    	emptyMessage?: string;
    	groupBySource?: boolean;
    	projectPath?: string;
    	selectionMode?: "none" | "single" | "multiple";
    	isMultiSelectMode?: boolean;
    	updateStatuses?: ReadonlyMap<string, SkillUpdateResponse>;
    	/** Passed to SkillStatusBadge so auth-uncheckable skills are actionable
    	 * in the list (not just in the detail panel). */
    	onResolveAuth?: () => void;
    }
    ```

- [ ] **Step 2.2 — Destructure `onResolveAuth` in `SkillList` function signature.**
      Old line 61 area:

    ```tsx
    export function SkillList({
    	skills,
    	selectedKeys,
    	searchQuery,
    	onSelectionChange,
    	emptyMessage,
    	groupBySource = false,
    	projectPath,
    	selectionMode = "single",
    	isMultiSelectMode = false,
    	updateStatuses,
    }: SkillListProps) {
    ```

    New:

    ```tsx
    export function SkillList({
    	skills,
    	selectedKeys,
    	searchQuery,
    	onSelectionChange,
    	emptyMessage,
    	groupBySource = false,
    	projectPath,
    	selectionMode = "single",
    	isMultiSelectMode = false,
    	updateStatuses,
    	onResolveAuth,
    }: SkillListProps) {
    ```

- [ ] **Step 2.3 — Update import: replace `SkillUpdateBadge` with `SkillStatusBadge`.**
      Old import line 15:

    ```tsx
    import { SkillUpdateBadge } from "./skill-update-badge";
    ```

    New:

    ```tsx
    import { SkillStatusBadge } from "./skill-update-badge";
    ```

- [ ] **Step 2.4 — Update `renderSkillItem` to pass `onResolveAuth`.**
      In `renderSkillItem` (around line 294), old:

    ```tsx
    <SkillUpdateBadge status={updateStatuses?.get(skillGroup.name)} />
    ```

    New:

    ```tsx
    <SkillStatusBadge
    	status={updateStatuses?.get(skillGroup.name)}
    	onResolveAuth={onResolveAuth}
    />
    ```

- [ ] **Step 2.5 — TypeScript compile check.**

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit 2>&1 | head -40
    ```

    Expected: no new errors.

- [ ] **Step 2.6 — Commit.**

    ```bash
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      add crates/desktop/src/components/skill-list.tsx
    git -C /home/audichuang/research/aghub/.claire/worktrees/skills-sources-merge \
      commit -m "$(cat <<'EOF'
    feat(desktop): thread onResolveAuth through SkillList to SkillStatusBadge

    Auth-uncheckable skills in the list now show an actionable credential
    button (not just in the detail panel). Switches to SkillStatusBadge import.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

    ASSUMPTION: `git -C` path should be the worktree root. Fix `git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge` in the commit command above.

---

### Task 3: Add `checkSkillUpdatesQueryOptions` to `requests/skills.ts`

**Files:**

- Modify: `crates/desktop/src/requests/skills.ts`

The existing `checkSkillUpdatesMutationOptions` is a `useMutation` — it cannot self-trigger on mount (§12-GAP8). We add a parallel `checkSkillUpdatesQueryOptions` as a `useQuery` with `staleTime` for throttle. The mutation is kept for the manual refresh button.

- [ ] **Step 3.1 — Add `CheckSkillUpdatesQueryParams` interface and `checkSkillUpdatesQueryOptions` after line 369 (after `checkSkillUpdatesMutationOptions`).**

    Insert after the closing `}` of `checkSkillUpdatesMutationOptions` (around line 369):

    ```typescript
    interface CheckSkillUpdatesQueryParams {
    	api: ApiClient;
    	/** When false the query does not fire (use to suppress when offline). */
    	enabled?: boolean;
    	params?: CheckSkillUpdatesParams;
    	/**
    	 * Throttle threshold in ms. Default 10 minutes — matches the spec's
    	 * "staleTime = throttle" pattern so React Query skips a re-fetch if the
    	 * last result is younger than this.
    	 *
    	 * §12-C1: preflight is near-zero-cost only in steady-state (ref_commit
    	 * populated, local not drifted). Throttle + offline suppression are
    	 * REQUIRED, not optional.
    	 */
    	staleTime?: number;
    }

    /**
     * `useQuery`-compatible options for `GET /skills/check-updates`.
     *
     * Use this for the **auto-check-on-page-enter** path. The mutation variant
     * (`checkSkillUpdatesMutationOptions`) is kept for the manual refresh
     * button where explicit loading state matters.
     *
     * The check writes back to the skill lock (auto-heals ref_commit/hash) —
     * this side-effect is accepted per spec §4.3.
     */
    export function checkSkillUpdatesQueryOptions({
    	api,
    	enabled = true,
    	params,
    	staleTime = 600_000, // 10 minutes
    }: CheckSkillUpdatesQueryParams) {
    	return queryOptions({
    		queryKey: checkSkillUpdatesQueryKey(params),
    		queryFn: () => api.skills.checkUpdates(params),
    		enabled,
    		staleTime,
    	});
    }
    ```

- [ ] **Step 3.2 — TypeScript compile check.**

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit 2>&1 | head -40
    ```

    Expected: no errors.

- [ ] **Step 3.3 — Commit.**

    ```bash
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      add crates/desktop/src/requests/skills.ts
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      commit -m "$(cat <<'EOF'
    feat(desktop): add checkSkillUpdatesQueryOptions for auto-check-on-enter

    Parallel to the existing mutation, provides a useQuery variant with
    staleTime=10min throttle so skills.tsx can fire the check on page mount
    without useEffect. Offline suppression via enabled=navigator.onLine.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 4: Add i18n keys for last-checked timestamp and toast summary

**Files:**

- Modify: `crates/desktop/src/lib/locales/en.ts`
- Modify: `crates/desktop/src/lib/locales/zh-Hant.ts`

- [ ] **Step 4.1 — Add new keys to `en.ts` in the skill update section (after `skillSyncedSuccessfully` line ~424).**

    Locate the block `// --- Per-skill update check (F1 / Workstream D) ---` (~line 399 in en.ts). After the existing keys in that block, add:

    ```typescript
    	/** Toast when check completes and N skills have updates. */
    	skillCheckCompleteWithUpdates:
    		"Check complete — {{count}} update(s) available",
    	/** Toast when check completes and everything is current. */
    	skillCheckCompleteAllGood: "All skills are up to date",
    	/** List header label. {{time}} is a human-readable relative string
    	 *  like "3 minutes ago" produced by the component. */
    	lastCheckedAgo: "Last checked {{time}} ago",
    	/** Shown when check has never run this session. */
    	lastCheckedNever: "Never checked",
    	/** Button label to re-run the check manually. */
    	recheck: "Recheck",
    ```

    ASSUMPTION: Inserting after `skillNeedsCredentialTooltip` (around en.ts line ~419) keeps the grouping clean. The exact insertion point is after:

    ```typescript
    	skillNeedsCredentialTooltip:
    		"This private source needs a credential. Add one and retry.",
    ```

- [ ] **Step 4.2 — Add same keys to `zh-Hant.ts` in the matching section.**

    Find the block `// --- 單一技能更新檢查（F1 / Workstream D）---` and after `skillNeedsCredentialTooltip` add:

    ```typescript
    	skillCheckCompleteWithUpdates: "檢查完成，{{count}} 個可更新",
    	skillCheckCompleteAllGood: "全部都是最新",
    	lastCheckedAgo: "上次檢查 {{time}} 前",
    	lastCheckedNever: "尚未檢查",
    	recheck: "重新檢查",
    ```

- [ ] **Step 4.3 — TypeScript compile check (strict locale shape).**

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit 2>&1 | head -40
    ```

    Expected: no errors.

    ASSUMPTION: The locale files use a plain `export default { ... }` object and TypeScript infers the shape. If there is a separate type declaration for the locale shape, add the new keys there too. Check by running `grep -rn "skillNeedsCredential" crates/desktop/src/lib/locales/` — if a `.d.ts` or `type Translation` exists, update it.

- [ ] **Step 4.4 — Check zh-Hans locale exists and requires same keys.**

    ```bash
    ls /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/src/lib/locales/
    ```

    If `zh-Hans.ts` exists, add the Simplified Chinese translations too:

    ```typescript
    	skillCheckCompleteWithUpdates: "检查完成，{{count}} 个可更新",
    	skillCheckCompleteAllGood: "全部都是最新版本",
    	lastCheckedAgo: "上次检查 {{time}} 前",
    	lastCheckedNever: "从未检查",
    	recheck: "重新检查",
    ```

- [ ] **Step 4.5 — Commit.**

    ```bash
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      add crates/desktop/src/lib/locales/
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      commit -m "$(cat <<'EOF'
    i18n(desktop): add Phase 1 locale keys for last-checked timestamp + toast

    New keys: skillCheckCompleteWithUpdates, skillCheckCompleteAllGood,
    lastCheckedAgo, lastCheckedNever, recheck. Added to en/zh-Hant/(zh-Hans).

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 5: Rewrite `skills.tsx` — auto-check, timestamp, toast, onResolveAuth

**Files:**

- Modify: `crates/desktop/src/pages/settings/skills.tsx`

This is the largest task. It replaces the manual-only check path with `useQuery` auto-trigger, adds the "last checked" timestamp to the header, and fires a toast summary on completion. The existing manual refresh button (`handleRefreshSkills`) is kept but augmented to also invalidate the query so it re-fires.

- [ ] **Step 5.1 — Add `checkSkillUpdatesQueryOptions` to the import from `../../requests/skills`.**

    Old import around lines 28-33:

    ```tsx
    import {
    	checkSkillUpdatesMutationOptions,
    	checkSkillUpdatesQueryKey,
    	skillListQueryOptions,
    } from "../../requests/skills";
    ```

    New:

    ```tsx
    import {
    	checkSkillUpdatesMutationOptions,
    	checkSkillUpdatesQueryOptions,
    	checkSkillUpdatesQueryKey,
    	skillListQueryOptions,
    } from "../../requests/skills";
    ```

- [ ] **Step 5.2 — Add `useEffect` is banned: use `useCallback` pattern. Add `lastCheckedAt` state and relative-time helper.**

    After the existing `updateCheckParams` memo (~line 38), add:

    ```tsx
    const [lastCheckedAt, setLastCheckedAt] = useState<Date | null>(null);

    /** Returns a human-readable relative string ("3 分鐘", "1 小時", …). */
    function formatRelativeTime(date: Date): string {
    	const diffMs = Date.now() - date.getTime();
    	const diffMin = Math.floor(diffMs / 60_000);
    	if (diffMin < 1) return t("justNow") ?? "just now";
    	if (diffMin < 60) return `${diffMin} 分鐘`;
    	const diffHr = Math.floor(diffMin / 60);
    	if (diffHr < 24) return `${diffHr} 小時`;
    	return `${Math.floor(diffHr / 24)} 天`;
    }
    ```

    ASSUMPTION: A `justNow` key is not yet in locales; if the `< 1 minute` case needs translation, add it in Step 4. The inline fallback string handles the en locale until then. Alternatively, treat "< 1 min" as "剛才" in zh-Hant. Add `justNow: "just now"` / `"剛才"` to locales if required.

- [ ] **Step 5.3 — Replace the manual-only check path with `useQuery` for auto-check.**

    Remove the standalone `checkUpdatesMutation` block and replace with a combined pattern:

    Old code (~lines 46-57):

    ```tsx
    const checkUpdatesMutation = useMutation(
    	checkSkillUpdatesMutationOptions({
    		api,
    		queryClient,
    		onError: () => toast.danger(t("skillUpdateCheckError")),
    	}),
    );
    const { data: cachedUpdateChecks } = useQuery({
    	queryKey: checkSkillUpdatesQueryKey(updateCheckParams),
    	queryFn: () => api.skills.checkUpdates(updateCheckParams),
    	enabled: false,
    });
    ```

    New code (replace the block above with):

    ```tsx
    // Auto-check: fires on mount if data is stale (>10 min) and online.
    // navigator.onLine suppresses the check when offline (§12-OQ8) to avoid
    // turning every skill into uncheckable(network).
    const { data: cachedUpdateChecks, isFetching: isAutoChecking } = useQuery({
    	...checkSkillUpdatesQueryOptions({
    		api,
    		params: updateCheckParams,
    		enabled: navigator.onLine,
    	}),
    	// Fire toast summary when the check result arrives fresh.
    	// TanStack Query calls this on every successful fetch, not on cache hits.
    	// We use select+onSuccess pattern via the query cache subscription below.
    });

    // Manual refresh mutation (keeps explicit isPending state for the button).
    const checkUpdatesMutation = useMutation(
    	checkSkillUpdatesMutationOptions({
    		api,
    		queryClient,
    		onSuccess: (data) => {
    			setLastCheckedAt(new Date());
    			const updateCount = data.filter(
    				(s) => s.status === "updateAvailable",
    			).length;
    			if (updateCount > 0) {
    				toast.info(
    					t("skillCheckCompleteWithUpdates", {
    						count: updateCount,
    					}),
    				);
    			} else {
    				toast.success(t("skillCheckCompleteAllGood"));
    			}
    		},
    		onError: () => toast.danger(t("skillUpdateCheckError")),
    	}),
    );
    ```

    Note: The toast for the auto-check is handled differently (via query `onSuccess` in the `useQueryClient` subscription) — see Step 5.4.

- [ ] **Step 5.4 — Subscribe to query success for auto-check toast and timestamp.**

    The `useQuery` from TanStack Query v5 does not expose an `onSuccess` callback on the options object (removed in v5). Use `useEffect` is banned. Instead, use `queryClient.getQueryCache().subscribe()` pattern — but that requires `useEffect`. The correct v5-compatible approach with no `useEffect` is to use a `select` + derived state via `useMemo`.

    Replace the auto-check `useQuery` block from Step 5.3 with this finalized version:

    ```tsx
    const {
    	data: cachedUpdateChecks,
    	isFetching: isAutoChecking,
    	dataUpdatedAt: checksUpdatedAt,
    } = useQuery({
    	...checkSkillUpdatesQueryOptions({
    		api,
    		params: updateCheckParams,
    		enabled: navigator.onLine,
    	}),
    });

    // Derive lastCheckedAt from the query's dataUpdatedAt timestamp
    // (milliseconds since epoch, 0 when never fetched).
    // This avoids any useState update loop — it's a pure derivation.
    const lastCheckedDate = useMemo(
    	() => (checksUpdatedAt > 0 ? new Date(checksUpdatedAt) : null),
    	[checksUpdatedAt],
    );
    ```

    Remove the `const [lastCheckedAt, setLastCheckedAt] = useState<Date | null>(null)` from Step 5.2 — it is no longer needed. The `lastCheckedDate` derived from `checksUpdatedAt` is the single source of truth.

    For the auto-check toast: use a `useMemo`-derived counter that causes a toast to fire on first render after data changes — but `toast()` is a side-effect and cannot go in `useMemo`. The TanStack Query v5 pattern for "fire side-effect on data change" without `useEffect` is the `meta.onSuccess` callback on the query client's `QueryCache`. We configure this at the **query options level** using the `meta` field and a global cache subscription in the provider — that is infrastructure work.

    **Simpler Phase 1 solution**: the toast fires from the manual mutation's `onSuccess` (already wired in Step 5.3). For the auto-check, accept that the toast only fires on **manual** refresh; auto-check updates the badge silently. This matches the spec's "靜默自動跑" (silent auto-run) language — the spec does not say the auto-check fires a toast, only that a check completing fires a toast. In practice the manual button fires the toast; the auto-check silently updates badges. If user presses "重新檢查" they get the toast.

    ASSUMPTION: "檢查完成" toast fires on manual check only. Auto-check is silent (badge updates via `cachedUpdateChecks`). This matches §4.3 "toast 在 check 完成時回饋摘要" which refers to the user-triggered check flow.

- [ ] **Step 5.5 — Remove the old `[lastCheckedAt, setLastCheckedAt]` state (if added in 5.2 and now superseded by `lastCheckedDate`).**
      The `lastCheckedDate` from `checksUpdatedAt` is the timestamp. The `formatRelativeTime` helper from Step 5.2 still applies.

- [ ] **Step 5.6 — Update `isRefreshingSkills` to include `isAutoChecking`.**

    Old (~line 76):

    ```tsx
    const isRefreshingSkills = isFetching || checkUpdatesMutation.isPending;
    ```

    New:

    ```tsx
    const isRefreshingSkills =
    	isFetching || checkUpdatesMutation.isPending || isAutoChecking;
    ```

- [ ] **Step 5.7 — Update `handleRefreshSkills` to invalidate the query so auto-check re-fires.**

    Old (~lines 78-81):

    ```tsx
    const handleRefreshSkills = async () => {
    	await refetch();
    	checkUpdatesMutation.mutate(updateCheckParams);
    };
    ```

    New:

    ```tsx
    const handleRefreshSkills = async () => {
    	await refetch();
    	// Invalidate the query cache entry so the next manual check
    	// re-fetches even within the staleTime window.
    	await queryClient.invalidateQueries({
    		queryKey: checkSkillUpdatesQueryKey(updateCheckParams),
    	});
    	checkUpdatesMutation.mutate(updateCheckParams);
    };
    ```

- [ ] **Step 5.8 — Add the "上次檢查 X 前" label and "重新檢查" button to the list header.**

    In the `ListSearchHeader` JSX (around lines 154–269), add a small row beneath the header toolbar. The easiest insertion point is INSIDE `<div className="relative flex w-80 shrink-0 flex-col border-r border-border">`, after `</ListSearchHeader>` but before `<SkillList`:

    ```tsx
    {
    	/* Last-checked timestamp row */
    }
    <div className="flex items-center justify-between border-b border-separator px-3 py-1.5">
    	<span className="text-xs text-muted">
    		{lastCheckedDate
    			? t("lastCheckedAgo", {
    					time: formatRelativeTime(lastCheckedDate),
    				})
    			: t("lastCheckedNever")}
    	</span>
    	{!isAutoChecking && (
    		<button
    			type="button"
    			className="text-xs text-accent hover:underline"
    			onClick={() => {
    				void handleRefreshSkills();
    			}}
    		>
    			{t("recheck")}
    		</button>
    	)}
    </div>;
    ```

- [ ] **Step 5.9 — Pass `onResolveAuth` to `<SkillList>`.**

    In the `<SkillList>` call (~line 272), add the prop. The detail panel already handles auth resolution via `setCredDialogOpen` — but `skills.tsx` does not have a credential dialog of its own (that lives in `skill-detail.tsx`). For the list-level auth, the simplest approach is: clicking the credential button in the list selects that skill (bringing up the detail panel where the full dialog is wired). We achieve this by setting `onResolveAuth` to a function that selects the skill name.

    Update the `<SkillList>` call to:

    ```tsx
    <SkillList
    	skills={skills}
    	selectedKeys={effectiveSelectedKeys}
    	searchQuery={searchQuery}
    	onSelectionChange={handleSelectionChange}
    	selectionMode="multiple"
    	isMultiSelectMode={isMultiSelectMode}
    	groupBySource={true}
    	updateStatuses={updateStatuses}
    	onResolveAuth={(skillName?: string) => {
    		// Selecting the skill opens the detail panel where
    		// SourceCredentialBindingDialog is already wired.
    		if (skillName) {
    			setSelectedName(skillName);
    		}
    	}}
    />
    ```

    ASSUMPTION: `SkillList`'s `onResolveAuth` prop (as defined in Task 2) takes no arguments. To pass the skill name we need to extend the prop signature or use a closure per skill. Since `renderSkillItem` already has `skillGroup.name` in scope, the closure approach is cleaner — update `SkillListProps.onResolveAuth` signature to `(skillName: string) => void` and update both the call in `skill-list.tsx` and the caller here.

    **Revised approach (simpler, no signature change):** In `skill-list.tsx`'s `renderSkillItem`, wrap:

    ```tsx
    onResolveAuth={
    	onResolveAuth
    		? () => onResolveAuth(skillGroup.name)
    		: undefined
    }
    ```

    And in `SkillListProps` / `SkillList` signature, keep `onResolveAuth?: (skillName: string) => void`.

    Update Task 2 steps accordingly: the prop type is `onResolveAuth?: (skillName: string) => void`.

- [ ] **Step 5.10 — TypeScript compile check.**

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit 2>&1 | head -60
    ```

    Expected: no errors.

- [ ] **Step 5.11 — Commit.**

    ```bash
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      add crates/desktop/src/pages/settings/skills.tsx
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      commit -m "$(cat <<'EOF'
    feat(desktop/skills): auto-check on enter + last-checked timestamp + toast

    - Replaces manual-only mutation with useQuery (staleTime=10min) for
      silent auto-check on page enter; suppressed when navigator.onLine=false
    - Derives lastCheckedDate from checksUpdatedAt (no useState, no useEffect)
    - Shows "上次檢查 X 前" / "尚未檢查" beneath the list header toolbar
    - Toast summary fires on manual recheck (silent for auto-check per spec)
    - Passes onResolveAuth(skillName) to SkillList so auth-uncheckable
      skills open the detail panel on credential-button click

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 6: Update `skill-detail.tsx` — switch to `SkillStatusBadge`

**Files:**

- Modify: `crates/desktop/src/components/skill-detail.tsx`

The detail panel currently imports `SkillUpdateBadge`. Since Task 1 added the deprecated alias, this compiles today but should be updated.

- [ ] **Step 6.1 — Update import in `skill-detail.tsx`.**
      Old line 38:

    ```tsx
    import { SkillUpdateBadge } from "./skill-update-badge";
    ```

    New:

    ```tsx
    import { SkillStatusBadge } from "./skill-update-badge";
    ```

- [ ] **Step 6.2 — Replace usage in the JSX (around line 487).**
      Old:

    ```tsx
    								{sourceUrl &&
    									(updateStatus ? (
    										<SkillUpdateBadge
    											status={
    												updateStatus
    											}
    											onResolveAuth={() => {
    												if (
    													currentSkillSource.bindingSource
    												) {
    													setCredDialogOpen(
    														true,
    													);
    												}
    											}}
    										/>
    									) : (
    ```

    New:

    ```tsx
    								{sourceUrl &&
    									(updateStatus ? (
    										<SkillStatusBadge
    											status={
    												updateStatus
    											}
    											onResolveAuth={() => {
    												if (
    													currentSkillSource.bindingSource
    												) {
    													setCredDialogOpen(
    														true,
    													);
    												}
    											}}
    										/>
    									) : (
    ```

    Note: The `detail` panel preserves the existing behavior of hiding the badge/button when `updateStatus` is undefined and showing the "Check for updates" button instead. This is consistent with Phase 1 scope (the detail panel has its own manual check).

- [ ] **Step 6.3 — TypeScript compile check.**

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit 2>&1 | head -40
    ```

- [ ] **Step 6.4 — Commit.**

    ```bash
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      add crates/desktop/src/components/skill-detail.tsx
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      commit -m "$(cat <<'EOF'
    refactor(desktop): migrate skill-detail to SkillStatusBadge import

    Removes use of deprecated SkillUpdateBadge alias in the detail panel.
    Behavior is unchanged; only the import name updated.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 7: End-to-end smoke test

**Files:** None modified.

- [ ] **Step 7.1 — Build the desktop frontend.**

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run build 2>&1 | tail -20
    ```

    Expected: build succeeds (exit 0), no TypeScript errors.

- [ ] **Step 7.2 — Verify no remaining `SkillUpdateBadge` usage (except the alias re-export itself).**

    ```bash
    grep -rn "SkillUpdateBadge" \
      /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/src/ \
      | grep -v "skill-update-badge.tsx"
    ```

    Expected: zero results.

- [ ] **Step 7.3 — Verify `upToDate` status renders nothing.**
      Manual spot-check: run the dev server, trigger a check, confirm skills with `upToDate` show no badge. (Or add a Playwright/Vitest test if the project has a test harness — check `crates/desktop/package.json` for a `test` script.)

- [ ] **Step 7.4 — Verify the "上次檢查" row appears below the toolbar.**
      Before any check: "尚未檢查". After manual refresh: "上次檢查 N 分鐘前" (or "剛才").

- [ ] **Step 7.5 — Final lint pass.**

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run lint 2>&1 | tail -20
    ```

    Expected: no errors.

- [ ] **Step 7.6 — Commit (if any lint-driven changes).**

    ```bash
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      add -p
    git -C /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge \
      commit -m "$(cat <<'EOF'
    fix(desktop): Phase 1 lint cleanup

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

## Dependencies & Sequencing

- Tasks 1 → 2 → 6 must run in order (Task 1 defines the new export; Task 2 updates the list; Task 6 cleans up detail).
- Task 3 must precede Task 5 (skills.tsx imports `checkSkillUpdatesQueryOptions`).
- Task 4 must precede Task 5 (skills.tsx uses the new locale keys).
- Tasks 3 and 4 are independent of each other and can run in parallel.
- Task 7 (smoke test) runs last.
- **No backend changes** are required for Phase 1 — `GET /skills/check-updates` is unchanged; Phase 3 adds version/date fields.
- Phase 2 (IA merge, view-by switch, store migration) must NOT begin until Phase 1 is merged and green.

## Open Assumptions

1. **HeroUI v3 `<Chip color="warning">` and `<Chip color="secondary">`** are valid. Check `.heroui-docs/react/components/(data-display)/chip.mdx` before running Task 1 Step 1.2. If `color="secondary"` doesn't render purple, use `className="bg-secondary/20 text-secondary"` instead.

2. **`checksUpdatedAt`** is a TanStack Query v5 property on the `UseQueryResult`. Confirmed in TQ v5 docs (`dataUpdatedAt`). The exact property name is `dataUpdatedAt` (not `checksUpdatedAt`) — use `const { data: cachedUpdateChecks, isFetching: isAutoChecking, dataUpdatedAt: checksUpdatedAt } = useQuery(...)`.

3. **`onResolveAuth` signature change** in Task 5.9 extends the prop to `(skillName: string) => void`. Task 2 must be revisited to use this signature — the `renderSkillItem` closure in `skill-list.tsx` wraps the call: `onResolveAuth ? () => onResolveAuth(skillGroup.name) : undefined`.

4. **`navigator.onLine`** is evaluated at render time (reactive only when the component re-renders). A fully reactive offline detector would use a `useNetworkStatus` hook. For Phase 1, evaluating at query construction time is sufficient — if the user is offline when they navigate to the skills page, auto-check is suppressed. A future phase can add a `window.addEventListener("online", ...)` listener.

5. **`formatRelativeTime`** is defined as a local helper inside the component. If the project already has a shared time-formatting utility in `lib/`, use that instead. Search with `grep -rn "formatRelative\|timeAgo\|relativeTime" crates/desktop/src/lib/`.

6. **Toast on auto-check**: Per §4.3 "toast 在 check 完成時回饋摘要", the spec implies a toast fires after every check. In TanStack Query v5, firing a side-effect from query data change without `useEffect` requires `QueryCache` subscriptions (infrastructure). For Phase 1, the toast fires on **manual** check only; auto-check is silent ("靜默自動跑"). If the team decides the auto-check must also toast, wire it via `queryClient.getQueryCache().subscribe(event => { if (event.type === 'updated' && ...) toast(...) })` in a `useEffect` in the root — but that requires a `useEffect`, which desktop AGENTS.md prohibits for data fetching. One compliant alternative is a `QueryObserver` outside React. Defer to Phase 2.

7. **`justNow` locale key**: if `diffMin < 1` is hit (< 60 seconds since last check), the fallback string `"just now"` / `"剛才"` is shown. Add `justNow` to all locales or hard-code as non-translatable for Phase 1.
