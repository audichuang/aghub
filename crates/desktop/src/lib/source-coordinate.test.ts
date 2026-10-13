import assert from "node:assert/strict";
import { test } from "node:test";
import { sourceCoordinate } from "./source-coordinate.ts";

test("sourceCoordinate prefers the clone URL", () => {
	assert.equal(
		sourceCoordinate({
			source: "acme/skills",
			sourceUrl: "https://github.com/acme/skills.git",
		}),
		"https://github.com/acme/skills.git",
	);
});

test("sourceCoordinate treats the empty-string URL sentinel as absent", () => {
	// A lock entry with no recorded URL carries "" — `??` would key on it.
	assert.equal(
		sourceCoordinate({ source: "acme/skills", sourceUrl: "" }),
		"acme/skills",
	);
	assert.equal(
		sourceCoordinate({ source: "acme/skills", sourceUrl: null }),
		"acme/skills",
	);
});
