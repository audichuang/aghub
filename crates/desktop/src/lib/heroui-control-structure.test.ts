import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";

const SRC = join(import.meta.dirname, "..");

function tsxFiles(dir: string): string[] {
	const out: string[] = [];
	for (const entry of readdirSync(dir)) {
		const full = join(dir, entry);
		if (statSync(full).isDirectory()) out.push(...tsxFiles(full));
		else if (entry.endsWith(".tsx")) out.push(full);
	}
	return out;
}

/** Blank out comments so a `<Checkbox …>` quoted in prose is not scanned. */
function stripComments(source: string): string {
	return source
		.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "))
		.replace(/\/\/[^\n]*/g, "");
}

/**
 * Since @heroui/react 3.2.x the ROOT of Checkbox/Switch is React Aria's
 * `CheckboxField`/`SwitchField` — a plain context div. The `<label>` and the
 * hidden `<input>` live in `CheckboxButton`/`SwitchButton`, exposed as
 * `.Content`. So:
 *
 *   - `.Control` with no `.Content`  -> renders NO input at all. Dead control.
 *   - `.Control` beside `.Content`   -> input exists, but the visible box sits
 *                                       outside the clickable label.
 *   - `.Control` inside `.Content`   -> correct.
 *
 * Every one of those renders without a TypeScript or eslint error, because
 * `children` is `ReactNode` — which is exactly how a dependency bump silently
 * killed every toggle and checkbox in the app at once. This test is the only
 * thing standing between that and the next bump.
 */
test("every Checkbox/Switch renders .Control inside .Content", () => {
	const offenders: string[] = [];
	for (const file of tsxFiles(SRC)) {
		const source = stripComments(readFileSync(file, "utf8"));
		for (const comp of ["Checkbox", "Switch"] as const) {
			const open = new RegExp(`<${comp}(?=[\\s>])`, "g");
			for (const m of source.matchAll(open)) {
				const close = source.indexOf(`</${comp}>`, m.index);
				if (close === -1) continue;
				const body = source.slice(m.index, close);
				if (!body.includes(`<${comp}.Control`)) continue;
				const rel = file.slice(SRC.length + 1);
				if (!body.includes(`<${comp}.Content`)) {
					offenders.push(
						`${rel}: <${comp}.Control> with no .Content`,
					);
					continue;
				}
				const control = body.indexOf(`<${comp}.Control`);
				const contentOpen = body.indexOf(`<${comp}.Content`);
				const contentClose = body.indexOf(`</${comp}.Content>`);
				if (!(contentOpen < control && control < contentClose)) {
					offenders.push(
						`${rel}: <${comp}.Control> is a sibling of .Content, not inside it`,
					);
				}
			}
		}
	}
	assert.deepEqual(
		offenders,
		[],
		`HeroUI control(s) render a dead click target:\n  ${offenders.join("\n  ")}\n` +
			"Nest <X.Control> inside <X.Content>; if .Content carries a layout " +
			"className, move it to a wrapper div around the original children.",
	);
});
