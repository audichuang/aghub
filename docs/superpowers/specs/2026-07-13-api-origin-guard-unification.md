# API origin guard 全面統一（所有非 OPTIONS route）— 設計 + 實作 plan

**日期**: 2026-07-13
**狀態**: approved（範圍=全掛；已過一輪 codex review + 實測，本版納入修正）
**Crate**: `aghub-api`
**類型**: 安全深度防禦（DNS-rebinding 硬化）

## 背景與問題（含實測證據）

desktop 把 `aghub-api` 嵌在 `127.0.0.1`（隨機 port），**無 token auth**（共享 token 與本 fork 的 SSH-remote / 多連線模型衝突，上游 `ApiAuth` 刻意不 port）。兩層 transport-agnostic 防護：

- **Layer 1 — CORS allow-list**（`lib.rs` `build_rocket`，掛 `rocket_cors` fairing）：擋 foreign `Origin`。
- **Layer 2 — `TrustedLocalOrigin` request guard**（`extractors.rs`）：擋 foreign `Host`。header 缺席時 lenient。

**實測發現（起真實 server + curl，與 codex 獨立實測一致）**：

| 請求                                             | 結果        | 結論                                            |
| ------------------------------------------------ | ----------- | ----------------------------------------------- |
| 未 guard route + foreign `Origin`                | 403         | **CORS fairing 改寫成 `/cors/403`**，非 Layer 2 |
| 已 guard route + foreign `Host`（無 Origin）     | 403         | Layer 2 guard 擋住                              |
| **未 guard route + foreign `Host`（無 Origin）** | **400/422** | **請求穿透到 handler = 真漏洞**                 |
| 未 guard GET + foreign `Host`                    | 200         | **讀到本地敏感資料**                            |

**關鍵**：`rocket_cors` 對**無 `Origin`** 的請求直接放行（DNS-rebinding：攻擊頁面 same-origin 不送 Origin、Host 是 rebound 域名）。此向量 **只有 Layer 2 能擋**。盤點 **107 個** mounted route（GET/POST/PUT/DELETE，不含 OPTIONS preflight），**只有 14 個掛了 Layer 2，93 個沒有**。未受保護的：

- **mutation（60）**：寫 keyring（inference api*key）、改 agent config（mcp/skill/sub-agent）、開本地編輯器、裝/移除 plugin、`clear*\*\_state` 清 agent config。
- **GET（33）**：讀 MCP `env`/`headers`、skill/sub-agent 內容、plugin config 與 plugin MCP secrets；部分 inference GET 還會 `open_db`（`create_dir_all` + SQLite migration，實為 state-changing）。

## 決策（已與使用者確認）

**範圍 = 全掛**：所有 `/api/v1` mounted route（GET/POST/PUT/DELETE）都掛 `_origin: TrustedLocalOrigin`；**唯一例外是 OPTIONS preflight**（`routes/mod.rs` `preflight`，由 CORS fairing 處理）。理由：guard 對正常流程零影響（trusted origin / 缺 header 放行），全掛規則最簡單、threat-model 最完整、枚舉測試最直接、最不易遺漏。

**機制 = per-handler guard 參數 + 枚舉防漏測試**（Rocket 慣用型別化 guard；Rocket 無內建全域 route guard，fairing 短路需 request-rewrite 技巧、風險高，不採）。

## 目標 / 非目標

**目標**

1. 所有非 OPTIONS route 受 Layer 2 保護（擋 foreign Host / DNS-rebinding）。
2. 資料驅動枚舉測試當守門員：任何新 route（任何 method）漏加 guard 即測試紅。
3. `crates/api/AGENTS.md` guard 政策與現實一致。

**非目標**

- 不動 OPTIONS preflight、不動 CORS Layer 1、不引入 token auth。
- 不改任何 route 業務邏輯 / 回傳型別（只加一個 guard 參數）。
- 不改既有的 DB-init-on-GET 副作用本身（僅用 guard 擋未授權觸發）。

## 設計

### guard 參數

對 93 個尚無 guard 的 handler，各加：

```rust
_origin: TrustedLocalOrigin,
```

放參數列**最前**。**訂正的理由**（原 plan 寫錯）：Rocket 0.5.1 codegen 固定求值順序為「所有 request guards → path params → query guards → data guard」，與參數宣告順序**無關**；`_origin`（`FromRequest`）本來就一定先於 data guard。放最前的真正作用是**讓它先於其他 `FromRequest` guard fail-fast**（例如 foreign Host 直接 403，而非先被別的 request guard 攔）。實測：guard 命中時回 **403**；通過 guard 後缺 body 通常回 **400**（非先前誤寫的 422）。

各檔補 `use crate::extractors::TrustedLocalOrigin;`（多數已 import）。

### 守門員測試 `all_routes_reject_foreign_host`

放 `crates/api/src/lib.rs` 的 crate-internal `#[cfg(test)]` module（`build_rocket` 是 `pub(crate)`，外部 `tests/*.rs` 用不到）。資料驅動、自我維護：

```
all_routes_reject_foreign_host:
  用 test_env_lock 隔離 HOME / XDG_CONFIG_HOME / XDG_STATE_HOME / app_data / PATH → tempdir
  rocket = build_rocket(test config, tempdir app_data)
  client = Client::tracked(rocket)
  for route in client.rocket().routes():
      if route.method == Options: continue          # preflight 例外
      req = client.req(route.method, fill_uri(route.uri))
                  .header(Header::new("Host", "evil.example"))   # foreign HOST，不送 Origin
                  .header(ContentType::JSON)                      # 避開 format=json route 的 404
      assert_eq!(req.dispatch().status(), Status::Forbidden)     # 403
```

**要點 / 陷阱（codex 實測確認）**：

- **用 foreign `Host`、不送 `Origin`**：foreign Origin 會被 CORS 改寫成 403（假象），測不出 guard 有無 → 假綠。Host 向量只有 Layer 2 擋，能真正驗證覆蓋。
- **一律加 `ContentType::JSON`**：`format = "json"` 的 route（`open_with_editor`、`skills open`、`skills edit`）缺 `Content-Type` 不 match → Rocket 回 **404** → false-red。
- **必須隔離環境（三重，codex review 指出）**：若某 route 漏 guard，本測試會**真的執行** handler（無 body 的 mutation 如 `clear_claude_state`/`clear_codex_state` 改 agent config、delete/sync provider 動 keyring、`update_marketplace` 可能觸發網路）。**env/tempdir 隔離不了 native OS keyring（macOS Keychain / Linux Secret Service）**，故需三重隔離：
    1. `test_env_lock` + tempdir 導向 `HOME` / `XDG_CONFIG_HOME` / `XDG_STATE_HOME` / `PATH`（agent config 檔、CLI）。
    2. tempdir `app_data`（inference 的 SQLite DB）。
    3. **native keyring 用 mock backend**：`keyring::set_default_credential_builder(keyring::mock::default_credential_builder())`（keyring 3.6 內建 `mock` module，process-global，配合 env lock 序列化；零 production 改動）。crates/api 需加 `keyring` v3 dev-dependency。inference 另有現成 `MemoryCredentialStore`/`FileCredentialStore` 可注入作 fallback。

    補完 guard 後（目標態）全部 403，handler 從不執行、keyring 從不被碰；三重隔離是防「開發中途 / 有 route 漏 guard」時不汙染真實環境的保險。呼應本 repo real-home 汙染教訓。

- dynamic segment（`<id>`/`<agent>`/`<name>`）填非空佔位即可命中 router；query 全是 dynamic/wild，可省略。**若未來出現 static query field，`fill_uri` 必須保留它**否則 route 不 match。
- 涵蓋**所有 method**（GET 也斷言 403 on foreign Host），故未來新增 PATCH 等也自動納入 —— 不寫死 method 白名單。
- 覆蓋證明（全綠=93 都補上）+ 回歸守門（未來漏加即紅）。**實作時先確認此測試在未改前為紅**（證明它真的會抓）。

### 正向 sanity（防過嚴誤擋 webview）

- 既有 `trusted_local_origin_guard_blocks_foreign_origin_and_host`、CORS 測試維持通過。
- 新增：對代表性 route 同時帶 `Origin: tauri://localhost` **與** `Host: 127.0.0.1:<port>`（webview 真實會同時帶兩者）→ 斷言**非** 403。

### 既有 unit test call-site 維護

多個 `#[cfg(test)]` 直接呼叫 handler 函式（非經 HTTP），加參數後會編譯失敗，需傳入 `TrustedLocalOrigin` unit struct：至少 `mcps.rs`（create/update/delete_mcp，:301 起多處）、`sub_agents.rs`（:284 起）。**這是必要測試維護，不是業務邏輯改動**。實作時 `cargo test -p aghub-api` 編譯錯誤會逐一指出。

## 實作清單（93 個 handler，逐檔）

> **權威做法**：對 `crates/api/src/routes/*.rs` 內**每一個** `#[get/post/put/delete(...)]` handler（**除** `routes/mod.rs` 的 `#[options]` preflight），若尚無 `_origin: TrustedLocalOrigin` 就加上（放參數最前）。以枚舉測試綠燈為完成依據，勿只靠計數。

各檔待補數（= 該檔 route attr 數 − 已 guard）：

| 檔               | route attr | 已 guard | **待補** |
| ---------------- | ---------- | -------- | -------- |
| inference.rs     | 26         | 1        | **25**   |
| plugins.rs       | 21         | 0        | **21**   |
| skills.rs        | 25         | 4        | **21**   |
| mcps.rs          | 10         | 0        | **10**   |
| sub_agents.rs    | 8          | 0        | **8**    |
| integrations.rs  | 3          | 0        | **3**    |
| agents.rs        | 2          | 0        | **2**    |
| coverage.rs      | 1          | 0        | **1**    |
| market.rs        | 1          | 0        | **1**    |
| sources.rs       | 2          | 1        | **1**    |
| credentials.rs   | 5          | 5        | 0        |
| skills_update.rs | 3          | 3        | 0        |
| **總計**         | **107**    | **14**   | **93**   |

## 驗收標準

1. `cargo build -p aghub-api` 通過。
2. `cargo clippy -p aghub-api --tests` 零警告（未用參數命名 `_origin`）。
3. `all_routes_reject_foreign_host` 通過（證明 93 全補、0 漏）；**改前先驗證它為紅**。
4. 正向 sanity（trusted Origin + trusted Host 不被 403）通過。
5. 既有 api 測試全綠（`cargo test -p aghub-api`，含更新後的 handler call-site）。
6. `cargo fmt` 乾淨（hard tabs / 80-col）。
7. `crates/api/AGENTS.md` Layer-2 段落更新：政策=「所有 `/api/v1` route（除 OPTIONS preflight）掛 Layer 2，由 `all_routes_reject_foreign_host` 強制；Layer 1 擋 foreign Origin、Layer 2 擋 foreign Host / DNS-rebinding」，移除 inference Gap 段。

## 風險與緩解

| 風險                                                     | 緩解                                                                                                                                                      |
| -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 枚舉用 foreign Origin → 假綠                             | 用 foreign Host、不送 Origin（實測確認 CORS 會用 Origin 假象污染判定）                                                                                    |
| guard 缺失時測試執行副作用 mutation 汙染真實環境/keyring | 三重隔離：`test_env_lock`+tempdir(HOME/XDG/PATH)、tempdir app_data(SQLite)、`keyring::mock` credential builder（native keyring 隔離；env/tempdir 擋不住） |
| `format=json` route 缺 Content-Type → 404 false-red      | 枚舉請求一律加 `ContentType::JSON`                                                                                                                        |
| 誤擋 desktop webview                                     | guard 對 trusted origin/host、缺 header 放行；正向 sanity 雙 header 驗證                                                                                  |
| 既有直接呼叫 handler 的 unit test 編譯失敗               | 實作清單含 call-site 更新，傳 `TrustedLocalOrigin`                                                                                                        |
| 未來新增 PATCH/其他 method 漏保護                        | 枚舉測試涵蓋所有非 OPTIONS method，不寫死白名單                                                                                                           |
| static query field 出現使 fill_uri 不 match              | `fill_uri` 保留 static query；審查時注意                                                                                                                  |

## 執行者（grok）注意

- 只加 guard 參數 + import + 測試 + 更新既有 call-site + 文件；**不改任何業務邏輯**。
- 每個非 OPTIONS handler 參數列最前加 `_origin: TrustedLocalOrigin,`。
- 逐檔掃 `#[get/post/put/delete]`，不要漏；preflight `#[options]` 不加。
- 先讓 `all_routes_reject_foreign_host` 在未改狀態為紅，再逐檔補到全綠。
- 遵守 repo 風格：hard tabs（width 4）、80-col、`clippy -D warnings`。
- 完成跑：`cargo test -p aghub-api` + `cargo clippy -p aghub-api --tests` + `cargo fmt`。
