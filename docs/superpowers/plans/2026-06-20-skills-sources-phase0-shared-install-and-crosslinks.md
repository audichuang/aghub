# Phase 0 — Shared install component + skills.sh installed-state + cross-links Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Extract a shared install modal from the two modal-based installers (ManageSkillAgentsDialog + skills-sh InstallModal), add an "installed" badge to skills.sh search results by joining against the global lock, and add a cross-link from skill-detail's "Installed From" block to the future `/sources?source=<key>` route.

**Architecture:** Three self-contained changes sharing no Rust backend work. The shared install modal (`SharedSkillInstallModal`) unifies the agent-selection UX surface (scope/project + per-agent result rows) while each caller retains its own trigger logic and mutation variant; the installed-badge feature consumes the already-cached `globalSkillLockQueryOptions` query so it adds zero new network calls in the steady state; the cross-link is a single presentational Button added to the existing "Installed From" Card section in `skill-detail.tsx`.

**Tech Stack:** React 19 / HeroUI v3 / TanStack Query / TypeScript / bun / Tailwind v4 (no Rust, no ts-rs, no new API routes)

---

## ⚠️ Codex 審查修正（實作前必讀；覆寫下方對應步驟）

> GPT-5.5 對著真實程式碼審查後的必改項。判定：**fix-then-ship**。

- **[P1] 共用 modal 的 target selector（~:367）**：`canInstallToProject={false}` **不會**隱藏 `InstallTargetSelector`，它仍渲染 disabled checkbox（`install-target-selector.tsx:27`）。改：給 `SharedSkillInstallModal` 加 `showTargetSelector?: boolean`（預設 true），僅 true 時渲染 `<InstallTargetSelector>`；`ManageSkillAgentsDialog` 傳 `showTargetSelector={false}`。並刪掉「canInstallToProject=false 會隱藏」的假設。
- **[P1] 跨連結 key（~:914 與 Architecture「`/sources?source=<key>`」）**：Sources 頁 `?source=` 期望**複合 key** `scope:projectRoot:source`（`sources/index.tsx:169`），非 raw source。建 key：`` const key = `${group.items[0]?.source === "project" ? "project" : "global"}:${projectPath ?? ""}:${currentSkillSource.source}` `` 再 navigate `` `/sources?source=${encodeURIComponent(key)}` ``。
- **[P2] SkillInfoCard（~:538）**：`installAll` 時不要傳 `null` 把整張卡藏掉；加獨立 prop `skillInfoName={installAll ? undefined : selectedSkill.name}`，維持顯示 source、僅省 name。
- **[P2] 措辭（~:797 與 Architecture「zero new network calls」）**：與事實矛盾。改寫成「在 skills.sh 頁新增一個 global-lock 查詢；warm-up 後靠 cache 便宜」。

## File Structure

| File                                                              | Created / Modified | Responsibility                                                                                                                            |
| ----------------------------------------------------------------- | ------------------ | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/desktop/src/components/shared-skill-install-modal.tsx`    | **Created**        | Reusable Modal shell: SkillInfoCard + AgentSelector/SkillsAgentList slot + InstallTargetSelector + ResultStatusItem list + footer buttons |
| `crates/desktop/src/components/manage-skill-agents-dialog.tsx`    | **Modified**       | Swap internal Modal markup for `SharedSkillInstallModal`; keep all existing reconcile-mutation + state logic                              |
| `crates/desktop/src/pages/skills-sh/components/install-modal.tsx` | **Modified**       | Swap internal Modal markup for `SharedSkillInstallModal`; keep all existing install-mutation + state logic                                |
| `crates/desktop/src/pages/skills-sh/search.tsx`                   | **Modified**       | Add `globalSkillLockQueryOptions` query; compute `installedSet`; pass installed flag to each row; render `<Chip>` badge                   |
| `crates/desktop/src/components/skill-detail.tsx`                  | **Modified**       | Add "View source" Button in the "Installed From" block; navigates to `/sources?source=<key>`                                              |
| `crates/desktop/src/lib/locales/en.ts`                            | **Modified**       | Add `viewSkillSource` key                                                                                                                 |
| `crates/desktop/src/lib/locales/zh-Hant.ts`                       | **Modified**       | Add `viewSkillSource` key                                                                                                                 |
| `crates/desktop/src/lib/locales/zh-Hans.ts`                       | **Modified**       | Add `viewSkillSource` key                                                                                                                 |

---

### Task 1: Create `SharedSkillInstallModal`

**Files:**

- Create: `crates/desktop/src/components/shared-skill-install-modal.tsx`

This component is a pure presentational shell. It owns the `Modal.*` chrome and two
content slots: (a) a "configuration" slot rendered when `installResults` is empty, and
(b) a "results" slot rendered after install begins. Callers pass the fully-typed props
exactly matching the union of both current callers' needs.

- [ ] Step 1 — Read component API contracts from both callers.

    Confirmed from reading the files:
    - `InstallModal` (skills-sh) props: `isOpen, selectedSkill: MarketSkill | null, selectedAgents: Set<string>, onSelectedAgentsChange, installResults: InstallResult[], isInstalling, skillAgents, installAll, onInstallAllChange, installToProject, canInstallToProject, onInstallToProjectChange, selectedProjectId, onSelectedProjectIdChange, projects, onClose, onInstall`.
    - `ManageSkillAgentsDialog` (skills): uses `SkillsAgentList` (CheckboxGroup) rather than `AgentSelector` (TagGroup). Its agents/results are rendered inline in a `Modal.Body`.

    The genuinely shared surface: heading text, Modal chrome (Backdrop/Container/Dialog/CloseTrigger/Header/Body/Footer), `SkillInfoCard` (optional), a "select agents" body slot, `InstallTargetSelector`, `ResultStatusItem` list, Cancel/Confirm footer.

- [ ] Step 2 — Write `SharedSkillInstallModal`.

    Create `/home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/src/components/shared-skill-install-modal.tsx`:

    ```tsx
    import { Button, Modal } from "@heroui/react";
    import { useTranslation } from "react-i18next";
    import { InstallTargetSelector } from "./install-target-selector";
    import { ResultStatusItem } from "./result-status-item";
    import type { MarketSkill } from "../generated/dto";
    import type { InstallResult } from "../lib/install-utils";
    import type { Project } from "../lib/store";
    import { SkillInfoCard } from "./skill-info-card";

    export interface SharedSkillInstallModalProps {
    	isOpen: boolean;
    	onClose: () => void;
    	/** Modal heading; defaults to t("installSkill") */
    	heading?: string;
    	/** Optional skill summary card shown above the agent picker */
    	selectedSkill?: MarketSkill | null;
    	/** The "select agents" body — rendered when installResults is empty */
    	agentPickerSlot: React.ReactNode;
    	/** When truthy, the results phase replaces the picker */
    	installResults: InstallResult[];
    	isInstalling: boolean;
    	/** Project/scope target selector */
    	installToProject: boolean;
    	canInstallToProject: boolean;
    	onInstallToProjectChange: (v: boolean) => void;
    	selectedProjectId: string | null;
    	onSelectedProjectIdChange: (id: string | null) => void;
    	projects: Project[];
    	/** Confirm button label; defaults to t("install") */
    	confirmLabel?: string;
    	/** Confirm button disabled predicate (in addition to isInstalling) */
    	isConfirmDisabled?: boolean;
    	onConfirm: () => void;
    	/** Anything extra to render in the picker body (e.g. "install all" checkbox) */
    	extraPickerSlot?: React.ReactNode;
    }

    export function SharedSkillInstallModal({
    	isOpen,
    	onClose,
    	heading,
    	selectedSkill,
    	agentPickerSlot,
    	installResults,
    	isInstalling,
    	installToProject,
    	canInstallToProject,
    	onInstallToProjectChange,
    	selectedProjectId,
    	onSelectedProjectIdChange,
    	projects,
    	confirmLabel,
    	isConfirmDisabled = false,
    	onConfirm,
    	extraPickerSlot,
    }: SharedSkillInstallModalProps) {
    	const { t } = useTranslation();
    	const isResultsPhase = installResults.length > 0;

    	return (
    		<Modal.Backdrop isOpen={isOpen} onOpenChange={onClose}>
    			<Modal.Container>
    				<Modal.Dialog className="w-[calc(100vw-2rem)] max-w-md sm:max-w-lg">
    					<Modal.CloseTrigger />
    					<Modal.Header>
    						<Modal.Heading>
    							{heading ?? t("installSkill")}
    						</Modal.Heading>
    					</Modal.Header>

    					<Modal.Body className="p-4">
    						{!isResultsPhase && (
    							<div className="space-y-4">
    								{selectedSkill && (
    									<SkillInfoCard
    										name={selectedSkill.name}
    										source={selectedSkill.source}
    										className="mb-0"
    									/>
    								)}
    								<p className="text-sm text-muted">
    									{t("selectAgentsForSkill")}
    								</p>
    								{agentPickerSlot}
    								{extraPickerSlot}
    								<InstallTargetSelector
    									installToProject={installToProject}
    									onInstallToProjectChange={
    										onInstallToProjectChange
    									}
    									selectedProjectId={selectedProjectId}
    									onSelectedProjectIdChange={
    										onSelectedProjectIdChange
    									}
    									projects={projects}
    									canInstallToProject={
    										canInstallToProject
    									}
    								/>
    							</div>
    						)}

    						{isResultsPhase && (
    							<div className="space-y-3">
    								{installResults.map((result) => (
    									<ResultStatusItem
    										key={result.agentId}
    										displayName={result.displayName}
    										status={result.status}
    										statusText={
    											result.status === "pending"
    												? t("installing")
    												: result.status ===
    													  "success"
    													? t("installSuccess")
    													: ""
    										}
    										error={result.error}
    									/>
    								))}
    							</div>
    						)}
    					</Modal.Body>

    					<Modal.Footer>
    						{!isResultsPhase && (
    							<>
    								<Button
    									slot="close"
    									variant="secondary"
    									isDisabled={isInstalling}
    								>
    									{t("cancel")}
    								</Button>
    								<Button
    									onPress={onConfirm}
    									isDisabled={
    										isConfirmDisabled || isInstalling
    									}
    								>
    									{isInstalling
    										? t("installing")
    										: (confirmLabel ?? t("install"))}
    								</Button>
    							</>
    						)}
    						{isResultsPhase && (
    							<Button slot="close" variant="secondary">
    								{t("done")}
    							</Button>
    						)}
    					</Modal.Footer>
    				</Modal.Dialog>
    			</Modal.Container>
    		</Modal.Backdrop>
    	);
    }
    ```

- [ ] Step 3 — Type-check the new file.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit --project tsconfig.json 2>&1 | grep shared-skill-install-modal
    ```

    Expected: no output (no errors for this file).

- [ ] Step 4 — Commit scaffold.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge
    git add crates/desktop/src/components/shared-skill-install-modal.tsx
    git commit -m "$(cat <<'EOF'
    feat(desktop): add SharedSkillInstallModal presentational shell

    Empty-callers phase — component is created but not yet wired to any caller.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 2: Wire `ManageSkillAgentsDialog` to `SharedSkillInstallModal`

**Files:**

- Modify: `crates/desktop/src/components/manage-skill-agents-dialog.tsx` (currently 242 lines)

`ManageSkillAgentsDialog` uses `SkillsAgentList` (CheckboxGroup variant). The shared
modal's `agentPickerSlot` receives the `<SkillsAgentList .../>` JSX directly so the
CheckboxGroup variant is preserved.

Key differences from skills-sh:

- heading = `t("manageAgents")`
- no `selectedSkill` card
- confirm label = `t("applyChanges")` (not "install")
- `isConfirmDisabled` = `!hasChanges`
- no `extraPickerSlot` (no "install all" checkbox)
- `installResults` is always `[]` (ManageSkillAgentsDialog uses `agentStates` not `InstallResult[]` for per-row feedback — the row feedback is _inside_ `SkillsAgentList`, not in the results phase)

ASSUMPTION: `ManageSkillAgentsDialog` never enters the "results phase" (it shows inline
state in the CheckboxGroup rows via `agentStates`, not the results list). Therefore
`installResults` is always `[]` and `isResultsPhase` is always false. The Footer shows
Cancel + Apply.

- [ ] Step 1 — Read `manage-skill-agents-dialog.tsx` (already done; 242 lines confirmed).

- [ ] Step 2 — Rewrite the return value to use `SharedSkillInstallModal`.

    Replace the entire `return (...)` block (lines 186-242) in `manage-skill-agents-dialog.tsx`:

    Old block (lines 186-242):

    ```tsx
    return (
    	<Modal.Backdrop isOpen={isOpen} onOpenChange={onCloseAndReset}>
    		<Modal.Container>
    			<Modal.Dialog className="w-[calc(100vw-2rem)] max-w-md sm:max-w-lg">
    				<Modal.CloseTrigger />
    				<Modal.Header>
    					<Modal.Heading>{t("manageAgents")}</Modal.Heading>
    				</Modal.Header>

    				<Modal.Body className="p-4">
    					{!hasValidGroup ? (
    						<p className="text-sm text-muted">
    							{t("invalidConfiguration")}
    						</p>
    					) : (
    						<div
    							className={cn(
    								"transition-opacity",
    								isApplying && "opacity-50",
    							)}
    						>
    							<SkillsAgentList
    								agents={usableAgents}
    								selectedKeys={selectedAgents}
    								onSelectionChange={handleSelectionChange}
    								scope={scope}
    								agentStates={agentStates}
    								diffLabels={diffLabels}
    								disabled={isApplying}
    								disabledAgents={disabledAgents}
    								label={t("selectAgentsForSkill")}
    								emptyMessage={t("noTargetAgents")}
    							/>
    						</div>
    					)}
    				</Modal.Body>

    				<Modal.Footer>
    					<Button
    						slot="close"
    						variant="secondary"
    						isDisabled={isApplying}
    					>
    						{t("cancel")}
    					</Button>
    					<Button
    						onPress={handleApply}
    						isDisabled={!hasChanges || isApplying}
    					>
    						{isApplying ? t("applying") : t("applyChanges")}
    					</Button>
    				</Modal.Footer>
    			</Modal.Dialog>
    		</Modal.Container>
    	</Modal.Backdrop>
    );
    ```

    New block — replace import and return:

    Add at the top of the file (after existing imports):

    ```tsx
    import type { InstallResult } from "../lib/install-utils";
    import { SharedSkillInstallModal } from "./shared-skill-install-modal";
    ```

    Replace the return block with:

    ```tsx
    const agentPicker = !hasValidGroup ? (
    	<p className="text-sm text-muted">{t("invalidConfiguration")}</p>
    ) : (
    	<div className={cn("transition-opacity", isApplying && "opacity-50")}>
    		<SkillsAgentList
    			agents={usableAgents}
    			selectedKeys={selectedAgents}
    			onSelectionChange={handleSelectionChange}
    			scope={scope}
    			agentStates={agentStates}
    			diffLabels={diffLabels}
    			disabled={isApplying}
    			disabledAgents={disabledAgents}
    			label={t("selectAgentsForSkill")}
    			emptyMessage={t("noTargetAgents")}
    		/>
    	</div>
    );

    // ManageSkillAgentsDialog uses inline agentStates for per-row
    // feedback; it never enters the results phase of SharedSkillInstallModal.
    const NO_RESULTS: InstallResult[] = [];

    return (
    	<SharedSkillInstallModal
    		isOpen={isOpen}
    		onClose={onCloseAndReset}
    		heading={t("manageAgents")}
    		agentPickerSlot={agentPicker}
    		installResults={NO_RESULTS}
    		isInstalling={isApplying}
    		installToProject={scope === "project"}
    		canInstallToProject={false}
    		onInstallToProjectChange={() => {}}
    		selectedProjectId={null}
    		onSelectedProjectIdChange={() => {}}
    		projects={[]}
    		confirmLabel={isApplying ? t("applying") : t("applyChanges")}
    		isConfirmDisabled={!hasChanges}
    		onConfirm={handleApply}
    	/>
    );
    ```

    ASSUMPTION: `ManageSkillAgentsDialog` does not allow changing scope/project target
    (it derives `scope` from the skill's own location). The `InstallTargetSelector` inside
    `SharedSkillInstallModal` is rendered but `canInstallToProject=false` disables it,
    effectively hiding the project option. This matches the current behavior. If the shared
    modal's `InstallTargetSelector` renders visually noisy disabled UI, add an
    `showTargetSelector?: boolean` prop to `SharedSkillInstallModal` in a follow-up.

- [ ] Step 3 — Remove now-unused `Modal` import from `manage-skill-agents-dialog.tsx`.

    Check the import line at line 1:

    ```tsx
    import { Button, Modal, toast } from "@heroui/react";
    ```

    After the change `Modal` is no longer used directly. Remove it:

    ```tsx
    import { Button, toast } from "@heroui/react";
    ```

    Also remove unused `cn` if it is no longer referenced (check — `cn` was used in the old Modal.Body div). After removal it is still used for `isApplying && "opacity-50"` inside `agentPicker`. Keep `cn` import from `"../lib/utils"`.

- [ ] Step 4 — Type-check.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit --project tsconfig.json 2>&1 | grep -E "manage-skill-agents-dialog|shared-skill-install-modal"
    ```

    Expected: no output.

- [ ] Step 5 — Commit.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge
    git add crates/desktop/src/components/manage-skill-agents-dialog.tsx
    git commit -m "$(cat <<'EOF'
    refactor(desktop): wire ManageSkillAgentsDialog to SharedSkillInstallModal

    Replaces the inline Modal markup with the shared shell; all reconcile
    mutation logic and agentStates are unchanged.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 3: Wire `InstallModal` (skills-sh) to `SharedSkillInstallModal`

**Files:**

- Modify: `crates/desktop/src/pages/skills-sh/components/install-modal.tsx` (currently 175 lines)

`InstallModal` already uses `AgentSelector` (TagGroup), `InstallTargetSelector`, and
`ResultStatusItem` — these map perfectly to `SharedSkillInstallModal`'s slots.

The "install all skills" Checkbox is the caller-specific extra; it goes in
`extraPickerSlot`.

- [ ] Step 1 — Confirm that `installResults` in skills-sh is the same `InstallResult[]`
      type as in the shared modal. Check `lib/install-utils.ts`:

    ```bash
    grep -n "InstallResult" /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/src/lib/install-utils.ts | head -10
    ```

    Expected: `export type InstallResult = { agentId: string; displayName: string; status: "pending" | "success" | "error"; error?: string; }` (or similar). Confirm the shape matches `ResultStatusItem` props.

- [ ] Step 2 — Replace `install-modal.tsx` body.

    Replace entire file content:

    ```tsx
    import { Checkbox, Label } from "@heroui/react";
    import { useTranslation } from "react-i18next";
    import { AgentSelector } from "../../../components/agent-selector";
    import { SharedSkillInstallModal } from "../../../components/shared-skill-install-modal";
    import type { MarketSkill } from "../../../generated/dto";
    import type { InstallResult } from "../../../lib/install-utils";
    import type { Project } from "../../../lib/store";

    interface InstallModalProps {
    	isOpen: boolean;
    	selectedSkill: MarketSkill | null;
    	selectedAgents: Set<string>;
    	onSelectedAgentsChange: (agents: Set<string>) => void;
    	installResults: InstallResult[];
    	isInstalling: boolean;
    	skillAgents: ReturnType<
    		typeof import("../hooks/use-skill-install").useSkillInstall
    	>["skillAgents"];
    	installAll: boolean;
    	onInstallAllChange: (value: boolean) => void;
    	installToProject: boolean;
    	canInstallToProject: boolean;
    	onInstallToProjectChange: (value: boolean) => void;
    	selectedProjectId: string | null;
    	onSelectedProjectIdChange: (id: string | null) => void;
    	projects: Project[];
    	onClose: () => void;
    	onInstall: () => void;
    }

    export function InstallModal({
    	isOpen,
    	selectedSkill,
    	selectedAgents,
    	onSelectedAgentsChange,
    	installResults,
    	isInstalling,
    	skillAgents,
    	installAll,
    	onInstallAllChange,
    	installToProject,
    	canInstallToProject,
    	onInstallToProjectChange,
    	selectedProjectId,
    	onSelectedProjectIdChange,
    	projects,
    	onClose,
    	onInstall,
    }: InstallModalProps) {
    	const { t } = useTranslation();

    	const agentPicker = (
    		<AgentSelector
    			agents={skillAgents}
    			selectedKeys={selectedAgents}
    			onSelectionChange={onSelectedAgentsChange}
    			emptyMessage={t("noTargetAgents")}
    			showSelectedIcon
    			variant="secondary"
    		/>
    	);

    	const installAllCheckbox = (
    		<Checkbox
    			value="installAll"
    			isSelected={installAll}
    			onChange={(isSelected) => onInstallAllChange(isSelected)}
    			variant="secondary"
    		>
    			<Checkbox.Control>
    				<Checkbox.Indicator />
    			</Checkbox.Control>
    			<Checkbox.Content className="flex flex-col items-start gap-0.5">
    				<Label className="text-sm font-medium">
    					{t("installAllSkills")}
    				</Label>
    				<span className="text-xs text-muted">
    					{t("installAllSkillsDescription")}
    				</span>
    			</Checkbox.Content>
    		</Checkbox>
    	);

    	return (
    		<SharedSkillInstallModal
    			isOpen={isOpen}
    			onClose={onClose}
    			selectedSkill={
    				selectedSkill && !installAll ? selectedSkill : null
    			}
    			agentPickerSlot={agentPicker}
    			extraPickerSlot={installAllCheckbox}
    			installResults={installResults}
    			isInstalling={isInstalling}
    			installToProject={installToProject}
    			canInstallToProject={canInstallToProject}
    			onInstallToProjectChange={onInstallToProjectChange}
    			selectedProjectId={selectedProjectId}
    			onSelectedProjectIdChange={onSelectedProjectIdChange}
    			projects={projects}
    			isConfirmDisabled={
    				selectedAgents.size === 0 ||
    				(installToProject && !selectedProjectId)
    			}
    			onConfirm={onInstall}
    		/>
    	);
    }
    ```

    Note: `SkillInfoCard` display logic was: `name={installAll ? undefined : selectedSkill.name}`. The new version achieves the same — when `installAll` is true `selectedSkill` is passed as `null` so no card is shown; when false, the card shows with the skill name. The `source` is on `selectedSkill` which is passed through.

    ASSUMPTION: `selectedSkill` in `SharedSkillInstallModal` is typed `MarketSkill | null | undefined`. When `null`/`undefined` the `SkillInfoCard` is omitted. Update `SharedSkillInstallModal` to accept `selectedSkill?: MarketSkill | null`.

- [ ] Step 3 — Type-check.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit --project tsconfig.json 2>&1 | grep -E "install-modal|shared-skill-install-modal"
    ```

    Expected: no output.

- [ ] Step 4 — Smoke-test visually (optional but recommended).

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run dev
    ```

    Open app → Skills page → click "Add to Agent" on any skill → modal renders. Open Skills.sh → search a skill → click Install → modal renders with AgentSelector and "Install all" checkbox.

- [ ] Step 5 — Commit.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge
    git add crates/desktop/src/pages/skills-sh/components/install-modal.tsx
    git commit -m "$(cat <<'EOF'
    refactor(desktop): wire skills-sh InstallModal to SharedSkillInstallModal

    Replaces inline Modal markup with shared shell; AgentSelector, install-all
    checkbox, and per-agent ResultStatusItem list are preserved in their slots.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 4: Add "installed" badge to skills.sh search results

**Files:**

- Modify: `crates/desktop/src/pages/skills-sh/search.tsx` (currently 287 lines)

Per §12-C11: `search.tsx` currently loads NO lock/skill-list data (lines 16-21,
193-200). The global lock response (`GlobalSkillLockResponse.skills:
SkillLockEntryResponse[]`) has fields `name: string` and `source: string`. A
`MarketSkill` row has `name: string` and `source: string`. The installed set is the
cartesian join on `(source, name)`.

`globalSkillLockQueryOptions` already exists in `requests/skills.ts` and is already
imported by `skill-detail.tsx`. No new API route needed.

- [ ] Step 1 — Add `globalSkillLockQueryOptions` import and query in `search.tsx`.

    Current import block (lines 1-21):

    ```tsx
    import { MagnifyingGlassIcon } from "@heroicons/react/24/solid";
    import { Button, Spinner } from "@heroui/react";
    import { useInfiniteQuery } from "@tanstack/react-query";
    import { useQueryState } from "nuqs";
    import { useCallback, useMemo, useState } from "react";
    import { useTranslation } from "react-i18next";
    import type { TableComponents } from "react-virtuoso";
    import { TableVirtuoso } from "react-virtuoso";
    import { useLocation } from "wouter";
    import {
    	Empty,
    	EmptyHeader,
    	EmptyMedia,
    	EmptyTitle,
    } from "../../components/ui/empty";
    import type { MarketSkill } from "../../generated/dto";
    import { useApi } from "../../hooks/use-api";
    import { marketSearchInfiniteQueryOptions } from "../../requests/market";
    import { InstallModal } from "./components/install-modal";
    import { SkillsHeader } from "./components/skills-header";
    import { useSkillInstall } from "./hooks/use-skill-install";
    ```

    Add after `import { useApi } from "../../hooks/use-api";`:

    ```tsx
    import { useQuery } from "@tanstack/react-query";
    import { Chip } from "@heroui/react";
    import { globalSkillLockQueryOptions } from "../../requests/skills";
    ```

    Also add `useQuery` to the `@tanstack/react-query` import (note: `useInfiniteQuery`
    is already imported; add `useQuery` to the same import statement):

    ```tsx
    import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
    ```

    And add `Chip` to the `@heroui/react` import:

    ```tsx
    import { Button, Chip, Spinner } from "@heroui/react";
    ```

- [ ] Step 2 — Add the lock query and `installedSet` inside `SkillsSearchPage`.

    After `const api = useApi();` (line 51), add:

    ```tsx
    const { data: globalLock } = useQuery(globalSkillLockQueryOptions({ api }));

    const installedSet = useMemo(() => {
    	const set = new Set<string>();
    	for (const entry of globalLock?.skills ?? []) {
    		// Key: "<source>|<name>" matching MarketSkill fields
    		set.add(`${entry.source}|${entry.name}`);
    	}
    	return set;
    }, [globalLock]);
    ```

    `SkillLockEntryResponse.source` is the raw source string (e.g. `"github/AkaraChen/skills"`)
    and `MarketSkill.source` is the same string (confirmed from `generated/dto/MarketSkill.ts`
    and `SkillLockEntryResponse.ts`).

- [ ] Step 3 — Pass `isInstalled` to each row's td cell in the `itemContent` callback.

    Current `itemContent` (lines 173-200):

    ```tsx
    itemContent={(_index, skill) => (
    	<>
    		<td className="p-2 align-middle">
    			<span className="font-medium">
    				{skill.name}
    			</span>
    		</td>
    		<td className="p-2 align-middle">
    			<span className="text-muted">
    				{compactFormatter.format(
    					skill.installs,
    				)}
    			</span>
    		</td>
    		<td className="p-2 align-middle">
    			<span className="text-muted text-sm">
    				{skill.source}
    			</span>
    		</td>
    		<td className="p-2 align-middle">
    			<Button
    				size="sm"
    				variant="tertiary"
    				onPress={() =>
    					handleInstallClick(skill)
    				}
    			>
    				{t("install")}
    			</Button>
    		</td>
    	</>
    )}
    ```

    Replace with:

    ```tsx
    itemContent={(_index, skill) => {
    	const isInstalled = installedSet.has(
    		`${skill.source}|${skill.name}`,
    	);
    	return (
    		<>
    			<td className="p-2 align-middle">
    				<div className="flex items-center gap-2">
    					<span className="font-medium">
    						{skill.name}
    					</span>
    					{isInstalled && (
    						<Chip
    							size="sm"
    							color="success"
    							variant="soft"
    						>
    							{t("installed")}
    						</Chip>
    					)}
    				</div>
    			</td>
    			<td className="p-2 align-middle">
    				<span className="text-muted">
    					{compactFormatter.format(
    						skill.installs,
    					)}
    				</span>
    			</td>
    			<td className="p-2 align-middle">
    				<span className="text-muted text-sm">
    					{skill.source}
    				</span>
    			</td>
    			<td className="p-2 align-middle">
    				<Button
    					size="sm"
    					variant="tertiary"
    					onPress={() =>
    						handleInstallClick(skill)
    					}
    				>
    					{t("install")}
    				</Button>
    			</td>
    		</>
    	);
    }}
    ```

    `t("installed")` maps to the existing locale key at `en.ts:720`, `zh-Hant.ts:693`
    ("已安裝"), `zh-Hans.ts:695` ("已安装") — no new i18n key needed for this step.

    ASSUMPTION: `Chip` from `@heroui/react` v3 accepts `color="success"` and
    `variant="soft"`. Verified from `.heroui-docs/react/components/(data-display)/chip.mdx`
    before implementing. If the prop names differ, adjust accordingly (e.g. `variant="flat"`
    with `classNames` override).

- [ ] Step 4 — Type-check.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit --project tsconfig.json 2>&1 | grep search.tsx
    ```

    Expected: no output.

- [ ] Step 5 — Commit.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge
    git add crates/desktop/src/pages/skills-sh/search.tsx
    git commit -m "$(cat <<'EOF'
    feat(desktop): add installed badge to skills.sh search results

    Joins MarketSkill rows against the cached global lock on (source, name);
    renders a success Chip for already-installed skills. Zero new API calls.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 5: Cross-link from skill-detail "Installed From" to `/sources?source=<key>`

**Files:**

- Modify: `crates/desktop/src/components/skill-detail.tsx` (currently 760 lines)
- Modify: `crates/desktop/src/lib/locales/en.ts`
- Modify: `crates/desktop/src/lib/locales/zh-Hant.ts`
- Modify: `crates/desktop/src/lib/locales/zh-Hans.ts`

The "Installed From" block lives at lines 450-599 in `skill-detail.tsx`. The block
is conditionally rendered when `currentSkillSource` is truthy. Inside it, `currentSkillSource.source`
is the raw source key (e.g. `"github/AkaraChen/skills"`). The target route is
`/sources?source=<key>` (the existing Sources page already understands this nuqs param;
Phase 2 will eventually change it to `/skills?view=source&source=<key>`, but for now
pointing at `/sources` is correct and non-breaking).

`setLocation` from `useLocation` (wouter) is already imported and in scope (line 65).

- [ ] Step 1 — Add `viewSkillSource` i18n key to all three locale files.

    In `en.ts`, after the `installedFrom: "Installed From",` line (line 830), add:

    ```ts
    	viewSkillSource: "View source",
    ```

    In `zh-Hant.ts`, after the `installedFrom: "安裝來源",` line (line 797), add:

    ```ts
    	viewSkillSource: "查看來源",
    ```

    In `zh-Hans.ts`, after the `installedFrom: "安装来源",` line (line 803), add:

    ```ts
    	viewSkillSource: "查看来源",
    ```

- [ ] Step 2 — Add the cross-link button inside `skill-detail.tsx`.

    Locate the "Installed From" row (lines 450-599). The source row starts at line 455:

    ```tsx
    <div className="flex items-center justify-between gap-3 rounded-lg bg-surface-secondary px-3 py-2">
    ```

    The right-side action buttons (open-in-browser, sync) are in a `<div className="flex shrink-0 items-center gap-1">` at line 554. Add a third button before the sync button:

    Find:

    ```tsx
    {
    	sourceUrl && (
    		<div className="flex shrink-0 items-center gap-1">
    			<Tooltip delay={0}>
    				<Button
    					isIconOnly
    					variant="ghost"
    					size="sm"
    					className="size-8 text-muted"
    					aria-label={t("syncFromSource")}
    					onPress={() => setSyncDialogOpen(true)}
    				>
    					<ArrowPathIcon className="size-4" />
    				</Button>
    				<Tooltip.Content>{t("syncFromSource")}</Tooltip.Content>
    			</Tooltip>
    			<Tooltip delay={0}>
    				<Button
    					isIconOnly
    					variant="ghost"
    					size="sm"
    					className="size-8 text-muted"
    					aria-label={t("openInBrowser")}
    					onPress={() => openUrl(sourceUrl)}
    				>
    					<LinkIcon className="size-4" />
    				</Button>
    				<Tooltip.Content>{t("openInBrowser")}</Tooltip.Content>
    			</Tooltip>
    		</div>
    	);
    }
    ```

    Replace with (adds a "View source" button before the existing sync button):

    ```tsx
    {
    	sourceUrl && (
    		<div className="flex shrink-0 items-center gap-1">
    			{currentSkillSource?.source && (
    				<Tooltip delay={0}>
    					<Button
    						isIconOnly
    						variant="ghost"
    						size="sm"
    						className="size-8 text-muted"
    						aria-label={t("viewSkillSource")}
    						onPress={() =>
    							setLocation(
    								`/sources?source=${encodeURIComponent(
    									currentSkillSource.source,
    								)}`,
    							)
    						}
    					>
    						<GlobeAltIcon className="size-4" />
    					</Button>
    					<Tooltip.Content>
    						{t("viewSkillSource")}
    					</Tooltip.Content>
    				</Tooltip>
    			)}
    			<Tooltip delay={0}>
    				<Button
    					isIconOnly
    					variant="ghost"
    					size="sm"
    					className="size-8 text-muted"
    					aria-label={t("syncFromSource")}
    					onPress={() => setSyncDialogOpen(true)}
    				>
    					<ArrowPathIcon className="size-4" />
    				</Button>
    				<Tooltip.Content>{t("syncFromSource")}</Tooltip.Content>
    			</Tooltip>
    			<Tooltip delay={0}>
    				<Button
    					isIconOnly
    					variant="ghost"
    					size="sm"
    					className="size-8 text-muted"
    					aria-label={t("openInBrowser")}
    					onPress={() => openUrl(sourceUrl)}
    				>
    					<LinkIcon className="size-4" />
    				</Button>
    				<Tooltip.Content>{t("openInBrowser")}</Tooltip.Content>
    			</Tooltip>
    		</div>
    	);
    }
    ```

    `GlobeAltIcon` is already imported at line 7 (`import { ..., GlobeAltIcon, ... } from "@heroicons/react/24/solid";`). No new import needed.

    The guard `currentSkillSource?.source && ...` ensures the button only appears when
    a source key is known. It is nested inside the `{sourceUrl && ...}` block which already
    gates on lock data being present; the extra guard handles the edge case where `sourceUrl`
    was derived from a non-lock path (not possible with current logic, but defensive).

- [ ] Step 3 — Type-check.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit --project tsconfig.json 2>&1 | grep skill-detail
    ```

    Expected: no output.

- [ ] Step 4 — Verify i18n key is present and consistent across all three locale files.

    ```bash
    grep "viewSkillSource" \
      /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/src/lib/locales/en.ts \
      /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/src/lib/locales/zh-Hant.ts \
      /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/src/lib/locales/zh-Hans.ts
    ```

    Expected: three lines, one per file.

- [ ] Step 5 — Commit.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge
    git add \
      crates/desktop/src/components/skill-detail.tsx \
      crates/desktop/src/lib/locales/en.ts \
      crates/desktop/src/lib/locales/zh-Hant.ts \
      crates/desktop/src/lib/locales/zh-Hans.ts
    git commit -m "$(cat <<'EOF'
    feat(desktop): add cross-link from skill-detail installed-from to sources page

    Adds a globe icon button in the 'Installed From' row that navigates to
    /sources?source=<key> so users can jump from skill detail to the source view.

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 6: Full typecheck + lint gate

**Files:** none (validation only)

- [ ] Step 1 — Full TypeScript check.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run tsc --noEmit --project tsconfig.json 2>&1
    ```

    Expected: clean (zero errors).

- [ ] Step 2 — Prettier check on modified TS/TSX files.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop
    bun run prettier --check \
      src/components/shared-skill-install-modal.tsx \
      src/components/manage-skill-agents-dialog.tsx \
      src/pages/skills-sh/components/install-modal.tsx \
      src/pages/skills-sh/search.tsx \
      src/components/skill-detail.tsx \
      src/lib/locales/en.ts \
      src/lib/locales/zh-Hant.ts \
      src/lib/locales/zh-Hans.ts
    ```

    Expected: `All matched files use Prettier code style!`

    If any file fails, run `bun run prettier --write <file>` and re-stage + amend (or
    add a fixup commit). This project uses prettier for TS/TSX formatting.

- [ ] Step 3 — ESLint check (if configured).

    ```bash
    cd /home/audichuang/research/aghub/.claire/worktrees/skills-sources-merge/crates/desktop
    bun run eslint src/components/shared-skill-install-modal.tsx \
      src/components/manage-skill-agents-dialog.tsx \
      src/pages/skills-sh/components/install-modal.tsx \
      src/pages/skills-sh/search.tsx \
      src/components/skill-detail.tsx 2>&1
    ```

    If `eslint` script is not in `package.json`, skip. Check via:

    ```bash
    grep "eslint" /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge/crates/desktop/package.json | head -5
    ```

- [ ] Step 4 — Final commit.

    ```bash
    cd /home/audichuang/research/aghub/.claude/worktrees/skills-sources-merge
    git add -p   # review any unstaged formatting fixups
    git commit -m "$(cat <<'EOF'
    style(desktop): prettier fixups for Phase 0 files

    Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
    EOF
    )" 2>/dev/null || echo "nothing to commit"
    ```

---

## Dependencies & Sequencing

- **No backend changes required.** All three deliverables use existing API routes
  (`GET /api/v1/skills/lock/global` for the installed-badge, the existing reconcile
  and install mutations for the shared modal callers).
- **No ts-rs regeneration.** No Rust types are added or changed.
- **Task ordering:** Tasks 1→2→3 are sequential (Task 1 creates the component consumed
  by Tasks 2 and 3). Task 4 (search badge) and Task 5 (cross-link) are fully independent
  of each other and of Tasks 1-3; they can be parallelized with a subagent team.
- **Phase 1 prerequisite:** The shared modal established here is the foundation for the
  Phase 1 `SkillStatusBadge` work and the Phase 2 merged Skills Center. Phase 0 must
  land before Phase 1 begins.
- **Cross-link route:** The `/sources?source=<key>` target routes to the current
  `SourcesPage` (registered at `App.tsx:284`). Phase 2 will change this to
  `/skills?view=source&source=<key>` and add a redirect; this file will need a one-line
  change to `setLocation` at that point.

---

## Open Assumptions

1. **`InstallTargetSelector` in `ManageSkillAgentsDialog` context:** The shared modal
   always renders `InstallTargetSelector`. With `canInstallToProject=false`, the checkbox
   is disabled and the project dropdown never appears. If the resulting greyed-out section
   is visually unwanted, add an `showTargetSelector?: boolean` prop to
   `SharedSkillInstallModal` and default it to `true` for skills-sh, `false` for the
   manage-dialog. This is a 2-line change.

2. **`HeroUI v3 Chip` color/variant props:** This plan uses `color="success"` and
   `variant="soft"` for the installed badge. Verify in
   `.heroui-docs/react/components/(data-display)/chip.mdx` before implementing; adjust
   if the actual API differs (e.g. the project may use `variant="flat"` + className).

3. **Prettier formatting:** The plan's code blocks use 4-space indentation for
   readability. The repo's actual files use **hard tabs** (Rust) and prettier defaults
   (TSX). Run `bun run prettier --write` on all modified files to canonicalize before
   commit.

4. **`installSuccess` locale key:** Used in `SharedSkillInstallModal` for the success
   result row. Verify it exists:

    ```bash
    grep "installSuccess" crates/desktop/src/lib/locales/en.ts
    ```

    If absent, add `installSuccess: "Installed successfully"` to all three locale files.

5. **`encodeURIComponent` on source key:** The cross-link uses
   `encodeURIComponent(currentSkillSource.source)`. Sources like `"github/AkaraChen/skills"`
   contain `/` which must be encoded for the `?source=` query param. The existing
   `SourcesPage` reads this via nuqs `useQueryState("source")` which decodes it
   automatically — no backend change needed.

6. **`useQuery` import in `search.tsx`:** The file currently imports only
   `useInfiniteQuery`. Adding `useQuery` to the same import line is straightforward; if
   tree-shaking or bundler quirks arise, a separate import line also works.
