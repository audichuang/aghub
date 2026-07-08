# VM 切換非阻斷連線（保留框架、gating 下沉內容區）

**日期**: 2026-07-09
**狀態**: 設計核准（走方案 A）
**範圍**: `crates/desktop` 前端 connection 層

## 問題

切換 VM（或 Local↔Remote）時，整個 app 會「卡一次」。根因不是 JS freeze，而是
`ConnectionProvider`（`crates/desktop/src/providers/connection.tsx`）在 `baseUrl === null`
時直接 `return <ConnectionPendingScreen />`——**全屏接管**，連 sidebar / titlebar /
連線切換器都一起卸載。切換流程：

1. `setActive(id)` → `removeQueries(DATA_NAMESPACES)` + `removeQueries(["server", id])` + `setActiveId(id)`
2. `serverQuery`（key `["server", activeId]`）對 remote 跑 `invoke("connect_remote")`——SSH + 建 tunnel + `TUNNEL_SETTLE`，數秒
3. 這期間 `serverQuery.data` 為 undefined → `port/baseUrl === null` → 全屏 pending 取代整個 children，連上後整棵重新掛載

體感就是「切一下整頁閃掉重掛」。

## 目標 / 非目標

**目標**

- 切換已連過的連線時，**sidebar / titlebar / 連線切換器保留不卸載**；只有右側內容區顯示連線狀態。
- 連線中 / 失敗 / 版本不符三種狀態都下沉到內容區（不再全屏）。
- 連線中允許使用者在 sidebar 直接改切另一個 VM（或 Local）。

**非目標**

- 不做背景預連線（連好才切 activeId）——體驗雖更好但需管背景連線/取消，本次不做。
- 不保留切換前的舊資料（不採 `keepPreviousData`：會讓切換中打到舊 VM 的 tunnel、閃舊資料，比卡頓更糟）。
- 不改 Rust 端連線/tunnel/憑證轉發邏輯。
- 冷啟動（app 首次啟動、從未成功連過）**維持全屏** pending/error——此時 sidebar 也還沒有可顯示的意義。

## UX 決策（已與使用者確認）

1. 內容區沿用**現有的 `ConnectionPendingScreen`**（已有連線階段、耗時秒數、VM 名、「用 Local」），縮到內容區顯示，不再全屏。
2. 下沉範圍 = `connecting` + `error`（一般）+ `error`（incompatible）三種；冷啟動維持全屏。
3. 連線中，sidebar 再點另一個 VM → 直接改切新目標（放棄當前 in-flight 連線，其結果被忽略）。切換器對正在連的那個顯示既有 spinner 狀態。

## 架構（方案 A）

核心限制：**sidebar 的連線切換器需要 `ConnectionContext`（必須在 Provider 內）**，而
**pages + `AgentAvailabilityProvider` 需要非 null 的 `baseUrl`（必須在 gate 之後）**。
因此框架夾在中間：Provider 永遠 render → 共用 `MainLayout`（框架）→ 內容區 `ConnectionGate` →
ready 才提供 `ServerContext` + `AgentAvailabilityProvider` + pages。

已驗證（grep）：`AppSidebar` / `ConnectionSwitcher` / `ProjectList` / `useProjects` /
`OnboardingController` 都**不依賴** `useApi`/`useServer`/`useAgentAvailability`，所以框架層可安全待在 gate 之前。

### 前（現況）

```
ConnectionProvider            (baseUrl null → 全屏 return，取代整個 app)
  AgentAvailabilityProvider   (useApi → useServer；靠上面全屏 return 才不會在 null 時 throw)
    Router
      OnboardingController
      Switch
        Route "/skills"  → MainLayout(sidebar + main) > page   (每個 route 各自包 MainLayout)
        Route "/mcp"     → MainLayout(sidebar + main) > page
        ...
```

### 後（方案 A）

```
ConnectionProvider            (永遠 render；提供 ConnectionContext；只有「冷啟動」才全屏 return)
  Router
    OnboardingController       (不依賴 baseUrl，保持)
    MainLayout(sidebar + main) (共用一次，sidebar 掛一次、切換連線不卸載)
      main:
        ConnectionGate         (讀 useConnection() 的 status)
          connecting            → <ConnectionPendingScreen/>
          error (incompatible)  → <IncompatibleConnectionScreen/>
          error (其他)           → <ConnectionErrorScreen/>
          connected             → <ServerContext> + <AgentAvailabilityProvider>
                                     <Switch><Route …>{page}</Route></Switch>
    DeepLinkImportModal
```

### 元件邊界

- **`ConnectionGate`（新，`components/connection-gate.tsx`）**
    - 輸入：透過 `useConnection()` 取 `status` + 新增暴露的 error 欄位。
    - 職責：把 4-state（+incompatible 細分）投影成要 render 的畫面；ready 時包 `ServerContext` +
      `AgentAvailabilityProvider` 並 render `children`（= Switch/pages）。
    - 分支判斷抽成純函式 `selectConnectionView(status, isIncompatible)`（見下「測試」）。
    - 不知道任何 page 細節；`children` 由 `MainLayout` 傳入。

- **`ConnectionProvider`（改造）**
    - 移除 `baseUrl === null` 的全屏 `return`（`connection.tsx:622`）與 error/incompatible 的全屏 `return`（`593–620`）。
    - 改為：維護 `hasEverConnected`（`useRef`，`baseUrl` 首次非 null 時設 `true`）。
        - **冷啟動**（`!hasEverConnected` 且 `status` 為 `connecting`/`error`）→ 仍 `return` 全屏對應畫面（保留現行冷啟動體驗）。
        - 否則永遠 render `<ConnectionContext>{children}</ConnectionContext>`（**不再**在此包 `ServerContext`）。
    - 擴充 `ConnectionContextValue`（`contexts/connection.tsx`）新增：
        - `connectError: unknown`（原始 `serverQuery.error`，供 gate 用 `asRemotePayload`/`remoteErrorMessage`）
        - `isConnecting: boolean` / 保留既有 `status`
        - `retryConnect: () => void`（`serverQuery.refetch`）
        - `applyConnectResult: (r: ConnectResult) => void`（incompatible redeploy 後 `setQueryData(["server", activeId], r)`）
    - `port`/`baseUrl` 保留在 context（既有消費者相容）；但 `ServerContext` 改由 `ConnectionGate` 的 ready 分支提供。

- **`MainLayout`（改造）**
    - 由「每個 route 各自包」改為 **App.tsx 中共用一次**，包住 `ConnectionGate`；`ConnectionGate` 再包 `Switch`。
    - `<main>` 內容 = `<ConnectionGate>{routesSwitch}</ConnectionGate>`。
    - sidebar/titlebar 結構不變。

- **`App.tsx`（重排）**
    - `AgentAvailabilityProvider` 從最外層移到 `ConnectionGate` 的 ready 分支內。
    - `MainLayout` 提為共用；各 `Route` 去掉自己的 `MainLayout` 包裹，只保留 `ErrorBoundary`/`Suspense`/page。
    - redirect-only route（`"/"`、`"/sources"`、fallback）維持 redirect 行為；置於 gate 的 Switch 內（連線中不 render、連上後才 redirect，可接受）。
    - `OnboardingController` 位置：保持在 `Router` 內、`MainLayout` 外（grep 確認不依賴 baseUrl）。

## 資料流

```
使用者點 VM
  → setActive(id)  (清 per-host 資料快取 + 該 host 的 server 快取；setActiveId)
  → serverQuery key 變 ["server", id]，重新 invoke connect_remote
  → status: connected → connecting
  → ConnectionGate 重算：ready 分支卸載，顯示 <ConnectionPendingScreen/>
     （sidebar / titlebar 不受影響，仍掛著）
  → connect_remote 完成 → serverQuery.data 有值 → status connected
  → ConnectionGate ready 分支重新掛載 <ServerContext>+<AgentAvailabilityProvider>+pages
     （用新 baseUrl 乾淨抓資料）
```

連線中再切：使用者點另一 VM → `setActive` 再次執行 → `activeId` 改 → 舊的 in-flight
`connect_remote` 結果因 key 已變而被 React Query 忽略；gate 持續顯示 pending 直到新目標就緒。

## 錯誤處理

- `error`（incompatible）→ `IncompatibleConnectionScreen`（自帶 redeploy mutation + 確認 dialog；redeploy 成功呼叫 `applyConnectResult` 寫回 cache）。
- `error`（其他）→ `ConnectionErrorScreen`（重試 = `retryConnect`；「用 Local」= `setActive(LOCAL)`）。
- 這兩個畫面**內容區**顯示（非冷啟動時）；冷啟動時仍全屏。
- 既有的 `remote-disconnected` 事件與 focus pull-fallback（`connection.tsx:540/572`）行為不變。

## 測試

- 既有 `connection-logic.test.ts` 的 `projectStatus` 保留。
- 新增純函式 `selectConnectionView(status: ConnectionStatus, isIncompatible: boolean): "pending" | "error" | "incompatible" | "ready"`（放 `lib/connection-logic.ts`），把 gate 的分支決策抽出來，加 colocated `*.test.ts`（node:test）覆蓋四個分支 + incompatible 優先於一般 error。
- 元件層依專案慣例不加框架測試（`src/**/*.test.ts` 只測純邏輯）。
- 手動驗證（release 前）：Local→Remote、Remote→Remote、Remote→Local 切換時 sidebar 不消失；連線失敗顯示內容區 error；冷啟動仍全屏。

## 風險 / 回滾

- **風險**：`ConnectionProvider` 對 credential-forwarding race、tunnel 生命週期敏感，且此路徑無元件測試。改動集中在「畫面 gating 的位置」，不動 `serverQuery` 的 queryFn / `deriveSupportsCredentialForwarding` / `setActive` 的快取清除語意——這些保持逐字不變，降低回歸面。
- **需在實作時驗證的前提**：`OnboardingController` 若實際依賴 agent availability（間接），移出 `AgentAvailabilityProvider` 後需調整其位置或改為容忍。
- **回滾**：改動可整體 revert（App.tsx 路由重排 + connection.tsx + 新 gate 檔），無資料/格式遷移。

## 交付物

- `components/connection-gate.tsx`（新）
- `lib/connection-logic.ts` + `.test.ts`（新增 `selectConnectionView`）
- `providers/connection.tsx`（移除全屏 return，改冷啟動判斷；不再提供 ServerContext）
- `contexts/connection.tsx`（擴充 `ConnectionContextValue`）
- `layouts/main-layout.tsx` + `App.tsx`（共用 MainLayout、gate、provider 位置重排）
