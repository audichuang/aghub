//! Round trip through the REAL native keyring (keychain, Credential Manager,
//! Secret Service), from a `spawn_blocking` thread inside a tokio runtime —
//! where aghub-api calls it. Every other keyring test uses a mock store.
//! Ignored by default; CI runs it on all three platforms.

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
#[ignore = "touches the real OS keyring"]
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
			Ok(Err(error)) => {
				panic!("native keyring round trip failed: {error}")
			}
			Err(join_error) => panic!(
				"keyring call panicked inside the tokio runtime: {join_error}"
			),
		}
	});
}

/// Missing entries must report as `Ok(None)` (`keyring_core::Error::NoEntry`),
/// never an error — the baseline "no credential" outcome every caller
/// (`InferenceProviderStore::get_api_key`, the cascade's reachability
/// precondition, ...) depends on to distinguish "backend unreachable" from
/// "there just isn't a key yet".
#[test]
#[ignore = "touches the real OS keyring"]
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
