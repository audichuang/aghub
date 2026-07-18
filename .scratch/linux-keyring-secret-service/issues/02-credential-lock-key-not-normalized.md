# 02 — `credential_lock_for` keys its registry by an un-normalized `PathBuf`

**Status:** open (deferred)

**Where:** `crates/inference/src/store.rs:51`:

```rust
fn credential_lock_for(app_data_dir: &Path) -> Arc<Mutex<()>> {
	static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
		OnceLock::new();
	let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
	let mut map = locks.lock().unwrap_or_else(|e| e.into_inner());
	map.entry(app_data_dir.to_path_buf())
		.or_insert_with(|| Arc::new(Mutex::new(())))
		.clone()
}
```

## Root cause

The registry is keyed by the raw `PathBuf` as handed to
`InferenceProviderStore::new`/`with_credentials`, with no canonicalization
(`std::fs::canonicalize`) and no symlink resolution. Two `InferenceProviderStore`
instances constructed with two different-but-equivalent paths to the SAME
app data directory — a relative path vs. its absolute form, or a path through
a symlink vs. the resolved target — hash to two DIFFERENT `HashMap` entries
and therefore get two DIFFERENT `Arc<Mutex<()>>` locks. That defeats the
whole point of `credential_lock_for` (GitHub #15 P2-5: serializing the
keyring+SQL read-modify-write sequence for `create`/`update`/`delete`/
`set_api_key`/`delete_api_key` against concurrent callers targeting the same
provider) — two stores that both think they're locking "the same" app data
dir but hold different mutexes can still race each other exactly the way the
fix in this branch (see `store.rs`'s `delete`/`set_api_key` doc comments)
was meant to prevent.

## Why this is deferred, not fixed now

The production API (`crates/api/src/routes/inference.rs`'s `store()` helper)
always constructs `InferenceProviderStore` from
`InferenceProviderState::app_data_dir`, which is itself set ONCE at Rocket
boot from a single fixed, already-resolved path (see `crate::state`) — every
request in the real API process reuses that identical `PathBuf` value, so
the registry key is consistent in practice and this gap has no observable
effect on the shipped API or CLI. It is only reachable if some OTHER caller
constructs a store pointed at a relative path, a differently-cased path
(case-insensitive filesystems), or a symlink alias for the same directory —
which nothing in this codebase currently does. Normalizing here is a small
but not-zero-cost change (canonicalize can fail — e.g. path doesn't exist
yet — needing a fallback), and isn't worth bundling into this branch's
scope.

## Suggested fix (when picked up)

Canonicalize `app_data_dir` before using it as the `HashMap` key (falling
back to the as-given path if canonicalization fails, e.g. because the
directory doesn't exist yet — `InferenceProviderStore` creates it lazily).
Add a regression test constructing two stores from two different string
forms of the same real directory (e.g. `foo/../foo/bar` vs `bar`, or a
symlink vs. its target) and asserting `credential_lock_for` returns the
`Arc::ptr_eq` same lock for both.
