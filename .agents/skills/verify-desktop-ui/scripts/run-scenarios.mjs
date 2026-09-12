#!/usr/bin/env node
/**
 * Drive the aghub desktop frontend in headless Chromium against a real
 * `aghub-api`, with a Tauri IPC shim standing in for the Tauri shell.
 *
 * Why this exists: every byte the frontend renders arrives through
 * `invoke("start_server")` -> `http://localhost:<port>/api/v1`, and the store,
 * event and menu plugins are IPC too. Open the vite dev server in a plain
 * browser and nothing loads. Inject `window.__TAURI_INTERNALS__` before the
 * page's first script and the app believes it is inside Tauri.
 *
 * Usage:
 *   node run-scenarios.mjs <scenarios.json> [--out DIR]
 * Env:
 *   API_PORT  (default 8899)   port the aghub-api you started is listening on
 *   DEV_URL   (default http://localhost:1420)
 *   PROJECTS  path to a JSON array of {id,name,path} to seed as open projects
 */
import { createRequire } from "node:module";
import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import path from "node:path";

// Playwright is not a dependency of this repo. Point at whatever copy the
// machine already has rather than installing one; `chromium-<rev>` must exist
// under ~/.cache/ms-playwright for the matching playwright-core.
const PW_PATHS = [
	process.env.PLAYWRIGHT_PATH,
	`${process.env.HOME}/.npm/_npx/a8a7eec953f1f314/node_modules/`,
	`${process.env.HOME}/node_modules/`,
].filter(Boolean);
let chromium;
for (const p of PW_PATHS) {
	try {
		({ chromium } = createRequire(p)("playwright-core"));
		break;
	} catch {
		/* try the next one */
	}
}
if (!chromium) {
	console.error(
		"playwright-core not found. Set PLAYWRIGHT_PATH to a node_modules dir that has it,\n" +
			"or run: npx playwright@1.58.2 install chromium",
	);
	process.exit(2);
}

const API_PORT = Number(process.env.API_PORT ?? 8899);
const DEV = process.env.DEV_URL ?? "http://localhost:1420";
const outFlag = process.argv.indexOf("--out");
const OUT = path.resolve(outFlag > -1 ? process.argv[outFlag + 1] : "shots");
mkdirSync(OUT, { recursive: true });

const projects = process.env.PROJECTS
	? JSON.parse(readFileSync(process.env.PROJECTS, "utf8"))
	: [];

/**
 * The seeded Tauri store. `version` must equal `CURRENT_VERSION` in
 * `src/lib/store/types.ts` or migrations run against a store that is not there;
 * `onboardingProgress` must be complete or the welcome overlay covers the page
 * and every scenario times out on an element it can see but cannot click.
 */
const STORE = {
	version: 7,
	projects,
	connections: [],
	onboardingProgress: {
		hasSeenWelcome: true,
		completedTours: { productMap: true, projectWorkflow: true },
	},
	disabledAgents: [],
	starredSkills: [],
	starredMcps: [],
	integrationPreferences: {},
};

// Modeled on @tauri-apps/api/mocks.js. Two return shapes are not guessable and
// cost an afternoon each if you get them wrong — see the comments inline.
const SHIM = `(() => {
  const store = new Map(Object.entries(${JSON.stringify(STORE)}));
  const callbacks = new Map();
  const listeners = new Map();
  const registerCallback = (cb, once = false) => {
    const id = crypto.getRandomValues(new Uint32Array(1))[0];
    callbacks.set(id, (data) => { if (once) callbacks.delete(id); return cb && cb(data); });
    return id;
  };
  window.__TAURI_SHIM_LOG__ = [];
  async function invoke(cmd, args = {}) {
    window.__TAURI_SHIM_LOG__.push(cmd);
    if (cmd.startsWith("plugin:store|")) {
      switch (cmd.slice("plugin:store|".length)) {
        case "load": case "get_store": return 1;               // an rid, not the store
        // NOT the bare value: the plugin unwraps a [value, exists] tuple, and
        // returning the value alone makes every read look like \`undefined\`.
        case "get": return store.has(args.key) ? [store.get(args.key), true] : [null, false];
        case "has": return store.has(args.key);
        case "set": store.set(args.key, args.value); return null;
        case "delete": return store.delete(args.key);
        case "keys": return [...store.keys()];
        case "values": return [...store.values()];
        case "entries": return [...store.entries()];
        case "length": return store.size;
        default: return null;                                   // save, reload, clear, reset
      }
    }
    if (cmd === "plugin:event|listen") {
      const l = listeners.get(args.event) ?? []; l.push(args.handler);
      listeners.set(args.event, l); return args.handler;        // the unlisten id
    }
    if (cmd === "plugin:event|unlisten" || cmd === "plugin:event|emit") return null;
    // [rid, id]. Returning null throws "object null is not iterable" out of
    // the menu plugin and the app logs a failure on every startup.
    if (cmd.startsWith("plugin:menu|")) return [1, "main"];
    switch (cmd) {
      case "start_server": return ${API_PORT};
      case "local_api_version": return "0.0.0-harness";
      case "get_last_skill_check": return null;
      case "get_skill_check_schedule": return { enabled: false };
      case "list_bound_sources": return [];
      case "plugin:deep-link|get_current": return null;
      case "plugin:autostart|is_enabled": return false;
      case "plugin:updater|check": return null;
      case "plugin:window|is_maximized": return false;
      case "plugin:window|theme": return "light";
      default: return null;   // unknown commands answer harmlessly, on purpose
    }
  }
  window.__TAURI_INTERNALS__ = {
    invoke, transformCallback: registerCallback,
    unregisterCallback: (id) => callbacks.delete(id),
    runCallback: (id, data) => callbacks.get(id)?.(data),
    convertFileSrc: (p) => p,
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
    plugins: { path: { sep: "/", delimiter: ":" } },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  window.isTauri = true;
})();`;

const scenariosPath = process.argv[2];
if (!scenariosPath || !existsSync(scenariosPath)) {
	console.error("usage: node run-scenarios.mjs <scenarios.json> [--out DIR]");
	process.exit(2);
}
const scenarios = JSON.parse(readFileSync(scenariosPath, "utf8"));

const browser = await chromium.launch({ headless: true });
const results = [];

for (const sc of scenarios) {
	const ctx = await browser.newContext({
		viewport: sc.viewport ?? { width: 1400, height: 860 },
		colorScheme: sc.dark ? "dark" : "light",
	});
	await ctx.addInitScript(SHIM);
	const page = await ctx.newPage();
	const errors = [];
	page.on("console", (m) => {
		if (m.type() === "error") errors.push(m.text().slice(0, 300));
	});
	page.on("pageerror", (e) =>
		errors.push("pageerror: " + String(e).slice(0, 300)),
	);

	const r = { name: sc.name, steps: [], errors };
	try {
		// domcontentloaded, never networkidle: a panel that fetches a git
		// remote (a source diff, an online update check) keeps the network
		// busy for tens of seconds and networkidle simply times out.
		await page.goto(DEV + sc.url, { waitUntil: "domcontentloaded" });
		await page.waitForTimeout(sc.settle ?? 3000);

		for (const st of sc.steps ?? []) {
			// Order matters: a step may carry `wait` ALONGSIDE an action, so
			// every action is tested before the bare `wait` fallback.
			if (st.click || st.clickLast) {
				const sel = st.click ?? st.clickLast;
				const loc = st.role
					? page.getByRole(st.role, {
							name: sel,
							exact: st.exact ?? false,
						})
					: page.locator(sel);
				// `.last()` reaches the detail panel when the same aria-label
				// also appears on every list row above it.
				await (st.clickLast ? loc.last() : loc.first()).click({
					timeout: 8000,
				});
				await page.waitForTimeout(st.wait ?? 700);
				r.steps.push(`click ${sel} -> ${page.url().replace(DEV, "")}`);
			} else if (st.fill) {
				await page.locator(st.fill).fill(st.value);
				await page.waitForTimeout(st.wait ?? 700);
				r.steps.push(`fill ${st.fill} = ${st.value}`);
			} else if (st.eval) {
				const v = await page.evaluate(st.eval);
				r.steps.push(
					`eval => ${String(JSON.stringify(v) ?? v).slice(0, 400)}`,
				);
			} else if (st.screenshot) {
				await page.screenshot({
					path: path.join(OUT, st.screenshot + ".png"),
				});
				r.steps.push(`shot ${st.screenshot}`);
			} else if (st.wait) {
				await page.waitForTimeout(st.wait);
			}
		}
		r.finalUrl = page.url().replace(DEV, "");
		await page.screenshot({ path: path.join(OUT, sc.name + ".png") });
	} catch (e) {
		r.error = String(e).slice(0, 500);
		await page
			.screenshot({ path: path.join(OUT, sc.name + "-ERROR.png") })
			.catch(() => {});
	}
	results.push(r);
	await ctx.close();
}

await browser.close();
writeFileSync(path.join(OUT, "results.json"), JSON.stringify(results, null, 2));

let failed = 0;
for (const r of results) {
	console.log(`\n## ${r.name}  -> ${r.finalUrl ?? "(error)"}`);
	for (const s of r.steps) console.log("  - " + s);
	if (r.error) {
		console.log("  ERROR: " + r.error);
		failed++;
	}
	if (r.errors.length)
		console.log("  console: " + r.errors.slice(0, 3).join(" | "));
}
console.log(`\nscreenshots + results.json in ${OUT}`);
process.exit(failed > 0 ? 1 : 0);
