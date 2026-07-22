import assert from "node:assert/strict";
// No FE test runner (vitest/jest) is installed; this pure-logic test uses
// Node's built-in runner (`node --test --experimental-strip-types`), matching
// git-token-forwarding.test.ts. The antfu rule preferring vitest doesn't apply.
// eslint-disable-next-line test/no-import-node-test
import { describe, it } from "node:test";
import { claimCredentialSpeedHint } from "./credential-speed-hint.ts";

function fakeStorage(): Pick<Storage, "getItem" | "setItem"> {
	const map = new Map<string, string>();
	return {
		getItem: (k) => map.get(k) ?? null,
		setItem: (k, v) => void map.set(k, v),
	};
}

const github = (credentialStatus: string) => ({
	sourceType: "github",
	credentialStatus,
});

describe("claimCredentialSpeedHint", () => {
	it("shows once, then throttles for a week", () => {
		const storage = fakeStorage();
		const day = 24 * 60 * 60 * 1000;
		assert.equal(
			claimCredentialSpeedHint([github("missing")], 0, storage),
			true,
		);
		// Same moment and six days later: silent.
		assert.equal(
			claimCredentialSpeedHint([github("missing")], 0, storage),
			false,
		);
		assert.equal(
			claimCredentialSpeedHint([github("missing")], 6 * day, storage),
			false,
		);
		// Past the interval: shows (and re-arms the throttle).
		assert.equal(
			claimCredentialSpeedHint([github("missing")], 8 * day, storage),
			true,
		);
		assert.equal(
			claimCredentialSpeedHint([github("missing")], 8 * day + 1, storage),
			false,
		);
	});

	it("stays silent when every GitHub source already has a credential", () => {
		const storage = fakeStorage();
		assert.equal(
			claimCredentialSpeedHint(
				[github("bound"), github("hostMatch")],
				0,
				storage,
			),
			false,
		);
		// Silence must not consume the throttle: a later uncredentialed
		// source still gets the hint immediately.
		assert.equal(
			claimCredentialSpeedHint([github("notRequired")], 1, storage),
			true,
		);
	});

	it("ignores non-github sources and empty lists", () => {
		const storage = fakeStorage();
		assert.equal(claimCredentialSpeedHint([], 0, storage), false);
		assert.equal(
			claimCredentialSpeedHint(
				[{ sourceType: "local", credentialStatus: "missing" }],
				0,
				storage,
			),
			false,
		);
	});

	it("stays silent when storage throws", () => {
		const throwing: Pick<Storage, "getItem" | "setItem"> = {
			getItem: () => {
				throw new Error("SecurityError");
			},
			setItem: () => {
				throw new Error("SecurityError");
			},
		};
		assert.equal(
			claimCredentialSpeedHint([github("missing")], 0, throwing),
			false,
		);
		// setItem-only failure must also stay silent (cannot throttle → no nag).
		const storage = fakeStorage();
		assert.equal(
			claimCredentialSpeedHint([github("missing")], 0, {
				getItem: storage.getItem,
				setItem: () => {
					throw new Error("QuotaExceededError");
				},
			}),
			false,
		);
	});

	it("treats a corrupt stored timestamp as never shown", () => {
		const storage = fakeStorage();
		storage.setItem("credentialSpeedHintShownAt", "not-a-number");
		assert.equal(
			claimCredentialSpeedHint([github("missing")], 0, storage),
			true,
		);
	});
});
