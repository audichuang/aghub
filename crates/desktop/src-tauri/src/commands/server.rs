use crate::AppState;
use aghub_api::{start, ApiOptions, INFERENCE_PROVIDERS_FILE};
use log::{debug, error, info, warn};
use std::path::{Path, PathBuf};
use tauri::Manager;

pub(crate) fn find_available_port() -> Result<u16, String> {
	let listener = std::net::TcpListener::bind("127.0.0.1:0")
		.map_err(|e| e.to_string())?;
	let port = listener.local_addr().map_err(|e| e.to_string())?.port();
	Ok(port)
}

/// What to tell a user whose inference providers were written into Tauri's
/// identifier-scoped app data dir (`<data>/com.akrc.aghub`) before every
/// surface agreed on [`aghub_api::default_app_data_dir`].
///
/// A message, never a move. aghub USED to copy the db across on startup; that
/// migration is gone on purpose. The shared db has writers this process cannot
/// coordinate with — `aghub-cli inference …`, a standalone `aghub-api`, a
/// second desktop — so any automatic publish has a real interleaving that drops
/// a provider committed by one of them (the desktop's own lock never bound
/// them, and a lock timeout started the API anyway). The split itself is fixed
/// by every surface resolving ONE root; carrying the old bytes over is a
/// convenience, and the convenience was the entire risk.
///
/// So this reads two paths and formats a string. It creates nothing, opens
/// nothing, and leaves both roots byte-identical — including the legacy db,
/// which is never opened, so a schema-only leftover produces the same message a
/// populated one does. There is no marker either: the hint keys on the legacy
/// file EXISTING, so copying it across does not silence it — only renaming the
/// original aside does, and the message says so. Getting that wrong is not
/// cosmetic: a notice that still fires after a successful hand-migration invites
/// the copy a second time, which would restore the pre-upgrade list over
/// everything added since.
fn legacy_inference_db_hint(
	old_root: &Path,
	new_root: &Path,
) -> Option<String> {
	if old_root == new_root {
		return None;
	}
	let legacy = old_root.join(INFERENCE_PROVIDERS_FILE);
	if !legacy.exists() {
		return None;
	}
	Some(format!(
		"inference providers added before this upgrade are in {}, but aghub \
		 now reads {} on every surface, so that list starts out empty. To \
		 bring the old one over by hand: quit every aghub process (this app, \
		 aghub-api, aghub-cli), back up the destination first if it already \
		 exists — it holds anything added since the upgrade, and copying over \
		 it discards that — then copy the file across. Finally RENAME the old \
		 file aside (any name works); this notice keys on its existence, so \
		 until then it repeats every boot and following it a second time would \
		 put the pre-upgrade list back over whatever you have added since. \
		 aghub deliberately does not move it for you.",
		legacy.display(),
		new_root.join(INFERENCE_PROVIDERS_FILE).display()
	))
}

/// The app data root the embedded API opens, plus the legacy root the hint
/// above points at.
///
/// A seam, not decoration: nothing else pins WHICH root reaches `ApiOptions`,
/// and handing it `tauri_dir` — Tauri's identifier-scoped
/// `<data>/com.akrc.aghub` — is exactly the bug this module exists to fix.
/// `tauri_dir` is `None` only when Tauri cannot resolve its own dir, and then
/// there is simply nothing to point at.
fn embedded_api_roots(
	tauri_dir: Option<PathBuf>,
) -> (PathBuf, Option<PathBuf>) {
	(aghub_api::default_app_data_dir(), tauri_dir)
}

#[tauri::command]
pub async fn start_server(
	state: tauri::State<'_, AppState>,
	app: tauri::AppHandle,
) -> Result<u16, String> {
	// Best-effort: a desktop that cannot resolve its own legacy dir must still
	// start, it just has nothing to point the user at.
	let tauri_dir = app
		.path()
		.app_data_dir()
		.inspect_err(|error| {
			warn!(
				"could not resolve the legacy Tauri app data dir, skipping the inference db hint: {error}"
			)
		})
		.ok();
	let (app_data_dir, legacy) = embedded_api_roots(tauri_dir);
	if let Some(hint) = legacy
		.as_deref()
		.and_then(|legacy| legacy_inference_db_hint(legacy, &app_data_dir))
	{
		warn!("{hint}");
	}

	let port = {
		let mut guard = state.port.lock().unwrap();
		if let Some(port) = *guard {
			debug!("reusing embedded API server port {port}");
			return Ok(port);
		}

		let port = find_available_port()?;
		*guard = Some(port);
		debug!("stored embedded API server port {port} in application state");
		port
	};

	info!("received request to start embedded API server on port {port}");
	tokio::spawn(async move {
		info!("starting embedded API server on 127.0.0.1:{port}");
		if let Err(error) = start(ApiOptions {
			port,
			app_data_dir: Some(app_data_dir),
		})
		.await
		{
			error!("embedded API server exited with error: {error}");
		}
	});
	Ok(port)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::BTreeMap;

	/// What a write to `path` would change: identity, size, and last-modified.
	/// Directories included — a staging file created and then removed leaves
	/// the file SET alone while bumping its parent's mtime.
	fn fingerprint(path: &Path) -> String {
		// Inside the body: `MetadataExt` is unix-only and an import gated at
		// module level would be dead on Windows.
		#[cfg(unix)]
		use std::os::unix::fs::MetadataExt;

		let meta = std::fs::symlink_metadata(path).expect("metadata");
		#[cfg(unix)]
		let identity = meta.ino();
		#[cfg(not(unix))]
		let identity = 0u64;
		format!(
			"ino={identity} dir={} len={} mtime={:?}",
			meta.is_dir(),
			meta.len(),
			meta.modified().expect("mtime")
		)
	}

	/// Every entry under `root`, root included. An absent root snapshots as
	/// nothing at all, so "it was never created" and "it never changed" are the
	/// same assertion.
	fn snapshot(root: &Path) -> BTreeMap<PathBuf, String> {
		let mut out = BTreeMap::new();
		let mut stack = vec![root.to_path_buf()];
		while let Some(path) = stack.pop() {
			if std::fs::symlink_metadata(&path).is_err() {
				continue;
			}
			out.insert(path.clone(), fingerprint(&path));
			if path.is_dir() {
				for entry in std::fs::read_dir(&path).expect("read_dir") {
					stack.push(entry.expect("dir entry").path());
				}
			}
		}
		out
	}

	/// The whole point of this module: the embedded API must open the SHARED
	/// root, and Tauri's identifier-scoped dir is only ever the legacy one.
	/// Swap the two and an upgraded desktop goes on reading
	/// `<data>/com.akrc.aghub` while the CLI reads `<data>/aghub` — the
	/// original bug, which nothing else in this crate can see.
	#[test]
	fn embedded_api_opens_the_shared_root_not_the_tauri_dir() {
		let tmp = tempfile::tempdir().expect("tempdir");
		let tauri_dir = tmp.path().join("com.akrc.aghub");

		let (api_root, legacy) = embedded_api_roots(Some(tauri_dir.clone()));

		assert_eq!(
			api_root,
			aghub_api::default_app_data_dir(),
			"the embedded API must open the same root the CLI writes"
		);
		assert_ne!(
			api_root, tauri_dir,
			"Tauri's identifier-scoped dir is the legacy root, never the target"
		);
		assert_eq!(
			legacy,
			Some(tauri_dir),
			"the legacy root the hint reports is Tauri's dir"
		);
	}

	/// Nothing to resolve, nothing to report — and the API root is unchanged.
	#[test]
	fn embedded_api_root_stands_alone_without_a_tauri_dir() {
		let (api_root, legacy) = embedded_api_roots(None);

		assert_eq!(api_root, aghub_api::default_app_data_dir());
		assert_eq!(legacy, None);
	}

	/// The hint names both paths and moves NOTHING. Byte-level, because the
	/// failure this replaces was a migration that copied, renamed and deleted
	/// under a lock that CLI and standalone-API writers never took: any file
	/// the desktop touches in either root at startup is the bug coming back.
	#[test]
	fn legacy_inference_db_is_named_but_never_touched() {
		let tmp = tempfile::tempdir().expect("tempdir");
		let old = tmp.path().join("com.akrc.aghub");
		let new = tmp.path().join("aghub");
		std::fs::create_dir_all(&old).expect("legacy root");
		std::fs::write(
			old.join(INFERENCE_PROVIDERS_FILE),
			"SQLite format 3\0the user's providers",
		)
		.expect("legacy db");
		// tauri-plugin-store writes into that same dir; it was never ours.
		std::fs::write(old.join("store.json"), "{}").expect("store");
		let (before_old, before_new) = (snapshot(&old), snapshot(&new));

		let hint = legacy_inference_db_hint(&old, &new)
			.expect("a legacy db must be reported");

		for named in [
			old.join(INFERENCE_PROVIDERS_FILE),
			new.join(INFERENCE_PROVIDERS_FILE),
		] {
			assert!(
				hint.contains(&named.display().to_string()),
				"the hint must name {} so the manual copy is spellable: {hint}",
				named.display()
			);
		}
		assert_eq!(
			snapshot(&old),
			before_old,
			"the legacy root must be byte-identical — not copied out of, not \
			 renamed, not marked"
		);
		assert_eq!(
			snapshot(&new),
			before_new,
			"and the shared root must not be written to or even created"
		);
		assert!(
			!new.exists(),
			"a hint that creates the shared root has started migrating"
		);
		assert!(
			legacy_inference_db_hint(&tmp.path().join("gone"), &new).is_none(),
			"no legacy db, nothing to say"
		);
	}
}
