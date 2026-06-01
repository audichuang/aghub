import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed in this environment, and the
// task forbids adding dependencies, so this pure-logic test uses Node's
// built-in runner (`node --test --experimental-strip-types`). The antfu config
// enforces vitest over node:test, which does not apply here.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import {
	connectionToForm,
	EMPTY_CONNECTION_FORM,
	formToConnection,
	isFormValid,
	validateConnectionForm,
	type ConnectionFormState,
} from "./connection-form.ts";

function makeForm(partial: Partial<ConnectionFormState>): ConnectionFormState {
	return { ...EMPTY_CONNECTION_FORM, ...partial };
}

test("empty form is invalid (missing label + sshTarget)", () => {
	const errors = validateConnectionForm(EMPTY_CONNECTION_FORM);
	assert.equal(errors.label, "connRequiredLabel");
	assert.equal(errors.sshTarget, "connRequiredSshTarget");
	assert.equal(isFormValid(EMPTY_CONNECTION_FORM), false);
});

test("label of only whitespace is rejected", () => {
	const errors = validateConnectionForm(
		makeForm({ label: "   ", sshTarget: "vm" }),
	);
	assert.equal(errors.label, "connRequiredLabel");
	assert.equal(errors.sshTarget, undefined);
});

test("sshTarget of only whitespace is rejected", () => {
	const errors = validateConnectionForm(
		makeForm({ label: "My VM", sshTarget: "  " }),
	);
	assert.equal(errors.sshTarget, "connRequiredSshTarget");
});

test("minimal valid form has no errors", () => {
	const form = makeForm({ label: "My VM", sshTarget: "my-vm" });
	assert.deepEqual(validateConnectionForm(form), {});
	assert.equal(isFormValid(form), true);
});

test("blank optional port is valid", () => {
	const form = makeForm({ label: "VM", sshTarget: "vm", port: "" });
	assert.equal(validateConnectionForm(form).port, undefined);
});

test("port out of range is rejected", () => {
	assert.equal(
		validateConnectionForm(
			makeForm({ label: "VM", sshTarget: "vm", port: "0" }),
		).port,
		"connInvalidPort",
	);
	assert.equal(
		validateConnectionForm(
			makeForm({ label: "VM", sshTarget: "vm", port: "70000" }),
		).port,
		"connInvalidPort",
	);
	assert.equal(
		validateConnectionForm(
			makeForm({ label: "VM", sshTarget: "vm", port: "22.5" }),
		).port,
		"connInvalidPort",
	);
	assert.equal(
		validateConnectionForm(
			makeForm({ label: "VM", sshTarget: "vm", port: "abc" }),
		).port,
		"connInvalidPort",
	);
});

test("valid in-range port passes", () => {
	const form = makeForm({ label: "VM", sshTarget: "vm", port: "2222" });
	assert.equal(validateConnectionForm(form).port, undefined);
	assert.equal(isFormValid(form), true);
});

test("formToConnection omits blank optional fields", () => {
	const conn = formToConnection(makeForm({ label: "VM", sshTarget: "vm" }));
	assert.deepEqual(conn, { label: "VM", sshTarget: "vm" });
	assert.equal("user" in conn, false);
	assert.equal("port" in conn, false);
	assert.equal("remoteAghubPath" in conn, false);
});

test("formToConnection trims fields and includes provided optionals", () => {
	const conn = formToConnection(
		makeForm({
			label: "  My VM  ",
			sshTarget: " my-vm ",
			user: " root ",
			port: "2222",
			remoteAghubPath: " ~/.local/bin/aghub-api ",
		}),
	);
	assert.deepEqual(conn, {
		label: "My VM",
		sshTarget: "my-vm",
		user: "root",
		port: 2222,
		remoteAghubPath: "~/.local/bin/aghub-api",
	});
});

test("connectionToForm round-trips through formToConnection", () => {
	const form = makeForm({
		label: "My VM",
		sshTarget: "my-vm",
		user: "root",
		port: "2222",
		remoteAghubPath: "~/.local/bin/aghub-api",
	});
	const conn = formToConnection(form);
	const back = connectionToForm({ id: "x", ...conn });
	assert.deepEqual(back, form);
});

test("connectionToForm maps absent optionals to empty strings", () => {
	const back = connectionToForm({ id: "x", label: "VM", sshTarget: "vm" });
	assert.deepEqual(back, {
		label: "VM",
		sshTarget: "vm",
		user: "",
		port: "",
		remoteAghubPath: "",
	});
});
