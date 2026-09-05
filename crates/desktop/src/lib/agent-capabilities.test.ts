import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { AgentSkillCoverageDto } from "../generated/dto";
import {
	expandSelection,
	groupAgentsBySlot,
	needsMasterLink,
	sharedWith,
} from "./agent-capabilities.ts";

function cov(
	id: string,
	over: Partial<AgentSkillCoverageDto>,
): AgentSkillCoverageDto {
	return {
		id,
		scope: "global",
		needs_link: false,
		supported: true,
		shared_with: [],
		...over,
	};
}

test("needsMasterLink / sharedWith read the server fields", () => {
	assert.equal(needsMasterLink(cov("b", { needs_link: true })), true);
	assert.equal(needsMasterLink(undefined), false);
	assert.deepEqual(sharedWith(cov("cline", { shared_with: ["warp"] })), [
		"warp",
	]);
	assert.deepEqual(sharedWith(undefined), []);
});

test("an agent with its own directory is its own group", () => {
	const groups = groupAgentsBySlot([{ id: "claude" }], {
		claude: cov("claude", { needs_link: true }),
	});
	assert.equal(groups.length, 1);
	assert.equal(groups[0]?.shared, false);
	assert.deepEqual(
		groups[0]?.members.map((a) => a.id),
		["claude"],
	);
});

// The point of the whole grouping. cline and warp have no skills directory of
// their own: both read `.agents/skills`, so one write grants to both. Offering
// them as two checkboxes promises a per-agent choice the filesystem cannot keep.
test("agents sharing a directory come back as ONE group", () => {
	const installable = [{ id: "cline" }, { id: "warp" }, { id: "claude" }];
	const coverage: Record<string, AgentSkillCoverageDto> = {
		cline: cov("cline", { needs_link: true, shared_with: ["warp"] }),
		warp: cov("warp", { needs_link: true, shared_with: ["cline"] }),
		claude: cov("claude", { needs_link: true }),
	};
	const groups = groupAgentsBySlot(installable, coverage);
	assert.equal(groups.length, 2, "cline+warp collapse into one row");
	const shared = groups.find((g) => g.shared);
	assert.deepEqual(shared?.members.map((a) => a.id).sort(), [
		"cline",
		"warp",
	]);
	assert.equal(
		groups.filter((g) => !g.shared).length,
		1,
		"claude keeps its own row",
	);
});

// A sharer the server names but that this list does not contain (not installed,
// filtered out) must not become a phantom checkbox for an agent the user cannot
// see.
test("a peer that is not installable is not pulled into the group", () => {
	const groups = groupAgentsBySlot([{ id: "cline" }], {
		cline: cov("cline", { needs_link: true, shared_with: ["warp"] }),
	});
	assert.equal(groups.length, 1);
	assert.equal(groups[0]?.shared, false, "no peer present, so not a group");
	assert.deepEqual(
		groups[0]?.members.map((a) => a.id),
		["cline"],
	);
});

test("an unsupported agent is offered no row at all", () => {
	const groups = groupAgentsBySlot([{ id: "jetbrains-ai" }], {
		"jetbrains-ai": cov("jetbrains-ai", {
			needs_link: false,
			supported: false,
		}),
	});
	assert.deepEqual(groups, []);
});

// What the request must carry. Submitting the raw checkbox state names one agent
// while the disk grants several, so the result rows disagree with the choice the
// user made.
test("selecting one member of a shared slot submits the whole slot", () => {
	const coverage: Record<string, AgentSkillCoverageDto> = {
		cline: cov("cline", { needs_link: true, shared_with: ["warp"] }),
		warp: cov("warp", { needs_link: true, shared_with: ["cline"] }),
		claude: cov("claude", { needs_link: true }),
	};
	assert.deepEqual(
		expandSelection(["cline"], coverage, [
			"cline",
			"warp",
			"claude",
		]).sort(),
		["cline", "warp"],
	);
	assert.deepEqual(
		expandSelection(["claude"], coverage, ["cline", "warp", "claude"]),
		["claude"],
		"a private directory expands to nobody",
	);
});

test("expansion never names an agent outside the installable set", () => {
	const coverage: Record<string, AgentSkillCoverageDto> = {
		cline: cov("cline", { needs_link: true, shared_with: ["warp"] }),
	};
	assert.deepEqual(expandSelection(["cline"], coverage, ["cline"]), [
		"cline",
	]);
});
