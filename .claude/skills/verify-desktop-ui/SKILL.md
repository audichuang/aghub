---
name: verify-desktop-ui
description: Verify aghub's desktop UI by launching the real frontend in headless Chromium (Playwright) against a real aghub-api — run it, screenshot it, and assert on the rendered DOM and on the URL after a click. Use when a change under crates/desktop needs checking beyond typecheck/lint/unit tests, when reviewing a desktop diff, when a claim like "clicking X opens Y" needs settling, or when a desktop UI bug cannot be reproduced by reading code; also on 跑一下桌面版 / 截圖看看 / 這頁點下去會怎樣 / 驗證桌面 UI. Not for aghub's CLI or its skill-install chain — that is the `verify` skill.
---

# Verifying the desktop UI without opening a window

`typecheck`, `lint` and the unit tests all pass on a page that renders an empty
list; they never look at what is drawn. `tauri dev` does, but it needs a
display, throws a window onto the user's desktop, and cannot be scripted.

This runs the real frontend in headless Chromium against a real `aghub-api`, so
you can screenshot it and assert on the DOM.

## Why a shim is needed

Every byte the frontend renders arrives over Tauri IPC: `invoke("start_server")`
returns a port, and everything else is `http://localhost:<port>/api/v1`. The
store, event, menu and updater plugins are IPC too. Open the vite dev server in
a plain browser and the app hangs on connection. Inject
`window.__TAURI_INTERNALS__` before the page's first script and it believes it
is inside Tauri — that is all `scripts/run-scenarios.mjs` does.

## Before you start

- `playwright-core` plus a downloaded chromium. The script hunts a few known
  locations and exits 2 with instructions if it finds none; you can point it
  with `PLAYWRIGHT_PATH`, or install with
  `npx playwright@1.58.2 install chromium`.
- `crates/desktop/node_modules` populated (`bun install`). Bun only.
- **Ports 1420 and 8899 free.** This matters more than it sounds: vite uses
  `strictPort`, so a busy 1420 kills it with the error buried in its log — and
  the harness then happily drives whatever else is serving 1420. Another aghub
  build there looks completely normal in the screenshot.

## Run it

```bash
ss -ltn | grep -E ':(1420|8899) ' && { echo "port busy"; exit 1; }

cargo build -p aghub-api --bin aghub-api
# The REPO's build, not ~/.cargo/bin — an older installed copy 404s on routes
# this branch added, which reads as a frontend bug. It may also already be
# running (the desktop app embeds one), so confirm 8899 is yours.
setsid nohup ./target/debug/aghub-api --port 8899 > /tmp/api.log 2>&1 < /dev/null & disown

cd crates/desktop && setsid nohup bun run dev > /tmp/vite.log 2>&1 < /dev/null & disown
sleep 5 && grep -q "ready in" /tmp/vite.log || { cat /tmp/vite.log; exit 1; }
curl -sf localhost:8899/api/v1/agents > /dev/null || { echo "api down"; exit 1; }

# Write your scenarios first (shape below), then:
node .claude/skills/verify-desktop-ui/scripts/run-scenarios.mjs \
  /tmp/scenarios.json --out /tmp/aghub-shots
```

Keep the scenarios file and the output under `/tmp` — neither is gitignored at
the repo root. `API_PORT`, `DEV_URL` and `PROJECTS` (a JSON file of
`{id,name,path}` seeded as open projects, which is how you reach project scope)
are the env knobs.

**Read-only scenarios do not mean read-only HTTP.** Simply loading `/skills`
POSTs to `/skills/repair` several times; that is safe only because the endpoint
defaults `dry_run` to true. The API reads the user's real `~/.agents` and
`~/.claude`, so a scenario that installs, deletes or updates a skill is
operating on their actual machine. Keep to navigation and assertions. If you
genuinely must test a mutation, bring the API up under an isolated `HOME` + XDG
the way the `verify` skill does, not against the real home.

## Scenario shape

The acceptance scenarios in a `.scratch/<feature>/spec.md` written by
`plan-desktop-ui` are what you translate into this format.

```json
[
	{
		"name": "source-deeplink",
		"url": "/skills?source=owner/repo",
		"settle": 4000,
		"dark": false,
		"steps": [
			{
				"eval": "({url: location.search, h1: document.querySelector('h1')?.innerText})"
			},
			{ "clickLast": "[aria-label='在來源中檢視']" },
			{ "wait": 2500 },
			{ "eval": "document.querySelectorAll('[role=option]').length" },
			{ "screenshot": "after-click" }
		]
	}
]
```

Per scenario: `url`, `settle`, optional `dark` and `viewport`. Steps: `click` /
`clickLast` (add `role` + `exact` to go through `getByRole`), `fill` + `value`,
`eval` (any expression, promises awaited), `screenshot`, `wait`. A key the
dispatcher does not recognise is skipped silently, so a typo like `screenShot`
produces a green scenario that did nothing.

## Traps, each of which cost real time here

| Symptom                                    | Cause                                                                                                                                                                     |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Every `goto` times out                     | `waitUntil: "networkidle"`. A source panel really fetches a git remote. The script uses `domcontentloaded` + an explicit `settle`; raise `settle` rather than reverting   |
| Page blank, `rows: 0`, no obvious error    | `plugin:store\|get` returned a bare value instead of `[value, exists]` — the plugin destructures it and throws `is not iterable` during store init, killing the whole app |
| `object null is not iterable` on startup   | `plugin:menu\|*` must return `[rid, id]`                                                                                                                                  |
| Clicks fail but `eval` assertions all pass | A welcome overlay: `onboardingProgress` was not seeded complete. Pure-`eval` scenarios stay green through this, so it hides well                                          |
| Clicked the wrong element                  | aria-labels repeat — `在來源中檢視` appears 8 times on the skills page. Use `clickLast` for the detail panel, or scope the selector                                       |
| A click times out at 8s on a visible thing | Something invisible is over it, usually a popover left open by the previous step. Close it, or click through a different affordance                                       |
| Update badges / rollups all read absent    | The online check is real network and takes about a minute. Raise `settle` until the badge appears; ~45s was not always enough                                             |
| Your own shell dies, exit 144              | A `pkill -f` or `pgrep -f` pattern that also matches the shell command string containing it. See Cleaning up                                                              |

## Reading the result

Read `<out>/results.json`, and then **open the screenshots themselves**
(`<out>/<scenario>.png`, plus `<scenario>-ERROR.png` for a failure). The step
log says what the harness did; only the image says what the page looked like.

Two things the exit code will not tell you, so check them by eye:

- **Console errors do not fail the run.** A page that threw into an error
  boundary still exits 0. The last line of each scenario block starts with
  `console:` — read it.
- **A dead API does not fail the run either.** The shim answers `start_server`
  unconditionally, so with no API up every request fails, the page renders its
  empty state, and every assertion "passes". That is why the readiness checks
  are in Run it.

Exit codes: **2** means the harness never started (no Playwright, missing
scenarios file); **1** means a scenario threw; **0** means every scenario ran to
the end, which is not the same as every scenario being right.

When a line contradicts what you expected, the default is that **the output is
right and your explanation is wrong**. Explaining an anomaly away as test noise
("probably clicked the wrong row") discards the one thing that would have found
the bug — that exact mistake happened here, and the regression surfaced two
reviews later instead.

## Cleaning up

Kill by PID, and get the PID from the port rather than from a pattern — a
pattern wide enough to match the process is also wide enough to match the shell
you are typing it into, and that is the exit-144 trap above.

```bash
ss -ltnp 'sport = :1420' 'sport = :8899' |
  grep -oP 'pid=\K[0-9]+' | sort -u | xargs -r kill
```
