import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
import { test } from "node:test";
import type { TFunction } from "i18next";
import {
	getKeyPairErrorMessage,
	validateHttpUrl,
	validateKeyPairs,
	validatePositiveInteger,
} from "./form-utils.ts";

/**
 * These four back the env-var and header editors in `create-mcp-panel` and
 * `edit-mcp-panel`. They decide whether a server definition can be saved at
 * all, and `validateHttpUrl` is a protocol allow-list — none of it was covered.
 *
 * `t` is the i18n lookup; returning the key itself makes assertions read as the
 * message id, which is what the panels render.
 */
const t = ((key: string) => key) as unknown as TFunction;

const pair = (key: string, value: string) => ({ key, value });

// ── key/value pairs ─────────────────────────────────────────────────────────

test("a wholly blank row is not an error — it is the empty next row", () => {
	assert.deepEqual(validateKeyPairs(t, [pair("", ""), pair("  ", "  ")]), [
		{},
		{},
	]);
});

test("a row with only one side filled names the missing side", () => {
	assert.deepEqual(validateKeyPairs(t, [pair("", "v")]), [
		{ key: "validationKeyRequired" },
	]);
	assert.deepEqual(validateKeyPairs(t, [pair("k", "")]), [
		{ value: "validationValueRequired" },
	]);
});

test("errors are positional — index i describes row i", () => {
	const errors = validateKeyPairs(t, [
		pair("ok", "1"),
		pair("", "2"),
		pair("also-ok", "3"),
	]);
	assert.deepEqual(errors, [{}, { key: "validationKeyRequired" }, {}]);
});

test("a duplicate key is flagged on EVERY row that carries it", () => {
	const errors = validateKeyPairs(t, [
		pair("A", "1"),
		pair("B", "2"),
		pair("A", "3"),
	]);
	assert.deepEqual(errors, [
		{ key: "validationDuplicateKey" },
		{},
		{ key: "validationDuplicateKey" },
	]);
});

test("duplicates are detected after trimming, so ' A' collides with 'A'", () => {
	const errors = validateKeyPairs(t, [pair(" A ", "1"), pair("A", "2")]);
	assert.deepEqual(errors, [
		{ key: "validationDuplicateKey" },
		{ key: "validationDuplicateKey" },
	]);
});

test("a unique key is never flagged as a duplicate", () => {
	assert.deepEqual(validateKeyPairs(t, [pair("A", "1"), pair("B", "2")]), [
		{},
		{},
	]);
});

// An incomplete row returns before it is recorded, so it cannot collide. Pinned
// because it is the non-obvious half of the interaction: fill in the missing
// value and the duplicate error appears, which looks like a new bug otherwise.
test("a row missing its value is not counted toward duplicates", () => {
	assert.deepEqual(validateKeyPairs(t, [pair("A", "1"), pair("A", "")]), [
		{},
		{ value: "validationValueRequired" },
	]);
});

test("getKeyPairErrorMessage returns the first key error, then the first value error", () => {
	assert.equal(getKeyPairErrorMessage([]), undefined);
	assert.equal(getKeyPairErrorMessage([{}, {}]), undefined);
	assert.equal(
		getKeyPairErrorMessage([{}, { value: "v1" }, { key: "k1" }]),
		"v1",
	);
	assert.equal(getKeyPairErrorMessage([{ key: "k0", value: "v0" }]), "k0");
});

// ── url ─────────────────────────────────────────────────────────────────────

test("a blank url is reported as required", () => {
	assert.equal(validateHttpUrl("", t), "validationUrlRequired");
	assert.equal(validateHttpUrl("   ", t), "validationUrlRequired");
});

test("only http and https are accepted", () => {
	assert.equal(validateHttpUrl("https://e.test/mcp", t), true);
	assert.equal(validateHttpUrl("http://e.test/mcp", t), true);
	for (const raw of [
		"file:///etc/passwd",
		"javascript:alert(1)",
		"data:text/html,x",
		"ftp://e.test/x",
		"aghub://import",
		// Schemes that CONTAIN "http" but are not it. Without these the two
		// equality checks can be collapsed into `protocol.includes("http")`
		// and every case above still passes.
		"httpx://e.test/x",
		"xhttp://e.test/x",
		"https-evil://e.test/x",
	]) {
		assert.equal(validateHttpUrl(raw, t), "validationUrlProtocol", raw);
	}
});

test("an unparseable url is reported as invalid, not as a protocol problem", () => {
	assert.equal(validateHttpUrl("not a url", t), "validationUrlInvalid");
	assert.equal(
		validateHttpUrl("://missing-scheme", t),
		"validationUrlInvalid",
	);
});

// ── timeout ─────────────────────────────────────────────────────────────────

test("an empty timeout is allowed — the field is optional", () => {
	assert.equal(validatePositiveInteger("", t), true);
	assert.equal(validatePositiveInteger("   ", t), true);
});

test("a positive integer is accepted", () => {
	assert.equal(validatePositiveInteger("1", t), true);
	assert.equal(validatePositiveInteger("30", t), true);
	assert.equal(validatePositiveInteger("007", t), true);
});

test("zero and anything that is not all digits is rejected", () => {
	for (const raw of ["0", "-1", "1.5", "1e3", "30s", " 30", "abc", "+1"]) {
		assert.equal(
			validatePositiveInteger(raw, t),
			"validationTimeoutPositiveInteger",
			raw,
		);
	}
});
