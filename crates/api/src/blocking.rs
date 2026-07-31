//! Running blocking skill mutations off the async executor.
//!
//! Every mutating skill flow takes the interprocess mutation lock
//! (`skill::lock::guard`), and acquiring it BLOCKS: it waits on the process mutex
//! (unbounded, by design — bounding it turns ordinary queued work into spurious
//! failures) and then polls `flock` with `thread::sleep` for up to 10s.
//!
//! Called straight from a Rocket handler, that parks an async worker for the whole
//! wait. Rocket's default worker count is the CPU count, so enough contended
//! mutations occupy every worker and the server stops answering **everything** —
//! including read-only routes that take no lock at all. Measured on this repo: an
//! external process holding the global lock plus 25 concurrent delete requests
//! took `GET /api/v1/agents` from 0.00s to a 30s client timeout, while the same 25
//! requests with no contention stayed at 0.00s.
//!
//! `block_in_place` is the mechanism: it hands the current worker over to blocking
//! work and has tokio bring up a replacement, so the remaining routes keep being
//! served. `spawn_blocking` would also work but demands `Send + 'static`, and
//! several of these transactions hold borrowed trait objects (`&dyn Fetcher`) that
//! are neither — reshaping every one of those signatures buys nothing here.
//! `MutationGuard` is `!Send`, so the whole transaction stays inside the closure
//! either way; it is created and dropped on one thread and never crosses a
//! boundary.

use crate::error::ApiError;

/// Run one blocking skill mutation without parking an async worker.
///
/// Use this for any handler whose body takes the skill mutation lock, directly or
/// through `aghub-core`. A handler that only READS needs nothing: read paths are
/// deliberately unlocked.
///
/// Generic over the whole `Ok` type rather than over `ApiResult<T>`'s payload, so
/// it fits `ApiResult<T>`, `ApiCreated<T>` and `ApiNoContent` alike.
///
/// A `.await` cannot live inside `f`; split the awaits (a git fetch, plugin
/// detection) out of the locked transaction and pass their results in.
///
/// A panic inside `f` propagates to Rocket exactly as it would from any handler
/// body (answered as a 500) — it is deliberately NOT converted into an error here,
/// because a lock error is retryable and a panic is not.
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
