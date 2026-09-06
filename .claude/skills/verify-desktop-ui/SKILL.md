---
name: verify-desktop-ui
description: Run aghub's desktop frontend headlessly against a real aghub-api and assert on what it actually renders — screenshots, DOM state, URL after a click. Use when a change to crates/desktop needs checking beyond typecheck/lint/unit tests, when reviewing someone else's desktop diff, when a claim like "clicking X opens Y" needs settling, or when a UI bug cannot be reproduced by reading code. Also use before shipping a desktop release.
---

# Verifying the desktop UI without opening a window

`bun run typecheck && bun run lint:check && bun run test` all pass on a page
that renders an empty list. They check types and pure helpers; they say nothing
about what the app draws. `tauri dev` shows you, but it needs a display, throws
a window onto the user's desktop, and cannot be scripted.

This runs the real frontend in headless Chromium against a real `aghub-api`, so
you can screenshot it and assert on the DOM.

## Why a shim is needed

Every byte the frontend renders arrives over Tauri IPC: `invoke("start_server")`
returns a port, and everything else is `http://localhost:<port>/api/v1`. The
store, event, menu and updater plugins are IPC too. Open the vite dev server in
a plain browser and the app hangs on connection. Inject
`window.__TAURI_INTERNALS__` before the page's first script and it believes it
is inside Tauri — that is all `scripts/run-scenarios.mjs` does.

## Run it

```bash
# 1. The API. Use the REPO's build, not ~/.cargo/bin — a stale installed copy
#    404s on routes this branch added, and it looks like a frontend bug.
cargo build -p aghub-api --bin aghub-api
setsid nohup ./target/debug/aghub-api --port 8899 > /tmp/api.log 2>&1 < /dev/null & disown

# 2. The frontend (vite pins 1420, strictPort).
cd crates/desktop && setsid nohup bun run dev > /tmp/vite.log 2>&1 < /dev/null & disown

# 3. Scenarios.
node .claude/skills/verify-desktop-ui/scripts/run-scenarios.mjs scenarios.json --out shots
```

`API_PORT`, `DEV_URL` and `PROJECTS` (a JSON file of `{id,name,path}` to seed as
open projects, which is how you reach project scope) are the env knobs.

The API reads the real `~/.agents` and `~/.claude`, so **read-only scenarios are
safe and mutating ones are not** — a scenario that installs or deletes a skill
is operating on the user's actual machine. Keep scenarios to navigation and
assertions unless the user asked for a mutation test.

## Scenario shape

```json
[
	{
		"name": "source-deeplink",
		"url": "/skills?source=owner/repo",
		"settle": 4000,
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

Steps: `click` / `clickLast` (add `role` + `exact` for `getByRole`), `fill` +
`value`, `eval` (any expression, promises awaited), `screenshot`, `wait`.
Exit code is non-zero if any scenario threw.

## Traps, each of which cost real time here

| Symptom                                  | Cause                                                                                                                                                                                                               |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Every `goto` times out                   | `waitUntil: "networkidle"`. A source panel really fetches a git remote and the online update check takes ~40s. The script uses `domcontentloaded` + an explicit `settle`; raise `settle` rather than switching back |
| Page stuck behind a welcome overlay      | `onboardingProgress` not seeded complete. The script seeds it; keep it if you edit the store                                                                                                                        |
| Every store read is `undefined`          | `plugin:store\|get` must return `[value, exists]`, not the value                                                                                                                                                    |
| `object null is not iterable` on startup | `plugin:menu\|*` must return `[rid, id]`                                                                                                                                                                            |
| Clicked the wrong element                | aria-labels repeat: a list group header and the detail panel can share one. Use `clickLast`, or scope the selector                                                                                                  |
| A click silently does nothing            | Raw `el.click()` inside `eval` does not drive React Aria. Use a `click` step                                                                                                                                        |
| Rollup/badge assertions all read 0       | The online update check is real network. Those scenarios need `settle` around 45000                                                                                                                                 |
| Your own shell dies, exit 144            | `pkill -f "aghub-api --port 8899"` matches the shell command string that contains it. Kill by PID from `pgrep`                                                                                                      |

## Reading the result

The output is evidence, so treat it as evidence. When a line contradicts what
you expected, the default is that **the output is right and your explanation is
wrong**. Explaining an anomaly away as test noise ("probably clicked the wrong
row") discards the one thing that would have found the bug — that exact mistake
happened here, and the regression was caught two reviews later by someone
reading code.

This is also the reason the skill exists: a claim about _behavior_ ("clicking X
opens Y", "this state updates when Z") is not settled by reading code. Two
independent reviewers read the same sources here and produced a fluent, detailed,
wrong mechanism. Running it took a minute and settled it.

## Cleaning up

```bash
pgrep -f "bin/vite" | xargs -r kill
pgrep -f "target/debug/aghub-api --port 8899" | xargs -r kill
```
