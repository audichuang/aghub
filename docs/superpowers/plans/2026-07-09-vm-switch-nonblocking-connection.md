# VM 切換非阻斷連線 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 切換 VM/Local 連線時保留 sidebar 框架，只在內容區顯示連線 pending/error/incompatible；不再全屏接管、不再整頁閃掉重掛。

**Architecture:** 把連線 gating 從 `ConnectionProvider`（取代整個 app）下沉到內容區的新元件 `ConnectionGate`。`ConnectionProvider` 永遠 render 並提供 `ConnectionContext`（僅「冷啟動」仍全屏）；`MainLayout` 在 App.tsx 共用一次，`ConnectionGate` 住在 `<main>` 內，ready 才提供 `ServerContext` + `AgentAvailabilityProvider` + pages。

**Tech Stack:** React 19、TypeScript（strict）、TanStack Query、wouter、HeroUI v3、Tailwind v4；測試用 node:test（`bun run test`，glob `src/**/*.test.ts`）。設計來源：`docs/specs/2026-07-09-vm-switch-nonblocking-connection.md`。

## Global Constraints

- 套件管理器一律 `bun`（never npm/yarn/pnpm）。
- 縮排 hard tab；className 串接用 `cn`（`lib/utils`），不用模板字串。
- 不新增 useEffect 做 data fetching / state sync（既有 timer/事件訂閱例外）。
- **不改動** `serverQuery` 的 queryFn、`deriveSupportsCredentialForwarding`、`setActive` 的快取清除語意——逐字保留（credential-forwarding race / tunnel 生命週期敏感）。
- 每個 task 結尾跑 `bun run --cwd crates/desktop typecheck`（Task 1 另跑 `bun run --cwd crates/desktop test`）。
- 所有指令從 repo 根 `/home/audichuang/research/aghub` 執行。

---

## File Structure

- `crates/desktop/src/lib/connection-logic.ts` — 新增純函式 `selectConnectionView`（Task 1）。
- `crates/desktop/src/lib/connection-logic.test.ts` — 既有；新增 `selectConnectionView` 測試（Task 1）。
- `crates/desktop/src/contexts/connection.tsx` — 擴充 `ConnectionContextValue`（Task 2）。
- `crates/desktop/src/providers/connection.tsx` — export 三個 Screen + 兩個 helper、Screen 高度改 `h-full`、provider 提供新 context 欄位（Task 2）、改 gating 為冷啟動判斷、移除 `ServerContext` 包裹（Task 4）。
- `crates/desktop/src/components/connection-gate.tsx` — 新元件（Task 3）。
- `crates/desktop/src/App.tsx` — 路由重排、共用 MainLayout、`AgentAvailabilityProvider` 移入 gate（Task 4）。

---

## Task 1: 純函式 `selectConnectionView` + 測試

**Files:**

- Modify: `crates/desktop/src/lib/connection-logic.ts`（在檔尾 `projectStatus` 之後新增）
- Test: `crates/desktop/src/lib/connection-logic.test.ts`

**Interfaces:**

- Consumes: 既有 `ConnectionStatus = "idle" | "connecting" | "connected" | "error"`。
- Produces: `selectConnectionView(status: ConnectionStatus, isIncompatible: boolean): ConnectionView`，其中 `export type ConnectionView = "pending" | "error" | "incompatible" | "ready"`。

- [ ] **Step 1: 寫失敗測試**

在 `crates/desktop/src/lib/connection-logic.test.ts` 檔尾（既有 import `node:test`/`node:assert` 沿用；若沒有則加 `import { test } from "node:test"; import assert from "node:assert/strict";` 與 `import { selectConnectionView } from "./connection-logic";`）新增：

```ts
test("selectConnectionView: connected -> ready", () => {
	assert.equal(selectConnectionView("connected", false), "ready");
});

test("selectConnectionView: connecting/idle -> pending", () => {
	assert.equal(selectConnectionView("connecting", false), "pending");
	assert.equal(selectConnectionView("idle", false), "pending");
});

test("selectConnectionView: error -> error", () => {
	assert.equal(selectConnectionView("error", false), "error");
});

test("selectConnectionView: incompatible wins over generic error", () => {
	assert.equal(selectConnectionView("error", true), "incompatible");
});

test("selectConnectionView: isIncompatible ignored unless status is error", () => {
	assert.equal(selectConnectionView("connected", true), "ready");
	assert.equal(selectConnectionView("connecting", true), "pending");
});
```

- [ ] **Step 2: 跑測試確認 FAIL**

Run: `bun run --cwd crates/desktop test`
Expected: FAIL（`selectConnectionView` 未定義 / import 失敗）。

- [ ] **Step 3: 實作**

在 `crates/desktop/src/lib/connection-logic.ts` 檔尾新增：

```ts
/** 連線畫面 gate 要 render 的四種視圖。 */
export type ConnectionView = "pending" | "error" | "incompatible" | "ready";

/**
 * 把連線狀態投影成內容區 gate 要顯示的視圖。
 * - connected → ready
 * - error + incompatible → incompatible（優先於一般 error）
 * - error → error
 * - 其餘（connecting / idle）→ pending
 */
export function selectConnectionView(
	status: ConnectionStatus,
	isIncompatible: boolean,
): ConnectionView {
	if (status === "connected") return "ready";
	if (status === "error") return isIncompatible ? "incompatible" : "error";
	return "pending";
}
```

- [ ] **Step 4: 跑測試確認 PASS**

Run: `bun run --cwd crates/desktop test`
Expected: PASS（含既有 `projectStatus` 測試）。

- [ ] **Step 5: typecheck + commit**

```bash
bun run --cwd crates/desktop typecheck
git add crates/desktop/src/lib/connection-logic.ts crates/desktop/src/lib/connection-logic.test.ts
git commit -m "feat(desktop): 加 selectConnectionView 純函式投影連線視圖"
```

---

## Task 2: 擴充 context + export 連線畫面/helper + Screen 高度改 h-full

此 task 只做「不改變現有行為」的準備：擴充 context 型別與提供值、把三個 Screen 與兩個 helper export、把 Screen 高度從 `h-screen` 改 `h-full`（並在 provider 冷啟動 return 補 `h-screen` wrapper，使全屏語境高度不變）。做完 app 行為與現在一致，typecheck 過。

**Files:**

- Modify: `crates/desktop/src/contexts/connection.tsx`
- Modify: `crates/desktop/src/providers/connection.tsx`

**Interfaces:**

- Produces（context 新欄位，Task 3 消費）：
    - `connectError: unknown`
    - `retryConnect: () => void`
    - `isRetryingConnect: boolean`
    - `applyConnectResult: (result: ConnectResult) => void`
- Produces（export，Task 3 import）：`ConnectionPendingScreen`、`ConnectionErrorScreen`、`IncompatibleConnectionScreen`、`asRemotePayload`、`remoteErrorMessage`。

- [ ] **Step 1: 擴充 `ConnectionContextValue`**

在 `crates/desktop/src/contexts/connection.tsx` 的 `ConnectionContextValue` interface（`disconnect` 之後、interface 結尾前）新增欄位；`ConnectResult` 已於檔頭 `export type { ConnectResult }`：

```ts
	/** 最近一次連線 bring-up 的原始錯誤（供 gate 判斷 incompatible / 取訊息）。null 表示無錯。 */
	connectError: unknown;
	/** 重試當前連線（serverQuery.refetch）。 */
	retryConnect: () => void;
	/** 當前連線是否正在重試 / 抓取中（serverQuery.isFetching）。 */
	isRetryingConnect: boolean;
	/** incompatible redeploy 成功後，把新的 ConnectResult 寫回 server 快取。 */
	applyConnectResult: (result: ConnectResult) => void;
```

- [ ] **Step 2: 在 `ConnectionProvider` 的 `connectionValue` 補上新欄位**

在 `crates/desktop/src/providers/connection.tsx` 組 `connectionValue` 的物件（`disconnect,` 之後）新增：

```ts
			connectError: serverQuery.isError ? serverQuery.error : null,
			retryConnect: () => {
				void serverQuery.refetch();
			},
			isRetryingConnect: serverQuery.isFetching,
			applyConnectResult: (result) =>
				queryClient.setQueryData<ConnectResult>(
					["server", activeId],
					result,
				),
```

（`ConnectResult` 型別已在此檔可用；`queryClient`/`activeId`/`serverQuery` 均為既有區域變數。）

- [ ] **Step 3: export 三個 Screen 與兩個 helper**

在 `crates/desktop/src/providers/connection.tsx`：

- 把 `function ConnectionPendingScreen(` 改為 `export function ConnectionPendingScreen(`
- 把 `function ConnectionErrorScreen(` 改為 `export function ConnectionErrorScreen(`
- 把 `function IncompatibleConnectionScreen(` 改為 `export function IncompatibleConnectionScreen(`
- 找到 `asRemotePayload` 與 `remoteErrorMessage` 的宣告（`function`/`const`），各加 `export`。

- [ ] **Step 4: 三個 Screen root 的 `h-screen` 改 `h-full`**

在同檔，把 `ConnectionPendingScreen`、`ConnectionErrorScreen`、`IncompatibleConnectionScreen` 三個元件「最外層 root `<div>`」的 className 內 `h-screen` 改為 `h-full`（只改這三個 Screen 的 root；每個各一處）。

- [ ] **Step 5: 冷啟動全屏 return 補 `h-screen` wrapper（暫時，Task 4 會改條件）**

此 task 尚未改 gating 條件，只把現有三個「全屏 return」的 Screen 各自用 `h-screen` wrapper 包起來，讓改成 `h-full` 後全屏語境高度不變。找到 `providers/connection.tsx` 現有的三個 return：

- incompatible：`return ( <IncompatibleConnectionScreen ... /> )`
- error：`return ( <ConnectionErrorScreen ... /> )`
- pending：`return ( <ConnectionPendingScreen ... /> )`

各自改成外面包一層：

```tsx
return (
	<div className="flex h-screen flex-col">
		<IncompatibleConnectionScreen ... />
	</div>
);
```

（三個 return 同樣手法，內層元件與其 props 保持不變。）

- [ ] **Step 6: typecheck + commit**

```bash
bun run --cwd crates/desktop typecheck
git add crates/desktop/src/contexts/connection.tsx crates/desktop/src/providers/connection.tsx
git commit -m "refactor(desktop): export 連線畫面/helper、擴充 context、Screen 改 h-full"
```

Expected: typecheck 通過；app 行為與改動前一致（仍全屏）。

---

## Task 3: 新增 `ConnectionGate` 元件

**Files:**

- Create: `crates/desktop/src/components/connection-gate.tsx`

**Interfaces:**

- Consumes: Task 2 的 context 新欄位、Task 1 的 `selectConnectionView`、Task 2 export 的三個 Screen + `asRemotePayload`/`remoteErrorMessage`；`useConnection`（`hooks/use-connection`）、`ServerContext`（`contexts/server`）、`AgentAvailabilityProvider`（`providers/agent-availability`）、`LOCAL_CONNECTION`（`lib/connection-logic`）。
- Produces: `export function ConnectionGate({ children }: { children: ReactNode })`。

- [ ] **Step 1: 建立元件**

建立 `crates/desktop/src/components/connection-gate.tsx`：

```tsx
import type { ReactNode } from "react";
import { ServerContext } from "../contexts/server";
import { useConnection } from "../hooks/use-connection";
import {
	LOCAL_CONNECTION,
	selectConnectionView,
} from "../lib/connection-logic";
import {
	asRemotePayload,
	ConnectionErrorScreen,
	ConnectionPendingScreen,
	IncompatibleConnectionScreen,
	remoteErrorMessage,
} from "../providers/connection";
import { AgentAvailabilityProvider } from "../providers/agent-availability";

/**
 * 內容區連線 gate：依當前連線狀態，render pending / error / incompatible /
 * ready。ready 時才提供 ServerContext + AgentAvailabilityProvider 給 children
 * （pages），所以 baseUrl 為 null 時 pages 根本不掛載、不會 throw。
 *
 * 冷啟動（從未成功連過）由 ConnectionProvider 自己全屏處理，不會 render 到這裡。
 */
export function ConnectionGate({ children }: { children: ReactNode }) {
	const {
		status,
		port,
		baseUrl,
		activeConnection,
		setActive,
		connectError,
		retryConnect,
		isRetryingConnect,
		applyConnectResult,
	} = useConnection();

	const payload = asRemotePayload(connectError);
	const isIncompatible = payload?.kind === "incompatible";
	const view = selectConnectionView(status, isIncompatible);

	if (view === "ready" && port !== null && baseUrl !== null) {
		return (
			<ServerContext value={{ port, baseUrl }}>
				<AgentAvailabilityProvider>
					{children}
				</AgentAvailabilityProvider>
			</ServerContext>
		);
	}

	if (view === "incompatible") {
		return (
			<IncompatibleConnectionScreen
				connection={activeConnection}
				remoteVersion={payload?.remoteVersion ?? null}
				onRedeployed={applyConnectResult}
			/>
		);
	}

	if (view === "error") {
		return (
			<ConnectionErrorScreen
				connection={activeConnection}
				message={remoteErrorMessage(connectError)}
				isRetrying={isRetryingConnect}
				onRetry={retryConnect}
				onUseLocal={() => setActive(LOCAL_CONNECTION.id)}
			/>
		);
	}

	return (
		<ConnectionPendingScreen
			connection={activeConnection}
			onUseLocal={() => setActive(LOCAL_CONNECTION.id)}
		/>
	);
}
```

**注意**：`ConnectionPendingScreen` 的 props 需與其定義一致（連線畫面已存在，Task 2 只加 export）。若其 prop 名與上例不同（例如缺 `key`），以現有定義為準調整——用 `codegraph_explore "ConnectionPendingScreen IncompatibleConnectionScreenProps"` 或讀 `providers/connection.tsx` 對照。`payload?.remoteVersion` 欄位名以 `asRemotePayload` 回傳型別為準。

- [ ] **Step 2: typecheck**

Run: `bun run --cwd crates/desktop typecheck`
Expected: 通過（元件尚未被使用，但型別要對）。

- [ ] **Step 3: commit**

```bash
git add crates/desktop/src/components/connection-gate.tsx
git commit -m "feat(desktop): 新增 ConnectionGate 內容區連線 gate 元件"
```

---

## Task 4: 改 ConnectionProvider gating + App.tsx 重排（接通）

這步讓一切接通，必須原子（中間狀態會壞）。做法：ConnectionProvider 改成「只有冷啟動才全屏」、且不再包 `ServerContext`；App.tsx 共用一次 `MainLayout`，內容區用 `ConnectionGate` 包 `Switch`，`AgentAvailabilityProvider` 移入 gate。

**Files:**

- Modify: `crates/desktop/src/providers/connection.tsx`
- Modify: `crates/desktop/src/App.tsx`

**Interfaces:**

- Consumes: Task 3 的 `ConnectionGate`。

- [ ] **Step 1: ConnectionProvider — 加 `hasEverConnected` 與冷啟動判斷**

在 `crates/desktop/src/providers/connection.tsx`：

1. 從 react 匯入補 `useRef`（若尚未匯入）。
2. 在 `const baseUrl = ...` 之後加：

```ts
// 一旦成功連過一次就記住：冷啟動（從未連過）才全屏；之後的切換交給內容區 gate。
const hasEverConnected = useRef(false);
if (baseUrl !== null) {
	hasEverConnected.current = true;
}
const isColdStart = !hasEverConnected.current;
```

3. 把現有三個「全屏 return」（Task 2 已各包 `h-screen` wrapper）的**條件**改為只在冷啟動時 return：
    - `if (serverQuery.isError) { ... }` 區塊整段改成 `if (isColdStart && serverQuery.isError) { ... }`
    - `if (baseUrl === null || port === null) { ... }` 區塊改成 `if (isColdStart && (baseUrl === null || port === null)) { ... }`
      （wrapper 與 Screen 內容不變，只改外層 `if` 條件。）

- [ ] **Step 2: ConnectionProvider — 不再包 `ServerContext`**

把最後的 return：

```tsx
return (
	<ConnectionContext value={connectionValue}>
		<ServerContext value={{ port, baseUrl }}>{children}</ServerContext>
	</ConnectionContext>
);
```

改為（移除 ServerContext，改由 ConnectionGate 在 ready 分支提供；移除此檔對 `ServerContext` 的 import）：

```tsx
return (
	<ConnectionContext value={connectionValue}>{children}</ConnectionContext>
);
```

- [ ] **Step 3: App.tsx — 重排路由**

在 `crates/desktop/src/App.tsx`：

1. import 補 `import { ConnectionGate } from "./components/connection-gate";`。
2. 移除最外層 `AgentAvailabilityProvider` 的 import 與包裹（改由 gate 內部使用；`App.tsx` 不再直接 import 它）。
3. 把 `return (...)` 的 provider/router 區塊改為：`ConnectionProvider > NuqsAdapter > Router`，Router 內先 `OnboardingController`，再共用一次 `MainLayout` 包 `ConnectionGate` 包 `Switch`；各 `Route` 去掉自己的 `MainLayout`，只留 `ErrorBoundary`/`Suspense`/page。redirect route（`"/"`、`"/sources"`、fallback）維持原樣放在 `Switch` 內。

改後的 `return`：

```tsx
return (
	<QueryClientProvider client={queryClient}>
		<Toast.Provider placement="bottom end" />
		<ThemeProvider>
			<ConnectionProvider>
				<NuqsAdapter>
					<Router>
						<OnboardingController />
						<MainLayout>
							<ConnectionGate>
								<Switch>
									<Route path="/">
										<DefaultSidebarRoute />
									</Route>
									<Route path="/skills">
										<ErrorBoundary>
											<Suspense
												fallback={
													<SkillsPageSkeleton />
												}
											>
												<SkillsPage />
											</Suspense>
										</ErrorBoundary>
									</Route>
									<Route path="/mcp">
										<ErrorBoundary>
											<Suspense
												fallback={
													<SkillsPageSkeleton />
												}
											>
												<MCPServersPage />
											</Suspense>
										</ErrorBoundary>
									</Route>
									<Route path="/inference-providers">
										<ErrorBoundary>
											<InferenceProvidersPage />
										</ErrorBoundary>
									</Route>
									<Route path="/skills-sh/search">
										<ErrorBoundary>
											<Suspense
												fallback={
													<SkillsPageSkeleton />
												}
											>
												<SkillsSearchPage />
											</Suspense>
										</ErrorBoundary>
									</Route>
									<Route path="/skills-sh">
										<ErrorBoundary>
											<Suspense
												fallback={
													<SkillsPageSkeleton />
												}
											>
												<SkillsShPage />
											</Suspense>
										</ErrorBoundary>
									</Route>
									<Route path="/cc-plugins">
										<ErrorBoundary>
											<Suspense
												fallback={
													<SkillsPageSkeleton />
												}
											>
												<PluginsPage />
											</Suspense>
										</ErrorBoundary>
									</Route>
									<Route path="/settings">
										<SettingsPage />
									</Route>
									<Route path="/settings/custom-agents">
										<CustomAgentsPage />
									</Route>
									<Route path="/sub-agents">
										<ErrorBoundary>
											<Suspense
												fallback={
													<SkillsPageSkeleton />
												}
											>
												<SubAgentsPage />
											</Suspense>
										</ErrorBoundary>
									</Route>
									<Route path="/projects/:id">
										<ProjectDetailPage />
									</Route>
									<Route path="/sources">
										<SourcesRedirect />
									</Route>
									<Route>
										<DefaultSidebarRoute />
									</Route>
								</Switch>
							</ConnectionGate>
						</MainLayout>
						<DeepLinkImportModal
							intent={currentIntent}
							onComplete={processNextIntent}
						/>
					</Router>
				</NuqsAdapter>
			</ConnectionProvider>
		</ThemeProvider>
	</QueryClientProvider>
);
```

- [ ] **Step 4: 驗證 OnboardingController 不依賴 baseUrl**

Run: `grep -n "useApi\|useServer\|useAgentAvailability" crates/desktop/src/components/onboarding-controller.tsx`
Expected: 無輸出。若有輸出（間接依賴 agent availability），把 `OnboardingController` 移到 `ConnectionGate` 內（ready 分支），或改為容忍 null——並在 commit message 註明。

- [ ] **Step 5: typecheck + build**

```bash
bun run --cwd crates/desktop typecheck
bun run --cwd crates/desktop build
```

Expected: 皆通過。

- [ ] **Step 6: commit**

```bash
git add crates/desktop/src/providers/connection.tsx crates/desktop/src/App.tsx
git commit -m "feat(desktop): 連線 gating 下沉內容區，切換 VM 保留 sidebar 框架"
```

---

## Task 5: 全量驗證

**Files:** 無（驗證）。

- [ ] **Step 1: typecheck + test + lint + build**

```bash
bun run --cwd crates/desktop typecheck
bun run --cwd crates/desktop test
cd crates/desktop && bunx prettier --check "src/**/*.{ts,tsx}" && bunx eslint src --max-warnings=0; cd /home/audichuang/research/aghub
bun run --cwd crates/desktop build
```

Expected: 全綠。prettier 若報格式，`bunx prettier --write` 對應檔後重跑。

- [ ] **Step 2: 手動煙測清單（release 前，由使用者執行）**

- Local → Remote 切換：sidebar/titlebar/切換器不消失，內容區顯示 pending 畫面（含耗時秒數），連上後 pages 出現。
- Remote → Remote、Remote → Local：同上。
- 連線失敗：內容區顯示 error（重試 / 用 Local 可按），sidebar 仍在。
- 冷啟動（首次開 app 或直接連一個 remote）：仍為全屏 pending/error。
- 連線中再點另一個 VM：直接改切新目標，sidebar 不卡。

- [ ] **Step 3: commit（若有格式修正）**

```bash
git add -A && git commit -m "chore(desktop): VM 切換非阻斷連線格式/lint 修正"
```

---

## Self-Review（已完成）

- **Spec coverage**：pending/error/incompatible 下沉（Task 3/4）、冷啟動全屏（Task 4 Step 1）、連線中再切（既有 setActive 語意不動，Task 4 保留）、內容區沿用現有 Screen（Task 2 改 h-full + Task 3 render）、測試（Task 1）、context 擴充（Task 2）、App 重排（Task 4）——皆有對應。
- **Placeholder scan**：無 TBD/TODO；export/高度改動以「明確指令 + 對照現有定義」給出（機械改動，非 placeholder）。
- **Type consistency**：`ConnectionView`、`selectConnectionView`、context 新欄位（`connectError`/`retryConnect`/`isRetryingConnect`/`applyConnectResult`）在 Task 1/2 定義、Task 3 消費，名稱一致。
