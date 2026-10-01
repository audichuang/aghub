import assert from "node:assert/strict";
import { test } from "node:test";
import { keptDeleteMessage } from "./skill-delete-message.ts";

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
