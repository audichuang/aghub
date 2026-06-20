import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { AgentSkillCoverageDto } from "../generated/dto";
import {
	isAutoCoveredByMaster,
	needsMasterLink,
	partitionByCoverage,
} from "./agent-capabilities.ts";

function cov(
	id: string,
	over: Partial<AgentSkillCoverageDto>,
): AgentSkillCoverageDto {
	return {
		id,
		scope: "global",
		reads_master: false,
		writes_master: false,
		needs_link: false,
		auto_covered: false,
		supported: true,
		...over,
	};
}

test("isAutoCoveredByMaster / needsMasterLink read the server booleans", () => {
	assert.equal(isAutoCoveredByMaster(cov("a", { auto_covered: true })), true);
	assert.equal(isAutoCoveredByMaster(cov("a", { needs_link: true })), false);
	assert.equal(isAutoCoveredByMaster(undefined), false);
	assert.equal(needsMasterLink(cov("b", { needs_link: true })), true);
	assert.equal(needsMasterLink(cov("b", { auto_covered: true })), false);
	assert.equal(needsMasterLink(undefined), false);
});

test("partitionByCoverage splits installable into autoCovered + linkTargets", () => {
	const installable = [{ id: "codex" }, { id: "claude" }, { id: "zed" }];
	const coverage: Record<string, AgentSkillCoverageDto> = {
		codex: cov("codex", { auto_covered: true, reads_master: true }),
		claude: cov("claude", { needs_link: true }),
		zed: cov("zed", { needs_link: true }),
	};
	const { autoCovered, linkTargets } = partitionByCoverage(
		installable,
		coverage,
	);
	assert.deepEqual(
		autoCovered.map((a) => a.id),
		["codex"],
	);
	assert.deepEqual(
		linkTargets.map((a) => a.id),
		["claude", "zed"],
	);
});

test("partitionByCoverage uses needs_link/auto_covered only, not reads/writes_master", () => {
	const installable = [{ id: "amp" }];
	const coverage: Record<string, AgentSkillCoverageDto> = {
		amp: cov("amp", { reads_master: true, needs_link: true }),
	};
	const { autoCovered, linkTargets } = partitionByCoverage(
		installable,
		coverage,
	);
	assert.deepEqual(autoCovered, []);
	assert.deepEqual(
		linkTargets.map((a) => a.id),
		["amp"],
	);
});

test("partitionByCoverage drops nothing: missing coverage entry is neither bucket", () => {
	const installable = [{ id: "ghost" }];
	const { autoCovered, linkTargets } = partitionByCoverage(installable, {});
	assert.deepEqual(autoCovered, []);
	assert.deepEqual(linkTargets, []);
});
