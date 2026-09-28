//! Running blocking config mutations off the async executor.
//!
//! Acquiring a mutation lock BLOCKS (unbounded process mutex, then up to 10s of
//! `flock` polling). Done on a Rocket worker it parks that worker; enough
//! contended mutations park all of them and even unlocked read routes stop
//! answering (measurements: `crates/api/AGENTS.md` ANTI-PATTERNS).
//!
//! `block_in_place` hands the worker to blocking work and tokio spawns a
//! replacement. Not `spawn_blocking`: several transactions borrow `!Send` /
//! non-`'static` values (`&dyn Fetcher`, `MutationGuard`), so the whole
//! transaction stays inside the closure on one thread.

use crate::error::ApiError;

/// Run one blocking mutation without parking an async worker.
///
/// Use for any handler whose body takes a mutation lock, directly or through
/// `aghub-core`; read-only handlers need nothing. Generic over the whole `Ok`
/// type so it fits `ApiResult<T>`, `ApiCreated<T>` and `ApiNoContent`.
///
/// No `.await` inside `f`: do the git fetch / plugin detection first and pass
/// the results in. A panic in `f` propagates as a 500, deliberately not mapped
/// to an error — a lock error is retryable, a panic is not.
pub async fn in_mutation_pool<R, F>(f: F) -> Result<R, ApiError>
where
	F: FnOnce() -> Result<R, ApiError>,
{
	// `block_in_place` PANICS on a current-thread runtime, which is what the unit
	// tests and `rocket::local::blocking::Client` use. There is nothing to protect
	// there — one thread, no concurrent requests — so run inline instead.
	let multi_thread = matches!(
		rocket::tokio::runtime::Handle::try_current()
			.map(|handle| handle.runtime_flavor()),
		Ok(rocket::tokio::runtime::RuntimeFlavor::MultiThread)
	);
	if multi_thread {
		rocket::tokio::task::block_in_place(f)
	} else {
		f()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rocket::serde::json::Json;

	/// A blocking body must not stop another task on the same runtime from making
	/// progress. One worker thread on purpose: without the hand-off, the sleep
	/// below owns the only worker and the interleaved task cannot run until it
	/// finishes, so the bounded wait fails.
	#[test]
	fn a_blocking_body_does_not_park_the_async_worker() {
		let runtime = rocket::tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_time()
			.build()
			.unwrap();

		runtime.block_on(async {
			let blocked = rocket::tokio::spawn(in_mutation_pool(|| {
				std::thread::sleep(std::time::Duration::from_millis(300));
				Ok(Json(1u8))
			}));
			// Interleaved on the ONE worker while the mutation blocks a pool
			// thread. A bounded wait so a regression fails instead of hanging.
			let progressed = rocket::tokio::time::timeout(
				std::time::Duration::from_secs(5),
				async {
					rocket::tokio::task::yield_now().await;
					2u8
				},
			)
			.await
			.expect("the async worker was parked by the blocking body");
			assert_eq!(progressed, 2);
			// `ApiError` has no `Debug`, so no `unwrap()` on the Err side.
			let Ok(value) = blocked.await.unwrap() else {
				panic!("the mutation body reported an error");
			};
			assert_eq!(*value, 1);
		});
	}

	/// A current-thread runtime must run the body inline rather than panicking:
	/// `block_in_place` is only legal on the multi-threaded flavor, and the unit
	/// tests plus `rocket::local::blocking::Client` are current-thread.
	#[test]
	fn a_current_thread_runtime_runs_the_body_inline() {
		let runtime = rocket::tokio::runtime::Builder::new_current_thread()
			.build()
			.unwrap();
		let Ok(value) = runtime.block_on(in_mutation_pool(|| Ok(Json(7u8))))
		else {
			panic!("the mutation body reported an error");
		};
		assert_eq!(*value, 7);
	}
}
