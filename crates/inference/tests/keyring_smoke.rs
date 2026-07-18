//! Real Secret Service smoke test (GitHub #15 P1-4).
//!
//! `NativeCredentialStore`'s Linux backend is the pure-Rust zbus/async-io
//! secret-service backend (`async-secret-service` + `crypto-rust` +
//! `async-io` — see this crate's `Cargo.toml` and `crates/api/Cargo.toml`,
//! which carries the full reasoning). It was chosen specifically because it
//! must never touch tokio's runtime machinery, so it cannot panic with
//! "Cannot start a runtime from within a runtime" no matter which thread it
//! runs on — which matters because `aghub-api` drives it from exactly that
//! kind of thread (a Rocket handler running inline inside the route's async
//! future, or explicitly via `tokio::task::spawn_blocking`).
//!
//! Every OTHER credential-store test in this workspace either mocks the
//! store entirely (deterministic, no real keyring involved) or runs under
//! the process-global `keyring::mock` builder installed by `aghub-api`'s
//! `IsolatedApiTest`. This is the ONLY test that drives the REAL native
//! backend end-to-end, so it needs an actual Secret Service session
//! (gnome-keyring under `dbus-run-session` — see
//! `.github/workflows/ci.yml`'s dedicated smoke step) and is therefore
//! `#[ignore]`d by default: a normal `cargo test` run, with no keyring
//! reachable, must never depend on it.

#![cfg(target_os = "linux")]

use aghub_inference::{
	CredentialStore, InferenceProviderError, NativeCredentialStore,
};

/// Runs a full set → get → delete → get round trip inside
/// `tokio::task::spawn_blocking`, called from a Tokio runtime — the exact
/// call shape `crate::error::run_blocking` uses in `aghub-api`'s real route
/// handlers (see `crates/api/src/error.rs`). If the zbus/async-io backend
/// ever regressed to touching tokio's runtime machinery (the hazard the
/// `async-io` feature choice specifically avoids), that would surface here
/// as a panicked blocking task; `spawn_blocking`'s `JoinError` is asserted
/// on directly rather than swallowed, so a nested-runtime regression fails
/// this test loudly instead of silently vanishing.
#[test]
#[ignore = "requires a real Secret Service session (gnome-keyring); run \
            under dbus-run-session — see .github/workflows/ci.yml"]
fn native_store_round_trips_under_spawn_blocking() {
	let runtime = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("failed to build tokio runtime");

	runtime.block_on(async {
		// A unique-per-run id keeps repeated CI runs (or a stray leftover
		// entry from a prior interrupted run) from colliding.
		let provider_id = format!(
			"aghub-ci-smoke-{}-{}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		);

		let result = tokio::task::spawn_blocking(move || {
			let store = NativeCredentialStore;

			// Precondition: no leftover entry from an earlier run.
			let before = store.get_api_key(&provider_id)?;
			assert!(
				before.is_none(),
				"precondition: no stale entry for this run's unique id"
			);

			store.set_api_key(&provider_id, "smoke-test-secret")?;
			let read_back = store.get_api_key(&provider_id)?;
			assert_eq!(
				read_back.as_deref(),
				Some("smoke-test-secret"),
				"a key just written must read back identically"
			);

			store.delete_api_key(&provider_id)?;
			let after_delete = store.get_api_key(&provider_id)?;
			assert_eq!(after_delete, None, "the key must be gone after delete");

			Ok::<(), InferenceProviderError>(())
		})
		.await;

		match result {
			Ok(Ok(())) => {}
			Ok(Err(error)) => panic!(
				"real Secret Service round trip failed: {error} -- is \
				 gnome-keyring unlocked under dbus-run-session? see \
				 .github/workflows/ci.yml"
			),
			Err(join_error) => panic!(
				"spawn_blocking task panicked (is_panic={}, is_cancelled={}) \
				 -- a nested-runtime panic here would mean the async-io \
				 secret-service backend regressed to touching tokio's \
				 runtime machinery: {join_error}",
				join_error.is_panic(),
				join_error.is_cancelled()
			),
		}
	});
}

/// Missing entries must report as `Ok(None)` (`keyring::Error::NoEntry`),
/// never an error — the baseline "no credential" outcome every caller
/// (`InferenceProviderStore::get_api_key`, the cascade's reachability
/// precondition, ...) depends on to distinguish "backend unreachable" from
/// "there just isn't a key yet".
#[test]
#[ignore = "requires a real Secret Service session (gnome-keyring); run \
            under dbus-run-session — see .github/workflows/ci.yml"]
fn native_store_missing_entry_is_ok_none() {
	let runtime = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("failed to build tokio runtime");

	runtime.block_on(async {
		let provider_id = format!(
			"aghub-ci-smoke-missing-{}-{}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		);

		let result = tokio::task::spawn_blocking(move || {
			NativeCredentialStore.get_api_key(&provider_id)
		})
		.await
		.expect("spawn_blocking must not panic for a plain missing-entry read");

		assert_eq!(
			result.unwrap(),
			None,
			"a never-written provider id must read back as Ok(None), not an error"
		);
	});
}
