import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed in this environment, so this
// pure-logic test uses Node's built-in runner, matching connection-logic.test.ts.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import { DEFAULT_INSTALL_LAYOUT, isUniversalLayout } from "./install-layout.ts";

test("default install layout is isolation (copy), never universal", () => {
	// The spec default is isolation: copy into each selected agent's own skills
	// dir and never touch `.agents`. The Sources page previously hardcoded
	// universal, leaking a non-default layout with no user choice.
	assert.equal(DEFAULT_INSTALL_LAYOUT, "isolation");
	assert.equal(isUniversalLayout(DEFAULT_INSTALL_LAYOUT), false);
});

test("isUniversalLayout maps each mode to the gitInstall `universal` flag", () => {
	assert.equal(isUniversalLayout("universal"), true);
	assert.equal(isUniversalLayout("isolation"), false);
});
