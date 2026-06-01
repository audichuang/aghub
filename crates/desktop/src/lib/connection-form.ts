/**
 * Pure connection-form logic — NO React / Tauri / HeroUI imports.
 *
 * Everything here is synchronous and side-effect free so it can be unit
 * tested with Node's built-in test runner (`node --test`). The only import
 * is a type-only import (erased at strip time), so this module pulls in no
 * runtime dependencies.
 */
import type { Connection } from "./store/types";

/**
 * The editable shape of a connection form. `port` is a string while editing
 * (the raw NumberField text / "" when empty) and is normalized to a number
 * only when building the persisted Connection.
 */
export interface ConnectionFormState {
	label: string;
	sshTarget: string;
	user: string;
	port: string;
	remoteAghubPath: string;
}

/** A blank form, used for the "add" flow. */
export const EMPTY_CONNECTION_FORM: ConnectionFormState = {
	label: "",
	sshTarget: "",
	user: "",
	port: "",
	remoteAghubPath: "",
};

/** Which fields failed validation. i18n keys are resolved by the caller. */
export interface ConnectionFormErrors {
	label?: "connRequiredLabel";
	sshTarget?: "connRequiredSshTarget";
	port?: "connInvalidPort";
}

/**
 * Validate the required fields (label, sshTarget) and the optional port.
 *
 * Pure: returns an errors object. The form is valid iff the returned object
 * has no keys (`isFormValid` is the canonical check).
 */
export function validateConnectionForm(
	form: ConnectionFormState,
): ConnectionFormErrors {
	const errors: ConnectionFormErrors = {};
	if (form.label.trim() === "") {
		errors.label = "connRequiredLabel";
	}
	if (form.sshTarget.trim() === "") {
		errors.sshTarget = "connRequiredSshTarget";
	}
	const port = form.port.trim();
	if (port !== "") {
		const n = Number(port);
		if (!Number.isInteger(n) || n < 1 || n > 65535) {
			errors.port = "connInvalidPort";
		}
	}
	return errors;
}

/** True when the form has no validation errors. */
export function isFormValid(form: ConnectionFormState): boolean {
	return Object.keys(validateConnectionForm(form)).length === 0;
}

/**
 * Build the persisted (id-less) Connection from a validated form. Optional
 * fields are omitted entirely when blank so the JSON sent to Rust matches the
 * `Option<T>` contract (absent rather than empty-string / NaN).
 *
 * The caller is responsible for having validated the form first.
 */
export function formToConnection(
	form: ConnectionFormState,
): Omit<Connection, "id"> {
	const connection: Omit<Connection, "id"> = {
		label: form.label.trim(),
		sshTarget: form.sshTarget.trim(),
	};
	const user = form.user.trim();
	if (user !== "") {
		connection.user = user;
	}
	const port = form.port.trim();
	if (port !== "") {
		connection.port = Number(port);
	}
	const remoteAghubPath = form.remoteAghubPath.trim();
	if (remoteAghubPath !== "") {
		connection.remoteAghubPath = remoteAghubPath;
	}
	return connection;
}

/** Project an existing Connection back into editable form state. */
export function connectionToForm(connection: Connection): ConnectionFormState {
	return {
		label: connection.label,
		sshTarget: connection.sshTarget,
		user: connection.user ?? "",
		port: connection.port === undefined ? "" : String(connection.port),
		remoteAghubPath: connection.remoteAghubPath ?? "",
	};
}
