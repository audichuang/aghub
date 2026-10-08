import assert from "node:assert/strict";
import { test } from "node:test";
import { HTTPError } from "ky";
import {
	isSharedMasterRefusal,
	keptDeleteMessage,
	keptDeleteMessageFromError,
} from "./skill-delete-message.ts";

// Echoes the key so the assertion is about WHICH localized string was picked,
// not about any one locale's wording.
const t = (key: string, options: { name: string; path?: string }) =>
	`${key}|${options.name}|${options.path ?? ""}`;

// The reported bug: the server's English `error` replaced the localized text.
test("a git refusal shows the localized message, not the server's English", () => {
	const message = keptDeleteMessage(
		{
			error: "/p/.agents/skills/x is tracked by git, so deleting it is refused; ...",
			skipped: ["/p/.agents/skills/x"],
		},
		"x",
		t,
	);
	assert.equal(message, "deleteSkillKeptGit|x|/p/.agents/skills/x");
});

test("a shared-reader keep (no server error) keeps the generic message", () => {
	assert.equal(
		keptDeleteMessage({ skipped: ["/p/.agents/skills/x"] }, "x", t),
		"deleteSkillKeptSharedMaster|x|",
	);
});

test("isSharedMasterRefusal matches 422 UNSUPPORTED_OPERATION HTTP error", () => {
	const err422 = new HTTPError(
		new Response(null, { status: 422 }),
		new Request("http://x"),
		{} as any,
	);
	err422.data = { code: "UNSUPPORTED_OPERATION", error: "preflight failed" };
	assert.equal(isSharedMasterRefusal(err422), true);

	const err400 = new HTTPError(
		new Response(null, { status: 400 }),
		new Request("http://x"),
		{} as any,
	);
	err400.data = { code: "INVALID_CONFIG", error: "bad" };
	assert.equal(isSharedMasterRefusal(err400), false);

	assert.equal(isSharedMasterRefusal(new Error("generic")), false);
});

test("keptDeleteMessageFromError formats localized git message from git-kind 422 error", () => {
	const errGit = new HTTPError(
		new Response(null, { status: 422 }),
		new Request("http://x"),
		{} as any,
	);
	errGit.data = {
		code: "UNSUPPORTED_OPERATION",
		error: "/p/.agents/skills/foo is tracked by git, so deleting it is refused",
		rejected_targets: [
			{
				agent: "claude-code",
				reason: "/p/.agents/skills/foo is tracked by git, so deleting it is refused",
				kind: "git",
				path: "/p/.agents/skills/foo",
			},
		],
	};

	assert.equal(
		keptDeleteMessageFromError(errGit, "foo", t),
		"deleteSkillKeptGit|foo|/p/.agents/skills/foo",
	);
});

test("keptDeleteMessageFromError formats localized shared master message from shared-kind 422 error", () => {
	const errShared = new HTTPError(
		new Response(null, { status: 422 }),
		new Request("http://x"),
		{} as any,
	);
	errShared.data = {
		code: "UNSUPPORTED_OPERATION",
		error: "Cannot remove for this agent alone shared slot .agents/skills/foo is also read by other agents for Cursor agent",
		rejected_targets: [
			{
				agent: "cursor",
				reason: "Cannot remove for this agent alone shared slot .agents/skills/foo is also read by other agents",
				kind: "shared",
			},
		],
	};

	assert.equal(
		keptDeleteMessageFromError(errShared, "foo", t),
		"deleteSkillKeptSharedMaster|foo|",
	);

	const errOther = new Error("other");
	assert.equal(keptDeleteMessageFromError(errOther, "foo", t), null);
});
