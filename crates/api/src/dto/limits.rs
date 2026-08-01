//! Request limits the HTTP surface enforces, exported to TypeScript alongside
//! the DTOs so a client can respect them without hand-copying the number.
//!
//! These are not ts-rs types (a `const` is a value, not a shape), so
//! `bin/export-dto.rs` writes them into `limits.ts` itself and the index writer
//! skips that file.

/// Upper bound on one `POST /skills/apply-updates` body's `names`, and a HARD
/// one — the request is refused, not truncated.
///
/// The mutation seam reads the Lock once and scans the agents once, but neither
/// hoist bounds the real cost: every resolvable row still runs its own
/// install+lock transaction, which re-scans every agent under the mutation lock
/// because that read has to be fresh. One request therefore occupies a mutation
/// worker for `O(names)` transactions, and Rocket has only CPU-count workers.
///
/// A client with more outdated skills than this must send several batches.
pub const MAX_BATCH_NAMES: usize = 256;
