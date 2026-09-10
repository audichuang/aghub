# Hindsight 知識頁狀態（2026-09-10）

## 檢查現況（不需要 MCP，直接打 API）

```sh
cd /tmp && python3 - <<'PY'
import json,urllib.request
cfg=json.load(open('/home/audichuang/.hindsight/coding-agent.json'))
B=cfg['apiUrl']+'/v1/default/banks/aghub'; T=cfg['apiToken']
def q(p):
    r=urllib.request.Request(B+p,headers={'Authorization':f'Bearer {T}'})
    return json.load(urllib.request.urlopen(r,timeout=60))
def walk(ns,d=0):
    for n in ns:
        if n['kind']=='page':
            print('  '*d, n['name'], len(q('/knowledge-base/pages/'+n['id']).get('body') or ''), 'chars')
        else: print('  '*d, '[', n['name'], ']')
        walk(n.get('children',[]),d+1)
walk(q('/knowledge-base/tree')['roots'])
PY
```

## 兩張正文還空的頁（會自己補上，不要手動催）

`遠端 VM 管理與憑證轉發`、`桌面殼層與 UI 驗證`。素材文件已在事實庫
（`vm-git-aghub`、`api-tauri`），兩張都已把重建負載調小
（`max_tokens` 2048、`recall_max_tokens` 1024、`include_chunks: false`），
`refresh_after_consolidation: true` 會持續重試。要手動催就**單獨發一張**，
且先確認 refresh 隊列沒有 pending/processing。

## 兩張總覽頁的舊敘述，刻意留著

`Key decisions and rationale` 的 D6 段（寫「native readers with no link yet」）
與 `Core concepts` 的 Master 回收句（寫「the view that reads the Master directly」）
都是舊敘述。delta 重建會**逐字保留**既有段落，所以 Correction 改不動它們；
而強迫 full regeneration 會把累積出來的 43K／32K 正文壓回 `max_tokens`（4096）
的上限，等於用縮水換一句更正 —— 不值得，已還原。

依家規：**Correction 與舊頁並存時，下一場以 Correction 為準。**
庫裡的 Correction 文件（reflect／search 立刻讀得到）：

- `correction-d6-referrer-native-reader` —— D6 的授權推導是 `shape::readers_of`，沒有 native reader 這一類
- `correction-master-master-view` —— Master 回收的條件只有「沒有任何 view 還引用它」
- `correction-sub-agent-sanitise` —— sub-agent 名字的 `..` 與空 sanitise 已被 `ensure_usable_component` 擋掉
- `correction-store-aghub-agent-hub` —— store 是 `.aghub`，`.agent-hub` 是被否決的名字

要把舊段落真的改掉，只能由人在控制台編輯那兩張頁。

## 踩過的坑

見記憶 `hindsight-dead-page-delta-trap`（refresh 死亡螺旋、ingest 會自己排全頁
refresh、delta 逐字保留舊段落）與 `hindsight-mcp-harness-env`（MCP 連不上的真因）。
