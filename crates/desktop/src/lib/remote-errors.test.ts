import assert from "node:assert/strict";
import { test } from "node:test";
import {
	asRemotePayload,
	remoteErrorMessage,
	remoteOutputSummary,
	sshConnectionErrorMessage,
} from "./remote-errors.ts";

test("incompatible payload preserves the version for the connection gate and message", () => {
	const payload = { kind: "incompatible", remoteVersion: "2.39.1" };
	assert.equal(
		asRemotePayload(payload)?.remoteVersion ?? "unknown",
		"2.39.1",
	);
	assert.equal(
		remoteErrorMessage(payload),
		"Remote aghub-api version 2.39.1 is incompatible.",
	);
});

test("incompatible payload with a null version displays unknown", () => {
	const payload = { kind: "incompatible", remoteVersion: null };
	assert.equal(asRemotePayload(payload)?.remoteVersion, null);
	assert.equal(
		remoteErrorMessage(payload),
		"Remote aghub-api version unknown is incompatible.",
	);
});

test("remoteApiMissing payload preserves the installation hint", () => {
	const payload = {
		kind: "remoteApiMissing",
		installHint: "Install aghub-api 2.39.1 on the remote.",
	};
	assert.equal(remoteErrorMessage(payload), payload.installHint);
});

test("crossPlatformRedeploy payload preserves the platform and deployment hint", () => {
	const payload = {
		kind: "crossPlatformRedeploy",
		remotePlatform: "macos/aarch64",
		hint: "Install the matching remote binary manually.",
	};
	assert.equal(asRemotePayload(payload)?.remotePlatform, "macos/aarch64");
	assert.equal(remoteErrorMessage(payload), payload.hint);
	assert.equal(
		remoteErrorMessage({
			kind: payload.kind,
			remotePlatform: payload.remotePlatform,
		}),
		"Remote platform macos/aarch64 differs from this desktop; cannot redeploy.",
	);
});

test("remoteErrorMessage: remoteDirectoryFailed with message", () => {
	assert.equal(
		remoteErrorMessage({
			kind: "remoteDirectoryFailed",
			message: "not a directory",
		}),
		"not a directory",
	);
});

test("remoteErrorMessage: remoteDirectoryFailed without message falls back", () => {
	assert.equal(
		remoteErrorMessage({ kind: "remoteDirectoryFailed" }),
		"Remote directory browsing failed.",
	);
});

test("remoteOutputSummary: strips ANSI and returns last non-empty line", () => {
	assert.equal(remoteOutputSummary("\x1B[31mfirst\x1B[0m\n\nlast\n"), "last");
});

test("remoteOutputSummary: returns last non-empty line from plain multiline string", () => {
	assert.equal(
		remoteOutputSummary("Host key failed\nBatchMode is set"),
		"BatchMode is set",
	);
});

const LOCAL_NETWORK_HINT =
	"If SSH works in Terminal, check aghub's Local Network access in System Settings.";

for (const reason of [
	"No route to host",
	"Network is unreachable",
	"Operation not permitted",
]) {
	test(`macOS SSH ${reason} preserves the error and adds recovery guidance`, () => {
		const stderr = `ssh: connect to host 192.168.31.65 port 22: ${reason}`;
		assert.equal(
			sshConnectionErrorMessage(stderr, true, LOCAL_NETWORK_HINT),
			`${stderr}\n\n${LOCAL_NETWORK_HINT}`,
		);
	});
}

test("macOS recovery guidance preserves multiline SSH stderr", () => {
	const stderr =
		"Warning: previous connection failed\nssh: connect to host ubuntu port 2222: No route to host\n";
	assert.equal(
		sshConnectionErrorMessage(stderr, true, LOCAL_NETWORK_HINT),
		`${stderr}\n\n${LOCAL_NETWORK_HINT}`,
	);
});

test("SSH failures on other platforms do not receive macOS instructions", () => {
	const stderr =
		"ssh: connect to host 192.168.31.65 port 22: No route to host";
	assert.equal(
		sshConnectionErrorMessage(stderr, false, LOCAL_NETWORK_HINT),
		stderr,
	);
});

test("authentication, DNS, timeout and remote command errors stay unchanged", () => {
	for (const stderr of [
		"audichuang@ubuntu: Permission denied (publickey).",
		"ssh: Could not resolve hostname ubuntu: nodename nor servname provided, or not known",
		"ssh: connect to host ubuntu port 22: Connection timed out",
		"curl: No route to host",
		"No route to host",
	]) {
		assert.equal(
			sshConnectionErrorMessage(stderr, true, LOCAL_NETWORK_HINT),
			stderr,
		);
	}
});

// ---------------------------------------------------------------------------
// Exhaustive coverage: every known Rust RemoteError kind must produce a
// friendly (non-JSON) string. If a new kind is added on the Rust side without
// a matching case here, this test will catch the regression by asserting that
// the returned string is NOT the raw JSON.stringify of the payload.
// ---------------------------------------------------------------------------

const KNOWN_KINDS = [
	"unreachable",
	"remoteApiMissing",
	"incompatible",
	"crossPlatformRedeploy",
	"startTimeout",
	"tunnelFailed",
	"deployFailed",
	"remoteDirectoryFailed",
	"alreadyConnecting",
	"internal",
] as const;

for (const kind of KNOWN_KINDS) {
	test(`remoteErrorMessage: ${kind} is handled (not raw JSON)`, () => {
		// Minimal payload — only kind set so the default arm cannot
		// accidentally succeed via message/stderr/hint.
		const payload = { kind };
		const result = remoteErrorMessage(payload);
		// Must not stringify the bare payload (that is the default-arm
		// fallback, meaning the kind hit the default case).
		assert.notEqual(
			result,
			JSON.stringify(payload),
			`kind "${kind}" hit the default/stringify fallback — add an explicit case`,
		);
		// Must be a non-empty string.
		assert.ok(result.length > 0, `kind "${kind}" returned empty string`);
	});
}

test("remoteErrorMessage: default arm prefers message over stringify", () => {
	// An unknown kind with a message field should return the message,
	// not the JSON serialisation.
	const payload = {
		kind: "__unknown_future_kind__",
		message: "human readable",
	};
	assert.equal(remoteErrorMessage(payload), "human readable");
});

test("remoteErrorMessage: default arm prefers stderr when message absent", () => {
	const payload = { kind: "__unknown__", stderr: "stderr line" };
	assert.equal(remoteErrorMessage(payload), "stderr line");
});

test("remoteErrorMessage: default arm prefers hint when message+stderr absent", () => {
	const payload = { kind: "__unknown__", hint: "install hint" };
	assert.equal(remoteErrorMessage(payload), "install hint");
});

test("remoteErrorMessage: default arm falls back to JSON.stringify when no human fields", () => {
	const payload = { kind: "__unknown__" };
	assert.equal(remoteErrorMessage(payload), JSON.stringify(payload));
});
