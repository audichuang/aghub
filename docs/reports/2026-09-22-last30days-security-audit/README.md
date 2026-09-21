# last30days 更新拒絕調查：技能風險、掃描誤判與封鎖政策

調查日期：2026-09-22（台北時間）。範圍：Ubuntu 上的更新拒絕事件、六個 Critical 的來源、aghub 判定機制與政策建議。本次沒有修改掃描規則、更新已安裝技能，或使用 `--force-unsafe`。

## 1. 先回答：到底是技能有問題嗎？

**現有證據不足以把 `last30days` 判定為惡意技能。本次可重現的六個 Critical，不是六個已確認的攻擊，而是兩種文字模式在六個檔案中的命中。**

- 五個命中來自同一條 Grok 安裝指令的文件、docstring 與提示字串。它確實採用「下載即執行」的高權限安裝方式，但該指令也出現在 Grok 官方網站。這值得提示使用者審閱來源，不能單憑它推論作者惡意或程式已偷偷執行。[Grok 官方安裝頁](https://x.ai/build)
- 一個命中來自 Google/Gemini API 驗證程式。規則看見讀取金鑰的程式碼與 `urllib` 這個字，就判為憑證外洩；沒有確認該金鑰被送到不相關第三方。原始碼所示目的地是 Google 的 Gemini API，用途是替搜尋結果評分。**就這個被標為「憑證外洩」的行為而言，判定過度。** [評測程式][up-eval]
- 因此，**這次拒絕主要暴露的是 aghub 把啟發式訊號升格為確定性封鎖的問題**。這不構成整包技能的安全保證：本次是針對六個阻擋原因的調查，沒有執行所有功能，也沒有稽核所有執行期依賴。

「含有需要信任的操作」「偵測到可疑模式」「已確認惡意行為」是不同結論。目前實作把它們混在 `Critical → Malicious → Block` 的單一路徑上。

## 2. 證據來源與版本界線

| 項目                   | 本次確認結果                                                                                 |
| ---------------------- | -------------------------------------------------------------------------------------------- |
| 出問題的主機／技能     | SSH 別名 `ubuntu`；`last30days`                                                              |
| 遠端 API 版本          | `/home/audichuang/.cargo/bin/aghub-api --version` 回覆 `2.26.4`                              |
| 遠端事件               | 日誌有 87 筆該技能 finding：Critical 6、High 15、Low 23、Info 43                             |
| 已安裝來源             | `mvanhorn/last30days-skill`，`main`，`skills/last30days/SKILL.md`                            |
| 已安裝 lock 記錄       | `refCommit = 56ba5ace27e4697aedc60aa0b1e1bfdcd592ff20`；這不是失敗更新的新 commit            |
| 本次重新取得的上游版本 | `349ca444b4fda466e74d471dffa2aff36bb997f1`，GitHub 回報 commit 時間 `2026-09-19T04:22:31Z`   |
| aghub 分析基準         | `707781bb689cf86096dda122764505d0a73c8c8a`；`v2.26.4` 與此基準的 `skill-audit/rules/` 無差異 |
| 重現方式               | Python 綁定 YARA-X `1.17.0`，編譯該 aghub 基準全部 17 份內建 YARA 來源；未執行技能程式       |

上游技能目錄共取得並核對 **135 個 Git blob**：每一個檔案都以 Git blob SHA-1 對照固定 commit 的 tree，另外保存 SHA-256。GitHub tarball 少了五個媒體檔，因此額外由 Git blob API 補齊後才做完整 L1 掃描，沒有把缺檔的掃描稱為完整重現。

**事件版本仍有一個不能越過的界線：原始拒絕日誌沒有記錄 fetched commit 或內容 digest。** 所以無法證明當時被拒絕的所有 bytes 與本次 snapshot 完全相同。但證據相當吻合：固定版本重現了六個相同的 Critical「規則＋檔案」組合；完整 L1 的 84 個「檔案＋規則＋severity」組合，也全部出現在遠端日誌且無額外差異。遠端多出的三筆是本次 L1-only probe 沒執行的兩個 L2 hidden-comment 訊號及一個合成 dataflow-chain 訊號。

Python wheel 與 aghub 的 Rust git-rev 依賴具有相同公開版本號，但未證明二進位逐位元相同。本次證明的是相同規則在同系列 YARA-X 引擎的可重現行為，**不是重新呼叫遠端完整更新交易**。

上一則簡答只核對了已安裝檔案；本報告補上固定上游版本及實際 pattern matches。五個安裝說明檔案在已安裝版與上游版間有差異；`scripts/evaluate_search_quality.py` 則 SHA-256 完全相同。

## 3. 六個 Critical 分別命中了什麼

以下行號全部屬於固定上游 commit `349ca444…`。

| 檔案／行號                                           | 實際命中                                                         | 所在上下文                                                | 可以成立的判斷                               |
| ---------------------------------------------------- | ---------------------------------------------------------------- | --------------------------------------------------------- | -------------------------------------------- |
| [SKILL.md:722][up-skill]                             | `curl -fsSL https://x.ai/cli/install.sh \| bash`                 | Grok CLI 安裝說明                                         | 文件推薦下載即執行的安裝方式                 |
| [prescriptions.py:98][up-prescriptions]              | 同上                                                             | `fix_nl` 自然語言修復建議；相鄰 `fix_cli` 是 npm 安裝命令 | 是會展示的修復建議，不是這一行正在執行 shell |
| [grok_x.py:8][up-grok]                               | 同上                                                             | 模組開頭 docstring                                        | 是模組說明中的指令                           |
| [health.py:197–198][up-health]                       | 同上，兩次                                                       | `_FALLBACK_PRESCRIPTIONS` 的安裝／重裝提示                | 一個檔案內兩次文字匹配，輸出一筆 finding     |
| [backends.py:303][up-backends]                       | 同上                                                             | 缺少 Grok 時回傳的 `prescription` 字串                    | 是缺少依賴時的提示                           |
| [evaluate_search_quality.py:16–17、197–201][up-eval] | `$net` 命中 `urllib`；`$read_env` 命中三個 `os.environ.get(...)` | 網路模組 import 與 API key 選取函式                       | 讀取金鑰＋包含網路模組名稱；不等於已證明外洩 |

前五筆是**同一種安裝方式被重複描述**，不是五個獨立攻擊。規則每個「檔案＋rule」產生一筆 finding，因此也不能把 finding 數當成執行次數。

### Grok 安裝指令：真實風險，過度定性

官方網站目前確實提供完全相同的 `curl -fsSL https://x.ai/cli/install.sh | bash` 指令。[Grok 官方安裝頁](https://x.ai/build)

這不表示下載腳本絕對安全：它要求使用者信任當下由遠端提供的內容，安裝腳本及後續下載也可能改變。**「來自官方」只能確認用途與來源合理，不能免除供應鏈風險。**

但此次五筆 matching context 都是說明文字、docstring 或提示字串，不能從中證明 skill 在更新時自動執行了該指令。尤其 `prescriptions.py` 明確把自然語言 `fix_nl` 和實際建議命令 `fix_cli` 分開；YARA 不讀這些程式語意。

也不能反過來制定「Markdown 一律安全」：技能的 Markdown 本來就會引導 agent 行為，安裝建議可能稍後被執行。正確結論應是「有下載即執行的安裝建議，需要審閱」，而不是直接宣告整包惡意。

### Gemini 評測程式：正常驗證被標成外洩

原始碼可追出這條用途鏈：[完整評測程式][up-eval]

1. `resolve_google_judge_api_key()` 依序取得 Google／Gemini key。
2. `build_judge_prompt()` 組合主題及搜尋結果的標題、來源、網址與日期。
3. `call_gemini_judge()` 將 key 放入 Google API 請求，將上述 prompt 當作 request body。
4. 目的地常數是 `https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}`。
5. 使用回應中的評分比較不同版本的搜尋品質。

Google 文件也說明 Gemini API 請求需要 API key 驗證。[Google Gemini API key 文件](https://ai.google.dev/gemini-api/docs/api-key)

這裡有資料傳輸，也有 key 的使用，**但把服務自己的 key 交給該服務做驗證，不等於把秘密偷送給不相關接收者**。本次閱讀的函式裡沒有看到把 `.ssh`、整份 `.env` 或其他憑證檔當成 request body 上傳的行為。此結論限於該命中所指的程式流程，不擴張到所有依賴與 runtime 行為。

此外，這是比較兩個版本的評測腳本，會把選取的憑證傳入受測的子程序環境；受測版本因而需要被信任。這是真實的能力與信任邊界，但不是上述 YARA finding 證明的「憑證檔外洩」。

## 4. 我怎麼判定：不是靠檔名猜測，而是做對照實驗

以 aghub 實際內建規則掃描以下八段最小文字。所有文字只當輸入 bytes；沒有執行 shell、Python 範例或任何網路請求。完整輸入、結果與 assert 均保存在 probe 與 JSON。

| 測試輸入                                                           | Critical 結果                                | 證明的侷限                                                              |
| ------------------------------------------------------------------ | -------------------------------------------- | ----------------------------------------------------------------------- |
| 官方安裝指令的普通文件                                             | download-pipe                                | 文件與實際執行不區分                                                    |
| `Never run` 加上同一條安裝指令                                     | download-pipe                                | 否定語意不區分，警告也會被擋                                            |
| 只有 Python docstring 的安裝說明                                   | download-pipe                                | 註解／字串也被當成操作訊號                                              |
| 讀 `GOOGLE_API_KEY` ＋未使用的 `urllib` import ＋ `print("ready")` | credential-exfil                             | **沒有發送網路請求也能觸發外洩判定**                                    |
| 讀 Google key、呼叫 Google API                                     | credential-exfil                             | 正常服務驗證也被擋                                                      |
| 讀同一 key、傳到 `attacker.invalid`                                | credential-exfil                             | 惡意正向控制也被擋，但與正常用途沒有區分                                |
| 上一個外送案例，只把 `os.environ` 經 `env` 別名讀取                | 無 Critical；仍有低／資訊級 source/sink 命中 | 同樣的外送意圖，改寫拼法即可避開阻擋；完整 engine 的共現鏈最多使其 Warn |
| 真正呼叫 subprocess 執行下載管線的文字                             | download-pipe                                | 規則保有發現真正危險形狀的用途                                          |

**八個預期結果與六個真實檔案命中組合均已跑過且 assertion 通過。** 這證明的是本次規則的區分能力缺口，不是以八個案例估計整個偵測器的誤報率；報告沒有足夠母體可以聲稱「誤報率 X%」。

最有力的反例是第四列：

```python
import os
from urllib.request import Request
key = os.environ.get("GOOGLE_API_KEY")
print("ready")
```

這段沒有傳輸動作，卻命中 `aghub_credential_file_exfil`。因此「該規則命中」在邏輯上不足以推出「發生憑證外洩」。它甚至不足以推出「程式會建立網路請求」。

## 5. aghub 為什麼會得出這個結果

> 🧠 **From Hindsight memory (技能安全稽核閘門（skill-audit）)** — 原設計要求 Critical 保持窄，只有這一級會拒絕 fetched 安裝與更新；High／Suspicious 只警告。這是設計意圖，不是已實證的低誤報保證。本次以現行規則及反例核對這個前提。

### 規則只做文字匹配，不做真正資料流分析

`aghub_download_pipe_execute` 的 `$pipe` 只要求同一行出現 `curl/wget/fetch … | sh/bash/zsh`。它沒有 Markdown／Python 語法資訊，也不理解「不要執行」。[規則程式碼](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/skill-audit/rules/aghub/real_world.yara#L67)

`aghub_credential_file_exfil` 的核心條件是「讀取形狀＋直接憑證來源＋網路字串」在同檔共現。`$net` 包含裸字串 `urllib`，所以 import 就能滿足；它沒有驗證哪一份資料流入哪一個請求，也沒有判斷目的地是不是服務本身。[規則程式碼](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/skill-audit/rules/aghub/real_world.yara#L39)

跨檔的 `aghub_dataflow_chain` 也只是找一個 source finding 和一個 sink finding 配對；原碼明說沒有 taint tracking。這一層只產生 High／Warn，並不補足「已確認外洩」的證據。[合成鏈實作](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/skill-audit/src/engine/mod.rs#L79)

### 風險等級直接決定惡意標籤與操作結果

任意一筆 Critical 就使整體成為 `Malicious`，再映射為 `Block`。六筆不是必要條件，一筆已足夠。偵測、危害程度、惡意判斷與使用者政策被串成單一開關。[verdict](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/skill-audit/src/verdict.rs#L23)、[policy](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/skill-audit/src/policy.rs#L21)

### 日誌看似證據，其實主要是規則描述

YARA finding 的 `evidence` 直接取 `meta.description`；不是 matched bytes，也不是執行觀測。於是文字「reads a credential/.env/.ssh file and sends it over the network」會在只有 `os.environ.get` 加 import 時照樣印出。[finding 轉換](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/skill-audit/src/engine/yara.rs#L38)

引擎雖會計算最早 pattern 的行號，core 日誌只輸出技能、規則、severity、檔名、描述，沒有輸出該行號或 pattern。對本例而言，最早位置甚至是 `urllib` import；單一「最早行號」也不能描述整條讀取／傳輸關係。[core 日誌](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/core/src/skills/audit.rs#L101)

### High confidence 並不代表已校準的可信度

目前 `Benign` 一律 High；其他 verdict 只要至少兩個 findings 也 High。這不是對真實惡意樣本做過校準的機率，也沒有處理同一條說明複製到多個檔案所造成的相關性。[confidence 實作](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/skill-audit/src/engine/mod.rs#L105)

同樣，`Benign` 只表示沒達到規則門檻，不能被理解為安全認證。掃描本身讀檔／執行失敗時，core 還會記錄「未稽核」後放行；這與「掃描通過」是不同狀態。[失敗處理](https://github.com/audichuang/aghub/blob/707781bb689cf86096dda122764505d0a73c8c8a/crates/core/src/skills/audit.rs#L86)

## 6. 你對「硬性掃描天然有問題」的質疑，哪些成立？

**對目前這種「通用正則命中就硬擋並標惡意」的設計，質疑成立，而且已有可執行的反例支持。**

根本矛盾不是再補幾個例外就會消失。讀 API key、發 HTTP 請求、安裝依賴、操作 shell，既可能是正常工作，也可能是攻擊。決定性的差別包含來源、接收者、使用者授權、實際資料流和執行上下文；這些沒有被目前 pattern 編碼。

規則加寬會擋更多正常技能；加窄又容許別名、分段與跨檔寫法通過。本次實驗同時呈現兩個方向，不能只用「再寫一條 regex」宣稱解決。這不是反對靜態分析，而是反對把它的證明能力誇大。

但不能據此推論「所有靜態掃描都沒有用」或「硬擋一律錯誤」。掃描仍能快速、離線、可重現地指出已知 payload 或值得查看的能力。若產品明確宣告「這個環境禁止任何下載即執行的安裝指令」，以規則執行這項能力政策可以合理；那應叫**違反環境政策**，而不是宣稱技能作者惡意。

另一個容易走錯的方向是「全交給 LLM 判斷」。LLM 可以幫忙理解上下文，但也是會錯的判斷器，且被掃描的技能內容本身就是不可信指令。它適合作為有來源證據的補充解釋，不能自動成為新的安全認證或讓技能自己的文字替自己取得授權。

## 7. 建議怎麼改：先改判定與使用者決策，不急著建大型分析器

以下是建議，**尚未實作，也不代表使用者已批准修改政策**。

| 優先序 | 建議                                                                                                           | 解決的問題                                                    |
| ------ | -------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- |
| 1      | 更新失敗直接顯示技能名、規則、相對檔名、命中位置及上下文；把規則描述與實際命中分開                             | 使用者能直接複核，不必猜是哪個技能                            |
| 1      | 保留 source commit、內容 digest、ruleset／engine 身分與拒絕紀錄；秘密值遮蔽                                    | 拒絕後仍能重建當次判斷，避免本次缺少精確輸入版本的問題        |
| 1      | 把「偵測訊號」「推定危害」「是否允許」分開；對本次這類模糊訊號使用「需審閱」                                   | 不再把匹配結果直接說成惡意事實                                |
| 2      | 以單個技能、固定內容與此次稽核結果為範圍，提供檢視後繼續／取消；內容或規則變動便重新評估                       | 避免使用者只剩反覆失敗或全域關閉掃描兩個選擇                  |
| 2      | 對 Critical 規則補正常用途、負面教學、重複描述和等價惡意寫法的對照集                                           | 同時約束誤報與漏報，不能只加一個 last30days 白名單            |
| 2      | 改善 `Benign`／`Malicious`／`High confidence` 文案，區分「無已知命中」「未掃描」「需要審閱」「已確認違反政策」 | 語氣與實際證據強度一致                                        |
| 3      | 必要時加入局部語法／資料流分析；真正敏感操作搭配 agent 的執行期權限與沙箱                                      | 更接近執行與授權邊界，但不聲稱靜態掃描能保證所有 runtime 行為 |

不建議的捷徑：只放行 `last30days` 名稱、只放行 `x.ai` 網域、忽略所有 Markdown、刪掉安全掃描，或要求每個技能作者把正常程式改寫成剛好避開 regex 的樣子。這些都沒有修正判斷能力與政策的錯配。

**對 `last30days` 的具體處置建議：將本次視為需要審閱的安裝建議與 API 使用，不能以六個 Critical 宣告惡意。** 是否允許這個固定版本，還應由使用者對其實際能力與來源做決定；本次沒有替使用者執行 override 或安裝。

## 8. 可重跑的證據與限制

- [probe.py](probe.py)：從固定 aghub commit 讀取原規則，執行六檔匹配、八組對照 assertion 和完整 L1 sweep。
- [probe-result.json](probe-result.json)：pattern 名、實際 matched text、行號、規則雜湊與測試結果。
- [remote-observation.json](remote-observation.json)：僅抽取該技能的 finding、lock 識別資訊及原始日誌雜湊，未保存其他請求或憑證。
- [source-manifest.json](source-manifest.json)：135 個上游檔案的 Git blob 與 SHA-256。
- [six-file-comparison.json](six-file-comparison.json)：六個目標檔案之已安裝／固定上游版本比較。

本機已驗證命令（來源快照在 `/tmp`，系統清理後須由固定 commit 重新取得）：

```sh
/tmp/last30days-audit-20260922/venv/bin/python \
  /Users/audi/GoogleDrive/research/aghub/docs/reports/2026-09-22-last30days-security-audit/probe.py \
  --repo /Users/audi/GoogleDrive/research/aghub \
  --source /tmp/last30days-audit-20260922/upstream
```

重建環境只需 Python 的 `yara-x==1.17.0` 與該固定 commit 的 `skills/last30days` 完整目錄。**不要僅用 GitHub tarball 當完整來源**：本次其缺少五個 media blob，必須對照 tree 補齊；一般 Git checkout 可避免 archive 排除檔案的問題。

本報告沒有做：完整 Rust API 更新交易重跑、安裝腳本執行、實際金鑰／瀏覽器 cookie 存取、所有第三方依賴及搜尋功能的動態稽核。未知部分保留為未知，不能拿「這六筆不足以證明惡意」推成「整包保證安全」。

[up-skill]: https://github.com/mvanhorn/last30days-skill/blob/349ca444b4fda466e74d471dffa2aff36bb997f1/skills/last30days/SKILL.md#L722
[up-prescriptions]: https://github.com/mvanhorn/last30days-skill/blob/349ca444b4fda466e74d471dffa2aff36bb997f1/skills/last30days/scripts/lib/prescriptions.py#L94-L102
[up-grok]: https://github.com/mvanhorn/last30days-skill/blob/349ca444b4fda466e74d471dffa2aff36bb997f1/skills/last30days/scripts/lib/grok_x.py#L1-L10
[up-health]: https://github.com/mvanhorn/last30days-skill/blob/349ca444b4fda466e74d471dffa2aff36bb997f1/skills/last30days/scripts/lib/health.py#L182-L200
[up-backends]: https://github.com/mvanhorn/last30days-skill/blob/349ca444b4fda466e74d471dffa2aff36bb997f1/skills/last30days/scripts/lib/backends.py#L297-L305
[up-eval]: https://github.com/mvanhorn/last30days-skill/blob/349ca444b4fda466e74d471dffa2aff36bb997f1/skills/last30days/scripts/evaluate_search_quality.py
