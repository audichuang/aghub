# Auto-deploy on connect: bundled aghub-api + connect-path upgrade Implementation Plan

> For agentic workers: execute this plan with `superpowers:subagent-driven-development` — one subagent per task, in the listed order. The unit-testable Rust changes (Gap B, then the Gap A precedence helper) land first via real red→green TDD; the config / CI / distribution pieces (validated only by JSON/YAML validity, a local Tauri build, and a tag push) land last. Each task is independently committable.

**Goal:** Make a shipped Linux desktop app, on connect to a same-platform Ubuntu VM running an older/incompatible `aghub-api`, silently upgrade the VM to the version-locked bundled binary and connect — no SSH, no button — and ship that bundled binary via release CI and a local `just` recipe.

**Architecture:** The desktop bundles a version-locked `aghub-api` as a Tauri `bundle.resources` resource, injected only at build time via a committed `--config` overlay (never in the committed `tauri.conf.json`). At runtime `remote_install_source()` resolves that resource first (bundled → env → cargo-git), giving a packaged build a `LocalBinary` source. The tauri-free `aghub-remote::bringup::ensure_remote_api` is restructured so "present" no longer short-circuits: a present-but-incompatible binary upgrades when a source exists and the platform matches.

**Tech Stack:** Rust (workspace, edition 2021, hard tabs width 4, 80-col, `cargo clippy -- -D warnings`); Tauri v2.11 core (`tauri::path::BaseDirectory` — no fs/shell plugin); React 19 / TypeScript / bun (desktop frontend, untouched here); GitHub Actions (release CI); `just` task runner.

---

## File Structure

| File                                              | Create / Modify                                                                                                                                               | Single responsibility                                                                                                                                                                                   |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `crates/remote/src/bringup.rs`                    | Modify (`ensure_remote_api` ~287-345; inline `#[cfg(test)] mod tests`)                                                                                        | Gap B: restructure so present-but-incompatible upgrades when a source + platform allow; add 4 new `MockRunner` branch tests.                                                                            |
| `crates/desktop/src-tauri/src/commands/remote.rs` | Modify (imports ~41; `remote_install_source` ~570; `bring_up` ~433; `force_redeploy_remote` ~282; `remote_install_source_available` ~675; inline `mod tests`) | Gap A: thread `&AppHandle`; add `bundled_api_path(app)` + pure `pick_install_source` precedence helper; add `app: AppHandle` to `remote_install_source_available`; import `tauri::path::BaseDirectory`. |
| `crates/desktop/src-tauri/tauri.bundle.conf.json` | Create                                                                                                                                                        | Committed `--config` overlay: `bundle.resources = ["binaries/aghub-api*"]`. Applied only by CI / the `just` recipe; never merged into the committed main config.                                        |
| `.gitignore`                                      | Modify                                                                                                                                                        | Ignore `crates/desktop/src-tauri/binaries/`.                                                                                                                                                            |
| `.prettierignore`                                 | Modify                                                                                                                                                        | Ignore `crates/desktop/src-tauri/binaries/`.                                                                                                                                                            |
| `justfile`                                        | Modify (add `desktop-bundle`)                                                                                                                                 | Local one-command path: detect host triple, clean+stage `src-tauri/binaries/aghub-api[.exe]`, run the desktop bundle build with the overlay.                                                            |
| `.github/workflows/release.yml`                   | Modify (`build-tauri` job: `Sync Version` ~175-193; new step before `Build Tauri` ~195; tauri-action `args` ~217)                                             | Gap C / CI: post-sync version assertion + drop `                                                                                                                                                        |     | true`on the`tauri.conf.json`sed only; version-locked per-target sidecar stage; host-arch-aware smoke; pass`--config src-tauri/tauri.bundle.conf.json`. |

Facts grounding this plan (verified against the source on 2026-06-21):

- Root workspace `Cargo.toml` `version = "1.1.1"`; `crates/desktop/src-tauri/tauri.conf.json` `"version": "1.2.1"` (a `version` line that the `sed` pattern DOES match); `crates/desktop/package.json` has **no** `"version"` field.
- Host triple: `rustc -vV` prints `host: x86_64-unknown-linux-gnu`.
- The desktop bin crate package name is `aghub` (`crates/desktop/src-tauri/Cargo.toml`), so its tests run with `cargo test -p aghub`. The lib name is `aghub_desktop_lib`.
- `aghub-remote` has no `tests/` dir; unit tests are the inline `#[cfg(test)] mod tests` in `bringup.rs`.
- `MockRunner` (`crates/remote/src/test_support.rs`) is `pub(crate)`, keys scripts on the exact `(program, args)` tuple via `HashMap::insert` (**last-write-wins, same output for every call to a key — no per-call-index sequencing**), and records every call.
- `remote_install_source_available` is registered in `crates/desktop/src-tauri/src/lib.rs:325` `generate_handler!`; a command taking an injected `AppHandle` needs no frontend payload change.
- `force_redeploy_remote` takes `app: AppHandle` by value and later calls `force_redeploy(&app, ...)` — so `remote_install_source` MUST be called with `&app` (borrow), never `app` (move).
- The remote finish step (`crates/remote/src/ssh.rs:375-382`) does `mv → chmod 755 → --version`; the local executable bit is NOT preserved or handled in the desktop layer.
- justfile recipe bodies use **4-space** indentation (e.g. `build`, `preflight`, `bump`); `set windows-shell := ["cmd.exe", "/c"]` is declared at the top.

> ## MockRunner seam (read before Task 1) — load-bearing
>
> `ensure_remote_api` builds **identical** ssh argv for its first probe and its post-install second probe (`resolved_path` is the same `"aghub-api"`). `MockRunner` keys on `(program, args)` and returns the **same** scripted output for every call to a key. Therefore a single `MockRunner` **cannot** return "incompatible then compatible" for that one probe key. The correct, buildable Gap-B upgrade test scripts the probe key as **incompatible-only** (single `.script` registration), so:
>
> 1. the first probe is present + `!compatible` → does NOT short-circuit,
> 2. the same-platform gate passes,
> 3. `install_remote_api` runs (prepare + scp + finish recorded),
> 4. the second probe replays the same key → still `1.0.0` but `api_present == true` → returns `Ok(second)` with `install_attempted == true`.
>
> The test asserts `install_attempted` + that an `scp` and a `finish` call were recorded. It does **NOT** assert `result.compatible == true` (unachievable with this seam). The version-flip-to-compatible contract is already covered by the pre-existing `force_redeploy_stages_then_finishes_then_probes` (its probe runs once, so a distinct compatible script works there).

---

### Task 1: Gap B — restructure `ensure_remote_api` to upgrade present-but-incompatible (TDD)

**Files:**

- Modify: `crates/remote/src/bringup.rs:287-345` (`ensure_remote_api`)
- Test: `crates/remote/src/bringup.rs` (inline `#[cfg(test)] mod tests`, append four tests after line 906)

Six branches must hold. Three are already tested and MUST keep passing unchanged: `ensure_remote_api_present_compatible_returns_ok_without_install` (line 851), `ensure_remote_api_absent_and_no_source_is_remote_api_missing` (line 883), `ensure_remote_api_local_binary_cross_platform_refuses_before_scp` (line 798, absent+127+Windows uname → `CrossPlatformDeploy`). We add four new branch tests.

- [ ] **Step 1: Add the four new failing branch tests.**

    Insert these into the existing `#[cfg(test)] mod tests` in `crates/remote/src/bringup.rs`, immediately after `ensure_remote_api_absent_and_no_source_is_remote_api_missing` (after line 906). They reuse the existing `conn()`, `probe_args()`, `args_as_str()` helpers and `const LOCAL: &str = "1.1.1";`. Add the `local_uname_stdout` helper and `local_source` fixture once (place them next to `conn()` inside the module):

    ```rust
    	/// `uname -sm` stdout mapping to THIS host's (os, arch) so the
    	/// same-platform gate passes wherever the test runs. Mirrors the mapping
    	/// in `probe_remote_platform` (Linux/Darwin + x86_64/arm64/aarch64).
    	fn local_uname_stdout() -> String {
    		let os = match std::env::consts::OS {
    			"linux" => "Linux",
    			"macos" => "Darwin",
    			other => other,
    		};
    		let arch = std::env::consts::ARCH;
    		format!("{os} {arch}\n")
    	}

    	fn local_source() -> RemoteInstallSource {
    		RemoteInstallSource::LocalBinary("/tmp/aghub-api".into())
    	}
    ```

    Then the four tests:

    ```rust
    	// Same-platform gate: the remote `uname -sm` must normalize to THIS
    	// host's (os, arch). `normalize_platform` (ssh.rs) only knows
    	// Linux/Darwin, so the Windows release-CI runner cannot satisfy the gate
    	// — cfg-gate this same-platform test off Windows. (Real Windows remote
    	// deploy is out of scope; the cross-platform test below still runs
    	// everywhere.)
    	#[cfg(any(target_os = "linux", target_os = "macos"))]
    	#[test]
    	fn ensure_present_incompatible_local_binary_same_platform_upgrades() {
    		// Present-but-INCOMPATIBLE remote binary + LocalBinary source +
    		// matching platform: ensure_remote_api must UPGRADE (uname gate ->
    		// stage(prepare+scp) -> finish(mv+chmod)) and re-probe.
    		//
    		// MockRunner keys on (program, args) and the first/second probe build
    		// IDENTICAL argv, so we script the probe key INCOMPATIBLE-ONLY: the
    		// first probe is present + !compatible (no short-circuit) and the
    		// re-probe replays the same incompatible output but api_present=true,
    		// so ensure_remote_api returns Ok(second) with install_attempted=true.
    		// We assert the side-effects (an scp + a finish ran, install_attempted)
    		// — NOT result.compatible, which this single-key seam cannot flip.
    		let probe = probe_args();
    		let uname_args = build_ssh_args(&conn(), "uname -sm");
    		let prepare_args =
    			build_ssh_args(&conn(), &build_remote_prepare_upload_cmd());
    		let scp_args = build_scp_args(
    			&conn(),
    			"/tmp/aghub-api",
    			crate::ssh::remote_api_upload_path(),
    		);
    		let finish_args = build_ssh_args(
    			&conn(),
    			&build_remote_finish_upload_cmd("aghub-api"),
    		);
    		let incompatible = || CommandOutput {
    			status_code: Some(0),
    			stdout: "aghub-api 1.0.0".to_string(),
    			stderr: String::new(),
    		};
    		let ok = || CommandOutput {
    			status_code: Some(0),
    			stdout: String::new(),
    			stderr: String::new(),
    		};
    		let runner = MockRunner::new()
    			.script("ssh", &args_as_str(&probe), incompatible())
    			.script("ssh", &args_as_str(&uname_args), CommandOutput {
    				status_code: Some(0),
    				stdout: local_uname_stdout(),
    				stderr: String::new(),
    			})
    			.script("ssh", &args_as_str(&prepare_args), ok())
    			.script("scp", &args_as_str(&scp_args), ok())
    			.script("ssh", &args_as_str(&finish_args), ok());

    		let result =
    			ensure_remote_api(&runner, &conn(), LOCAL, Some(&local_source()))
    				.expect("same-platform upgrade returns Ok(second)");
    		assert!(result.api_present, "re-probe still finds the binary present");
    		assert!(
    			result.install_attempted,
    			"the upgrade install path must have run"
    		);

    		let calls = runner.calls();
    		assert!(
    			calls.iter().any(|c| c.program == "scp" && c.args == scp_args),
    			"the bundled binary must be uploaded on upgrade: {calls:?}"
    		);
    		assert!(
    			calls.iter().any(|c| c.args == finish_args),
    			"the staged upload must be moved into place: {calls:?}"
    		);
    	}

    	#[test]
    	fn ensure_present_incompatible_no_source_returns_ok_first() {
    		// Present-but-incompatible + NO source: unchanged behaviour — return
    		// Ok(first) so the caller surfaces the Incompatible screen. No
    		// platform probe, no scp, only the single probe runs.
    		let probe = probe_args();
    		let runner = MockRunner::new().script(
    			"ssh",
    			&args_as_str(&probe),
    			CommandOutput {
    				status_code: Some(0),
    				stdout: "aghub-api 1.0.0".to_string(),
    				stderr: String::new(),
    			},
    		);
    		let result = ensure_remote_api(&runner, &conn(), LOCAL, None)
    			.expect("present-but-incompatible + no source returns Ok(first)");
    		assert!(result.api_present);
    		assert!(!result.compatible);
    		assert!(!result.install_attempted, "no install with no source");

    		let calls = runner.calls();
    		assert_eq!(calls.len(), 1, "only the probe should run: {calls:?}");
    		assert!(!calls.iter().any(|c| c.program == "scp"));
    	}

    	#[test]
    	fn ensure_present_incompatible_local_binary_cross_platform_returns_ok_first()
    	{
    		// Present-but-incompatible + LocalBinary source + CROSS-platform
    		// remote: cannot deploy the wrong-arch binary, so return Ok(first)
    		// (Incompatible screen) WITHOUT scp. Distinct from the absent case,
    		// which returns CrossPlatformDeploy.
    		let probe = probe_args();
    		let uname_args = build_ssh_args(&conn(), "uname -sm");
    		let runner = MockRunner::new()
    			.script("ssh", &args_as_str(&probe), CommandOutput {
    				status_code: Some(0),
    				stdout: "aghub-api 1.0.0".to_string(),
    				stderr: String::new(),
    			})
    			.script("ssh", &args_as_str(&uname_args), CommandOutput {
    				status_code: Some(0),
    				// Windows_NT does not map to the consts vocabulary -> None,
    				// treated as not-the-same-platform.
    				stdout: "Windows_NT x86_64\n".to_string(),
    				stderr: String::new(),
    			});
    		let result =
    			ensure_remote_api(&runner, &conn(), LOCAL, Some(&local_source()))
    				.expect("cross-platform incompatible returns Ok(first)");
    		assert!(result.api_present);
    		assert!(!result.compatible);
    		assert!(!result.install_attempted, "no install across platforms");

    		// Assert the platform probe actually RAN before refusing — this is
    		// what makes the test red against the OLD short-circuit (which
    		// returned Ok(first) without ever probing uname). Without it, the
    		// test passes even if the present-short-circuit is never removed.
    		let calls = runner.calls();
    		assert!(
    			calls
    				.iter()
    				.any(|c| c.program == "ssh" && c.args == uname_args),
    			"the platform probe must run before refusing: {calls:?}"
    		);
    		assert!(
    			!calls.iter().any(|c| c.program == "scp"),
    			"no scp on a cross-platform incompatible binary: {calls:?}"
    		);
    	}

    	// Same-platform gate (see the upgrade test above) — cfg-gate off
    	// Windows, whose uname vocabulary `normalize_platform` does not map.
    	#[cfg(any(target_os = "linux", target_os = "macos"))]
    	#[test]
    	fn ensure_absent_same_platform_local_binary_runs_install_then_fails() {
    		// Absent (command not found) + LocalBinary + matching platform: the
    		// same-platform gate passes and install runs (prepare+scp+finish).
    		// MockRunner replays the same probe key, so the SECOND probe is still
    		// 127 -> api_present=false -> ensure_remote_api returns DeployFailed.
    		// We assert that error AND that the install steps ran first (the
    		// single-key seam cannot make the re-probe "present"; the success path
    		// is exercised by ..._same_platform_upgrades above).
    		let probe = probe_args();
    		let uname_args = build_ssh_args(&conn(), "uname -sm");
    		let prepare_args =
    			build_ssh_args(&conn(), &build_remote_prepare_upload_cmd());
    		let scp_args = build_scp_args(
    			&conn(),
    			"/tmp/aghub-api",
    			crate::ssh::remote_api_upload_path(),
    		);
    		let finish_args = build_ssh_args(
    			&conn(),
    			&build_remote_finish_upload_cmd("aghub-api"),
    		);
    		let ok = || CommandOutput {
    			status_code: Some(0),
    			stdout: String::new(),
    			stderr: String::new(),
    		};
    		let runner = MockRunner::new()
    			.script("ssh", &args_as_str(&probe), CommandOutput {
    				status_code: Some(127),
    				stdout: String::new(),
    				stderr: "bash: aghub-api: command not found".to_string(),
    			})
    			.script("ssh", &args_as_str(&uname_args), CommandOutput {
    				status_code: Some(0),
    				stdout: local_uname_stdout(),
    				stderr: String::new(),
    			})
    			.script("ssh", &args_as_str(&prepare_args), ok())
    			.script("scp", &args_as_str(&scp_args), ok())
    			.script("ssh", &args_as_str(&finish_args), ok());

    		let err =
    			ensure_remote_api(&runner, &conn(), LOCAL, Some(&local_source()))
    				.expect_err("re-probe still absent -> DeployFailed");
    		assert!(matches!(err, ConnectError::DeployFailed(_)), "got {err:?}");

    		// The install ran before the failing re-probe.
    		let calls = runner.calls();
    		assert!(
    			calls.iter().any(|c| c.program == "scp" && c.args == scp_args),
    			"absent same-platform must scp the binary: {calls:?}"
    		);
    		assert!(
    			calls.iter().any(|c| c.args == finish_args),
    			"absent same-platform must run finish: {calls:?}"
    		);
    	}
    ```

- [ ] **Step 2: Run the new tests — expect FAIL (red).**

    Command:

    ```bash
    cargo test -p aghub-remote ensure_ -- --nocapture
    ```

    Expected against the CURRENT code (`if first.api_present { return Ok(first); }`).
    TWO tests are genuinely red; the other two pin unchanged branches:
    - `ensure_present_incompatible_local_binary_same_platform_upgrades` — **FAIL** (the load-bearing red): the present probe short-circuits with `Ok(first)` before any gate/install, so `install_attempted` is false and no `scp` is recorded. (cfg-gated to Linux/macOS — where you develop — so it runs locally; it does not run on the Windows CI leg.)
    - `ensure_present_incompatible_local_binary_cross_platform_returns_ok_first` — **FAIL** too: it now asserts the `uname` probe RAN, but the current short-circuit returns `Ok(first)` without ever probing — so that assertion fails. Becomes a real regression guard post-impl. (Runs on all platforms.)
    - `ensure_present_incompatible_no_source_returns_ok_first` — passes already (present → `Ok(first)`, one call); pins the unchanged no-source branch.
    - `ensure_absent_same_platform_local_binary_runs_install_then_fails` — passes under current code (absent already installs via the existing gate); a regression guard for the absent+same-platform path. (cfg-gated to Linux/macOS.)

    Note on counting: on a Linux/macOS dev box all four run (two red, two green); on the Windows CI leg the two cfg-gated same-platform tests are compiled out, leaving the cross-platform + no-source tests.

- [ ] **Step 3: Implement the restructured `ensure_remote_api` (minimal impl).**

    Replace the body of `ensure_remote_api` (lines 293-345, from `let first = ...` through the final `Ok(second)`) with:

    ```rust
    	let first = probe_connection(runner, conn, local_version);
    	if !first.reachable {
    		return Err(ConnectError::Unreachable {
    			stderr: first.message,
    		});
    	}
    	// Present AND compatible -> nothing to do (unchanged fast path).
    	if first.api_present && first.compatible {
    		return Ok(first);
    	}

    	// Absent, or present-but-incompatible: try to install/upgrade when a
    	// source exists. No source -> unchanged behaviour:
    	//   absent  => RemoteApiMissing (UI shows the manual install hint),
    	//   present => Ok(first) so the caller surfaces the Incompatible screen.
    	let Some(source) = source else {
    		return if first.api_present {
    			Ok(first)
    		} else {
    			Err(ConnectError::RemoteApiMissing {
    				install_hint: install_hint(),
    			})
    		};
    	};

    	// Same-platform gate for a LocalBinary source — covers BOTH the absent
    	// and the upgrade path (a wrong-arch binary would never run). CargoGit
    	// compiles on the VM, so it is un-gated for any remote platform.
    	if let RemoteInstallSource::LocalBinary(_) = source {
    		let local = (std::env::consts::OS, std::env::consts::ARCH);
    		let remote = probe_remote_platform(runner, conn);
    		let same = remote
    			.as_ref()
    			.map(|(os, arch)| os == local.0 && arch == local.1)
    			.unwrap_or(false);
    		if !same {
    			let remote_platform = remote
    				.map(|(os, arch)| format!("{os}/{arch}"))
    				.unwrap_or_else(|| "unknown".to_string());
    			// Cross-platform: cannot deploy the bundled binary.
    			//   present-but-incompatible => Ok(first) (Incompatible screen),
    			//   absent                    => CrossPlatformDeploy.
    			return if first.api_present {
    				Ok(first)
    			} else {
    				Err(ConnectError::CrossPlatformDeploy { remote_platform })
    			};
    		}
    	}

    	let bin = resolved_path(conn);
    	// `install_remote_api` does stage -> finish (mv + chmod 755), so an
    	// upgrade overwrites an old binary cleanly in place.
    	install_remote_api(runner, conn, &bin, source)?;

    	let second = probe_connection(runner, conn, local_version)
    		.with_install_result(true, "aghub-api installed/upgraded".to_string());
    	if !second.reachable {
    		return Err(ConnectError::Unreachable {
    			stderr: second.message,
    		});
    	}
    	if !second.api_present {
    		return Err(ConnectError::DeployFailed(format!(
    			"Automatic install ran, but aghub-api is still unavailable: {}",
    			second.message
    		)));
    	}
    	Ok(second)
    ```

    Also replace the doc comment above the function (lines 281-286) with:

    ```rust
    /// Ensure the remote has a COMPATIBLE `aghub-api`.
    ///
    /// Probes first. Returns early only when a compatible binary is already
    /// present. Otherwise — absent, OR present but version-incompatible — it
    /// installs/upgrades over ssh/scp when a source is available (a `LocalBinary`
    /// source is same-platform-gated on BOTH paths; `CargoGit` compiles on the VM
    /// and is un-gated), then re-probes. With no source (or a cross-platform
    /// `LocalBinary`), a present-but-incompatible binary returns the probe so the
    /// caller surfaces the Incompatible screen; an absent binary errors. The final
    /// [`TestResult`] is returned so callers can still reject incompatible
    /// versions.
    ```

    No other functions change; `install_remote_api`, `probe_remote_platform`, and the imports (lines 15-23) are already present.

- [ ] **Step 4: Run the new + pre-existing tests — expect PASS (green).**

    Command:

    ```bash
    cargo test -p aghub-remote ensure_ -- --nocapture
    ```

    Expected: all four new tests PASS, and the three pre-existing `ensure_remote_api_*` tests (`..._present_compatible_returns_ok_without_install`, `..._absent_and_no_source_is_remote_api_missing`, `..._local_binary_cross_platform_refuses_before_scp`) still PASS. If `..._present_compatible_returns_ok_without_install` regressed, confirm the `first.api_present && first.compatible` early return is intact and runs before any source/gate logic.

- [ ] **Step 5: Run the full remote suite + lint + fmt.**

    Command:

    ```bash
    cargo test -p aghub-remote && cargo clippy -p aghub-remote --all-targets -- -D warnings && cargo fmt -p aghub-remote --check
    ```

    Expected: every test passes (the restructure must not break `force_redeploy_*`, `start_remote_*`, `probe_*`, serde tests); zero clippy warnings; `fmt --check` clean (hard tabs, 80 cols).

- [ ] **Step 6: Commit.**

    ```bash
    # Stay on the existing feature branch `feat/desktop-bundle-aghub-api`,
    # which already holds the spec + this plan. (`git branch --show-current`
    # should print it; do NOT create a new branch.)
    git add crates/remote/src/bringup.rs
    git commit -m "$(cat <<'EOF'
    feat(remote): auto-upgrade present-but-incompatible aghub-api on connect

    Restructure ensure_remote_api so "present" no longer short-circuits: a
    present-but-incompatible binary now triggers an install/upgrade when a
    source exists and (for a LocalBinary) the platform matches. No-source +
    present-but-incompatible still returns Ok(first) (Incompatible screen);
    cross-platform + present-but-incompatible returns Ok(first); absent +
    cross-platform still returns CrossPlatformDeploy. The same-platform gate
    now covers both the absent and the upgrade path. install_remote_api's
    stage->finish (mv + chmod 755) overwrites the old binary in place.

    Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 2: Gap A — bundled-first install-source resolution with a pure precedence helper (TDD)

**Files:**

- Modify: `crates/desktop/src-tauri/src/commands/remote.rs` (imports line 41; `remote_install_source` line 570; `bring_up` line 433; `force_redeploy_remote` line 282; `remote_install_source_available` line 675)
- Test: `crates/desktop/src-tauri/src/commands/remote.rs` (inline `#[cfg(test)] mod tests`, append after line 748)

Extract the bundled→env→cargo-git ordering into a pure helper `pick_install_source` that takes already-resolved values, so the precedence is unit-testable **without** an `AppHandle`. The thin `remote_install_source(&AppHandle)` wrapper does the `BaseDirectory::Resource` resolve + env/git reads, then delegates. `bundled_api_path` is a deliberately thin `.exists()` I/O wrapper — acknowledged as not unit-testable (no `AppHandle` in a plain unit test).

- [ ] **Step 1: Add failing precedence tests.**

    Append to the `#[cfg(test)] mod tests` block in `crates/desktop/src-tauri/src/commands/remote.rs` (before its closing `}`, after line 748):

    ```rust
    	// --- pick_install_source precedence (pure, no AppHandle) --------------

    	#[test]
    	fn pick_source_prefers_bundled_over_env_and_git() {
    		let src = pick_install_source(
    			Some(PathBuf::from("/Applications/aghub.app/.../aghub-api")),
    			Some("/dev/override/aghub-api".to_string()),
    			Some("https://github.com/audichuang/aghub.git".to_string()),
    			Some("main".to_string()),
    		);
    		assert_eq!(
    			src,
    			Some(RemoteInstallSource::LocalBinary(PathBuf::from(
    				"/Applications/aghub.app/.../aghub-api"
    			))),
    			"a bundled binary wins over env + git"
    		);
    	}

    	#[test]
    	fn pick_source_falls_back_to_env_binary_when_unbundled() {
    		let src = pick_install_source(
    			None,
    			Some("/dev/override/aghub-api".to_string()),
    			Some("https://example.com/aghub.git".to_string()),
    			None,
    		);
    		assert_eq!(
    			src,
    			Some(RemoteInstallSource::LocalBinary(PathBuf::from(
    				"/dev/override/aghub-api"
    			))),
    			"env binary wins over git when unbundled"
    		);
    	}

    	#[test]
    	fn pick_source_falls_back_to_cargo_git_when_no_binary() {
    		let src = pick_install_source(
    			None,
    			None,
    			Some("https://example.com/aghub.git".to_string()),
    			Some("feat/x".to_string()),
    		);
    		assert_eq!(
    			src,
    			Some(RemoteInstallSource::CargoGit {
    				url: "https://example.com/aghub.git".to_string(),
    				branch: Some("feat/x".to_string()),
    			}),
    			"cargo-git is the last resort"
    		);
    	}

    	#[test]
    	fn pick_source_is_none_when_nothing_resolves() {
    		assert_eq!(pick_install_source(None, None, None, None), None);
    	}

    	#[test]
    	fn pick_source_ignores_blank_env_binary() {
    		// A blank/whitespace env var must not be treated as a real path; it
    		// falls through to git.
    		let src = pick_install_source(
    			None,
    			Some("   ".to_string()),
    			Some("https://example.com/aghub.git".to_string()),
    			None,
    		);
    		assert_eq!(
    			src,
    			Some(RemoteInstallSource::CargoGit {
    				url: "https://example.com/aghub.git".to_string(),
    				branch: None,
    			}),
    			"a blank env binary is ignored, falling through to git"
    		);
    	}
    ```

- [ ] **Step 2: Run the precedence tests — expect FAIL (does not compile).**

    Command (no pipe — a `| head` would mask cargo's non-zero exit status):

    ```bash
    cargo test -p aghub pick_source
    ```

    Expected: a compile error `cannot find function `pick_install_source` in this scope` (non-zero exit) — the helper does not exist yet. (Package name is `aghub` per `crates/desktop/src-tauri/Cargo.toml`.)

- [ ] **Step 3: Add the `BaseDirectory` import.**

    In `crates/desktop/src-tauri/src/commands/remote.rs:41`, change:

    ```rust
    use tauri::{AppHandle, Emitter, Manager, State};
    ```

    to:

    ```rust
    use tauri::path::BaseDirectory;
    use tauri::{AppHandle, Emitter, Manager, State};
    ```

    (`Manager` is already imported and provides `app.path()`; `BaseDirectory::Resource` is core Tauri v2.11 — no fs/shell plugin.)

- [ ] **Step 4: Implement the pure helper + the bundled-path resolver + the thin wrapper.**

    Replace the existing `fn remote_install_source()` (lines 570-590) with:

    ```rust
    /// Resolve the bundled, version-locked `aghub-api` shipped as a Tauri
    /// resource, or `None` in a dev build where it was never bundled. The
    /// `.exists()` gate keeps `bun run start` from the repo on the env/cargo-git
    /// fallback (the resource was never staged there). The executable bit is NOT
    /// handled here — the remote finish step (`crates/remote/src/ssh.rs`) chmods.
    fn bundled_api_path(app: &AppHandle) -> Option<PathBuf> {
    	let name = if cfg!(windows) {
    		"binaries/aghub-api.exe"
    	} else {
    		"binaries/aghub-api"
    	};
    	let path = app.path().resolve(name, BaseDirectory::Resource).ok()?;
    	path.exists().then_some(path)
    }

    /// Pure precedence: bundled `LocalBinary` -> env `LocalBinary` -> `CargoGit`.
    /// Extracted from [`remote_install_source`] so the bundled-first ordering is
    /// unit-testable without an `AppHandle`. A blank `env_binary` is ignored.
    fn pick_install_source(
    	bundled: Option<PathBuf>,
    	env_binary: Option<String>,
    	git_url: Option<String>,
    	git_branch: Option<String>,
    ) -> Option<RemoteInstallSource> {
    	if let Some(path) = bundled {
    		return Some(RemoteInstallSource::LocalBinary(path));
    	}
    	if let Some(raw) = env_binary {
    		let trimmed = raw.trim();
    		if !trimmed.is_empty() {
    			return Some(RemoteInstallSource::LocalBinary(PathBuf::from(
    				trimmed,
    			)));
    		}
    	}
    	let url = git_url?;
    	Some(RemoteInstallSource::CargoGit {
    		url,
    		branch: git_branch,
    	})
    }

    /// Resolve where an automatic remote install should source `aghub-api`: the
    /// bundled resource first (shipped builds), then the dev env override, then
    /// the git checkout (`cargo install --git`). Thin wrapper that performs the
    /// I/O (resource resolve + env/git reads) and delegates ordering to the pure
    /// [`pick_install_source`].
    fn remote_install_source(app: &AppHandle) -> Option<RemoteInstallSource> {
    	let bundled = bundled_api_path(app);
    	let env_binary = std::env::var("AGHUB_REMOTE_API_BINARY").ok();
    	let git_url = std::env::var("AGHUB_REMOTE_INSTALL_GIT_URL")
    		.ok()
    		.filter(|s| !s.trim().is_empty())
    		.or_else(|| git_output(&["remote", "get-url", "origin"]));
    	let git_branch = std::env::var("AGHUB_REMOTE_INSTALL_GIT_BRANCH")
    		.ok()
    		.filter(|s| !s.trim().is_empty())
    		.or_else(|| git_output(&["branch", "--show-current"]));
    	pick_install_source(bundled, env_binary, git_url, git_branch)
    }
    ```

    `git_output` (lines 592-603) is unchanged.

- [ ] **Step 5: Thread `&AppHandle` through the two callers and the command.**

    In `bring_up` (line 433), change:

    ```rust
    	let install_source = remote_install_source();
    ```

    to:

    ```rust
    	let install_source = remote_install_source(app);
    ```

    (`bring_up` already takes `app: &AppHandle` — line 427.)

    In `force_redeploy_remote` (line 282), change:

    ```rust
    	let source = remote_install_source().ok_or_else(|| {
    ```

    to:

    ```rust
    	let source = remote_install_source(&app).ok_or_else(|| {
    ```

    Use `&app` (borrow), NOT `app`: `force_redeploy_remote` takes `app: AppHandle` by value and later calls `force_redeploy(&app, ...)`; moving `app` here would break that later use.

    Replace `remote_install_source_available` (lines 674-677) with:

    ```rust
    /// Whether this build can resolve a source to deploy `aghub-api` to a remote.
    /// True in a bundled build (the embedded resource resolves) or a dev build
    /// with an env var / git checkout; false in a shipped build with none, so the
    /// UI can hide the otherwise-dead "Force redeploy" affordance.
    #[tauri::command]
    pub fn remote_install_source_available(app: AppHandle) -> bool {
    	remote_install_source(&app).is_some()
    }
    ```

    The `generate_handler!` registration in `crates/desktop/src-tauri/src/lib.rs:325` is unchanged — Tauri injects the `AppHandle` automatically, and the frontend `invoke("remote_install_source_available")` (no payload) is unaffected.

- [ ] **Step 6: Run tests + lint + fmt — expect PASS (green).**

    Command:

    ```bash
    cargo test -p aghub pick_source && cargo test -p aghub --lib && cargo clippy -p aghub --all-targets -- -D warnings && cargo fmt -p aghub --check
    ```

    Expected: the five `pick_source` tests pass; existing `slot_guard_*` / `lock_recover_*` tests still pass; zero clippy warnings; no fmt diff. (No `AppHandle` is constructed in tests — only the pure helper is exercised, which is the point of the extraction.)

- [ ] **Step 7: Commit.**

    ```bash
    git add crates/desktop/src-tauri/src/commands/remote.rs
    git commit -m "$(cat <<'EOF'
    feat(desktop): prefer the bundled aghub-api as the remote install source

    remote_install_source now takes &AppHandle and resolves the version-locked
    aghub-api shipped as a Tauri resource (BaseDirectory::Resource) before the
    dev env var / git checkout, so a packaged build gets a LocalBinary source.
    The bundled->env->cargo-git ordering is extracted into the pure
    pick_install_source helper so it is unit-testable without an AppHandle; the
    .exists() gate keeps dev builds (bun run start) on the cargo-git fallback.
    &AppHandle is threaded into remote_install_source and
    remote_install_source_available; the frontend invoke is unchanged.

    Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 3: Committed `--config` overlay + ignore the staging dir

**Files:**

- Create: `crates/desktop/src-tauri/tauri.bundle.conf.json`
- Modify: `.gitignore`
- Modify: `.prettierignore`

Config, not unit-testable Rust — verification is JSON validity + a `git check-ignore` probe + a build. Do NOT touch the committed `tauri.conf.json` (its `pubkey`/`version` must stay stable for the updater).

- [ ] **Step 1: Create the overlay file.**

    Write `crates/desktop/src-tauri/tauri.bundle.conf.json` with exactly (hard tabs, matching `tauri.conf.json`):

    ```json
    {
    	"bundle": {
    		"resources": ["binaries/aghub-api*"]
    	}
    }
    ```

    The glob `aghub-api*` covers Unix `aghub-api` and Windows `aghub-api.exe` from one file. `resources` paths resolve relative to the **main** config dir (`src-tauri/`) because `--config` merges as JSON into the main config — so the pattern means `src-tauri/binaries/aghub-api*`, and the runtime resource key Tauri assigns is `binaries/aghub-api[.exe]` (matching `bundled_api_path`).

- [ ] **Step 2: Ignore the staging dir in both ignore files.**

    Append to `.gitignore` (after the `crates/api/bindings/` line):

    ```

    # Bundled aghub-api sidecar staged by CI / the `just desktop-bundle` recipe
    # (never committed).
    crates/desktop/src-tauri/binaries/
    ```

    Add to `.prettierignore` under the `# Tauri` section, after `crates/desktop/src-tauri/target/`:

    ```
    crates/desktop/src-tauri/binaries/
    ```

- [ ] **Step 3: Verify JSON validity, that the staging dir is ignored, and that the main config is untouched.**

    Command:

    ```bash
    python3 -c "import json; json.load(open('crates/desktop/src-tauri/tauri.bundle.conf.json')); print('overlay JSON OK')"
    mkdir -p crates/desktop/src-tauri/binaries && touch crates/desktop/src-tauri/binaries/aghub-api
    git status --porcelain crates/desktop/src-tauri/binaries
    git check-ignore crates/desktop/src-tauri/binaries/aghub-api && echo "git ignores the staged binary"
    rm -rf crates/desktop/src-tauri/binaries
    git diff --quiet crates/desktop/src-tauri/tauri.conf.json && echo "main config untouched"
    ```

    Expected: `overlay JSON OK`; `git status --porcelain` prints **nothing** for that dir; `git check-ignore` prints `crates/desktop/src-tauri/binaries/aghub-api` (exit 0) then `git ignores the staged binary`; finally `main config untouched`. If `git status` shows the file as untracked, the `.gitignore` entry is wrong.

- [ ] **Step 4: Commit.**

    ```bash
    git add crates/desktop/src-tauri/tauri.bundle.conf.json .gitignore .prettierignore
    git commit -m "$(cat <<'EOF'
    build(desktop): add tauri.bundle.conf.json overlay + ignore binaries/

    A committed --config overlay declares bundle.resources = ["binaries/
    aghub-api*"] for the embedded aghub-api, applied only by CI / the just
    recipe so the committed tauri.conf.json (pubkey/version/bundle) stays clean
    and plain tauri dev / cargo build are unaffected. The staged binaries/ dir
    is gitignored and prettierignored so a CI/recipe-staged sidecar is never
    committed or dirties the lint/format gates.

    Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 4: Local `just desktop-bundle` recipe (distribution, today)

**Files:**

- Modify: `justfile`

A one-command local path to an installable desktop build with the embedded sidecar, mirroring CI. The justfile uses **4-space** recipe-body indentation (verified: `build`, `preflight`, `bump`); this recipe uses a `#!/usr/bin/env bash` shebang so `just` runs the whole body as one bash script (so `set -euo pipefail`, the `case`, and `cd` share one shell), which sidesteps the top-level `set windows-shell`.

- [ ] **Step 1: Append the recipe to `justfile`.**

    Add at the end of `justfile` (4-space body indentation to match the file):

    ```just

    # Produce an installable desktop bundle with the version-locked aghub-api
    # embedded as a Tauri resource (mirrors the release CI staging). Cleans +
    # stages crates/desktop/src-tauri/binaries/aghub-api[.exe] for the HOST
    # triple, then runs the bundle build with the committed --config overlay.
    # The committed tauri.conf.json is never modified; plain `just desktop` /
    # `bun run dev` stay on the cargo-git fallback (no staged sidecar).
    desktop-bundle:
        #!/usr/bin/env bash
        set -euo pipefail
        HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
        echo "Host triple: $HOST_TRIPLE"
        BIN="aghub-api"
        case "$HOST_TRIPLE" in
          *windows*) BIN="aghub-api.exe" ;;
        esac
        # Absolute path + an EXIT trap so the staged sidecar is ALWAYS removed,
        # even if the tauri build fails partway — a leftover staging dir would
        # make a later `bun run dev` wrongly resolve a stale `.exists()`-gated
        # bundled source. Absolute so the trap works regardless of the `cd`.
        STAGE="$(pwd)/crates/desktop/src-tauri/binaries"
        trap 'rm -rf "$STAGE"' EXIT
        rm -rf "$STAGE"
        mkdir -p "$STAGE"
        cargo build -p aghub-api --release
        cp "target/release/$BIN" "$STAGE/$BIN"
        echo "Staged $STAGE/$BIN"
        cd crates/desktop
        bun run tauri build --config src-tauri/tauri.bundle.conf.json --config '{"bundle":{"createUpdaterArtifacts":false}}'
        # The bundle now embeds the sidecar; the EXIT trap removes the staging
        # dir so the tree stays clean and `bun run dev` stays on the fallback.
        echo "Bundle built; staging dir will be removed on exit."
    ```

    Do NOT add a `--` separator: `bun run tauri build -- --config ...` would make tauri forward `--config` to the underlying `cargo build`, where cargo parses the `.json` path as a `--config` dotted-key TOML expression and fails (`failed to parse value from --config argument ... as a dotted key expression`). Without the `--`, `--config` is consumed by the tauri CLI (which accepts a path to a JSON/JSON5/TOML file), and the path is relative to `crates/desktop` (the `cd`'d cwd), so `src-tauri/tauri.bundle.conf.json` resolves. A SECOND `--config '{"bundle":{"createUpdaterArtifacts":false}}'` is appended (the two overlays merge over `tauri.conf.json` in order) so a LOCAL build does not require the `TAURI_SIGNING_PRIVATE_KEY` secret: the committed config sets `createUpdaterArtifacts: true` + a `pubkey` for the release auto-updater, which makes `tauri build` fail at the signing step when no private key is present. CI sets the key and keeps signing on; local installs don't need a signed updater artifact, and the produced `.deb`/`.rpm`/`.AppImage` are identical either way. `$STAGE` is an absolute path with a `trap 'rm -rf "$STAGE"' EXIT`, so the staged binary is removed on every exit — including a failed build — and never lingers to be wrongly resolved by a later `bun run dev`.

- [ ] **Step 2: Verify the recipe parses and the host-triple detection works.**

    Command:

    ```bash
    just --list | grep desktop-bundle
    rustc -vV | sed -n 's/^host: //p'
    ```

    Expected: `desktop-bundle` appears in the list with its doc comment; the second command prints a single triple (e.g. `x86_64-unknown-linux-gnu`). If `just --list` errors with a parse error, the recipe indentation is wrong.

- [ ] **Step 3: Verify the stage portion produces a runnable host binary.**

    Run only the staging commands (avoids a multi-minute full bundle in this checkpoint):

    ```bash
    HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"; cargo build -p aghub-api --release && mkdir -p crates/desktop/src-tauri/binaries && cp target/release/aghub-api crates/desktop/src-tauri/binaries/aghub-api && crates/desktop/src-tauri/binaries/aghub-api --version
    ```

    Expected: prints `aghub-api <version>` whose value matches the root `Cargo.toml` `version` (`1.1.1` for a local untagged build, since the local build is not version-synced). If `--version` errors, the staged path is wrong.

- [ ] **Step 4: Run the full recipe and assert the resource landed in the bundle.**

    Command (this performs a real desktop bundle build; allow several minutes):

    ```bash
    just desktop-bundle
    find crates/desktop/src-tauri/target -path '*resources/binaries/aghub-api' -o -path '*binaries/aghub-api' -type f 2>/dev/null | head
    ```

    Expected: at least one path ending in `binaries/aghub-api` inside the produced bundle's resource tree (e.g. under the AppImage / `.deb` payload on Linux). If empty, re-check the overlay path base (`src-tauri/binaries/...`) and that staging ran before the build. Then confirm the staging dir is still untracked:

    ```bash
    git status --porcelain crates/desktop/src-tauri/binaries
    rm -rf crates/desktop/src-tauri/binaries
    ```

    Expected: empty output (gitignored).

- [ ] **Step 5: Commit.**

    ```bash
    git add justfile
    git commit -m "$(cat <<'EOF'
    build: add `just desktop-bundle` to produce a bundled desktop build locally

    Detects the host triple, cleans+stages
    crates/desktop/src-tauri/binaries/aghub-api[.exe], and runs the desktop
    bundle build with --config src-tauri/tauri.bundle.conf.json — a one-command
    path to an installable build with the embedded sidecar, mirroring CI. The
    committed config stays clean.

    Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 5: Release CI staging + version assertion + host-arch smoke (`build-tauri`)

**Files:**

- Modify: `.github/workflows/release.yml` (`build-tauri` job: `Sync Version` step lines 175-193; a new step before `Build Tauri` at line 195; the tauri-action `args` at line 217)

CI behavior cannot be unit-tested; it is verified by YAML validity + a local dry-run of the smoke logic + a tag push. `build-cli` is unchanged. Both macOS targets (`aarch64-apple-darwin`, `x86_64-apple-darwin`) build on the same arm64 `macos-latest` runner, so executing the x86_64 binary relies on Rosetta (NOT guaranteed) — the smoke must be host-arch-aware.

- [ ] **Step 1: Harden `Sync Version` — drop `|| true` on the `tauri.conf.json` sed only, add a post-sync version assertion.**

    In the `build-tauri` job's `Sync Version` step (lines 179-193), replace the whole `run:` body with:

    ```bash
                    VERSION="${VERSION#v}"
                    echo "Syncing version across files: $VERSION"
                    case "${{ matrix.os }}" in
                      macos*)
                        sed -i '' "s/^version = .*/version = \"$VERSION\"/" Cargo.toml
                        sed -i '' "s/\"version\": \".*\"/\"version\": \"$VERSION\"/" crates/desktop/package.json
                        sed -i '' "s/\"version\": \".*\"/\"version\": \"$VERSION\"/" crates/desktop/src-tauri/tauri.conf.json
                        ;;
                      *)
                        sed -i "s/^version = .*/version = \"$VERSION\"/" Cargo.toml
                        sed -i "s/\"version\": \".*\"/\"version\": \"$VERSION\"/" crates/desktop/package.json
                        sed -i "s/\"version\": \".*\"/\"version\": \"$VERSION\"/" crates/desktop/src-tauri/tauri.conf.json
                        ;;
                    esac
                    # sed exits 0 even on no-match, so prove the sync actually took.
                    # package.json has NO "version" field, so do NOT assert it; its
                    # sed is a harmless no-op left in place.
                    grep -q "^version = \"$VERSION\"$" Cargo.toml \
                      || { echo "Cargo.toml version not synced to $VERSION" >&2; exit 1; }
                    grep -q "\"version\": \"$VERSION\"" crates/desktop/src-tauri/tauri.conf.json \
                      || { echo "tauri.conf.json version not synced to $VERSION" >&2; exit 1; }
                    echo "Version sync asserted: $VERSION"
    ```

    The `|| true` is removed from BOTH OS branches' `tauri.conf.json` sed (its `version` line DOES exist and match — the file currently shows `"version": "1.2.1"`). The `package.json` sed stays as-is with no assertion.

- [ ] **Step 2: Add the "Stage version-locked aghub-api" step AFTER `Sync Version`, BEFORE `Build Tauri`.**

    Insert this step immediately after the `Sync Version` step (after line 193, before the `- name: Build Tauri` block at line 195):

    ```yaml
    - name: Stage version-locked aghub-api (per target)
      shell: bash
      env:
          VERSION: ${{ github.event.inputs.version || github.ref_name }}
      run: |
          set -euo pipefail
          VERSION="${VERSION#v}"
          TARGET="${{ matrix.target }}"
          BIN="aghub-api"
          if [ "$TARGET" = "x86_64-pc-windows-msvc" ]; then
              BIN="aghub-api.exe"
          fi
          STAGE="crates/desktop/src-tauri/binaries"
          rm -rf "$STAGE"
          mkdir -p "$STAGE"
          # Built AFTER Sync Version, so it is version-locked to the tag.
          cargo build -p aghub-api --release --target "$TARGET"
          cp "target/$TARGET/release/$BIN" "$STAGE/$BIN"
          test -f "$STAGE/$BIN" \
            || { echo "sidecar not staged at $STAGE/$BIN" >&2; exit 1; }

          # Host-arch-aware smoke. Both aarch64-apple-darwin and
          # x86_64-apple-darwin build on the same arm64 macos-latest
          # runner; running the x86_64 binary relies on Rosetta, which is
          # NOT guaranteed. Only EXECUTE the binary when its target arch
          # matches the runner host arch; otherwise verify via the synced
          # Cargo.toml version string + file existence (never execute).
          HOST_ARCH="$(uname -m)"   # arm64 / aarch64 / x86_64
          case "$TARGET" in
            aarch64-*) TGT_ARCH="arm64" ;;
            x86_64-*)  TGT_ARCH="x86_64" ;;
            *)         TGT_ARCH="unknown" ;;
          esac
          norm() { case "$1" in aarch64) echo arm64 ;; *) echo "$1" ;; esac; }
          if [ "$(norm "$HOST_ARCH")" = "$(norm "$TGT_ARCH")" ]; then
              echo "Native target ($TARGET on $HOST_ARCH): executing --version"
              OUT="$("$STAGE/$BIN" --version)"
              echo "sidecar reports: $OUT"
              WANT_MM="$(printf '%s\n' "$VERSION" | cut -d. -f1-2)"
              GOT_MM="$(printf '%s\n' "$OUT" | sed -n 's/^aghub-api \([0-9]*\.[0-9]*\).*/\1/p')"
              [ -n "$GOT_MM" ] && [ "$GOT_MM" = "$WANT_MM" ] \
                || { echo "sidecar version $GOT_MM != synced $WANT_MM" >&2; exit 1; }
          else
              echo "Non-native target ($TARGET on $HOST_ARCH): file+version-string check, not executing"
              grep -q "^version = \"$VERSION\"$" Cargo.toml \
                || { echo "Cargo.toml not at $VERSION for non-native smoke" >&2; exit 1; }
          fi
    ```

- [ ] **Step 3: Pass the overlay to tauri-action.**

    In the `Build Tauri` step's `with:` block, change line 217:

    ```yaml
    args: --target ${{ matrix.target }}
    ```

    to:

    ```yaml
    args: --target ${{ matrix.target }} --config src-tauri/tauri.bundle.conf.json
    ```

    The `--config` path is relative to `projectPath: crates/desktop` (line 219), so it resolves to `crates/desktop/src-tauri/tauri.bundle.conf.json`; Tauri merges its `bundle.resources` into the main config and the staged `binaries/aghub-api*` is bundled. `build-cli` is unchanged.

- [ ] **Step 4: Validate the workflow YAML locally.**

    Command:

    ```bash
    python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml')); print('release.yml YAML OK')"
    ```

    Expected: `release.yml YAML OK`. If `actionlint` is installed, also run `actionlint .github/workflows/release.yml` and expect no output; if it is not installed, skip it.

- [ ] **Step 5: Dry-run the host-arch smoke logic against the real host triple.**

    This proves the native/non-native branch selection before relying on a tag push (the actual cross-platform bundling cannot be exercised locally):

    ```bash
    VERSION="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/')"; TARGET="$(rustc -vV | sed -n 's/^host: //p')"
    bash -c '
      set -euo pipefail
      VERSION="'"$VERSION"'"; TARGET="'"$TARGET"'"; HOST_ARCH="$(uname -m)"
      case "$TARGET" in aarch64-*) TGT_ARCH=arm64;; x86_64-*) TGT_ARCH=x86_64;; *) TGT_ARCH=unknown;; esac
      norm() { case "$1" in aarch64) echo arm64;; *) echo "$1";; esac; }
      if [ "$(norm "$HOST_ARCH")" = "$(norm "$TGT_ARCH")" ]; then echo "would EXECUTE (native): VERSION=$VERSION"; else echo "would FILE-CHECK (non-native)"; fi
    '
    ```

    Expected on an x86_64 Linux host: `would EXECUTE (native): VERSION=1.1.1`. (On the macOS runner, the `x86_64-apple-darwin` leg prints `would FILE-CHECK (non-native)` — the Rosetta-safe path.)

- [ ] **Step 6: CI-only validation note (honest).**

    Per-target resource bundling, the four-target matrix naming (incl. Windows `.exe`), the macOS dual-target Rosetta skip, the runtime `BaseDirectory::Resource` resolution inside a packaged bundle, and the overlay `--config` merge **cannot** be exercised locally — they are verification-only until a tag push, validated via the `releasing-aghub` re-release-a-botched-tag flow: push a `v*` tag on a CI-green commit, watch the `Build Desktop (...)` matrix, confirm each leg's "Stage version-locked aghub-api" step is green and the produced bundle embeds `Resources/binaries/aghub-api[.exe]`, then cancel/re-tag per the runbook if a leg fails. State this plainly in the PR description; do NOT claim CI is "verified" from a local run.

- [ ] **Step 7: Commit.**

    ```bash
    git add .github/workflows/release.yml
    git commit -m "$(cat <<'EOF'
    ci(release): stage version-locked aghub-api into the desktop bundle

    After Sync Version (so it is version-locked) the build-tauri job asserts
    the synced version is actually present (sed exits 0 on no-match) and drops
    the || true on the tauri.conf.json sed, builds aghub-api per target, stages
    it to src-tauri/binaries/aghub-api[.exe], and passes
    --config src-tauri/tauri.bundle.conf.json to tauri-action so it is bundled
    as a resource. The smoke is host-arch-aware: it only executes --version
    when the target arch == the runner host arch (the two macOS targets share
    an arm64 runner, so the x86_64 binary is verified by file + version string,
    never run under unguaranteed Rosetta). package.json (no version field) is
    left as a no-op. build-cli is unchanged.

    Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
    EOF
    )"
    ```

---

### Task 6: Full preflight, dev-untouched regression, and tag-push verification

**Files:** none (verification + integration).

- [ ] **Step 1: Run the full preflight gate.**

    Command:

    ```bash
    just preflight
    ```

    Expected: `cargo fmt --all --check` (no diff), `cargo clippy --workspace --all-targets -- -D warnings` (zero warnings), desktop `bun run typecheck` (clean), `cargo test --workspace` and `cargo test --workspace --doc` (all pass — including the new `aghub-remote` branch tests and the desktop `pick_source` tests). This is the same gate CI's `test` job runs; tag only a commit that passes it locally (the pre-push hook does NOT run tests).

- [ ] **Step 2: Prove plain dev builds are untouched (no overlay, no staged sidecar).**

    With NO `binaries/` dir staged and NOT passing the overlay:

    ```bash
    rm -rf crates/desktop/src-tauri/binaries
    cd crates/desktop && bun run tauri build && cd ../..
    ```

    Expected: the build succeeds with no staged sidecar (the committed `tauri.conf.json` has no `bundle.resources`, so nothing is required). This confirms `bun run dev` / `bun run start` / plain `cargo build` remain on the env/cargo-git fallback — `bundled_api_path` returns `None` because the resource was never bundled, so `remote_install_source` resolves `CargoGit` from the repo's `git remote get-url origin`.

- [ ] **Step 3: Final tree-clean check.**

    Command:

    ```bash
    git status --porcelain
    ```

    Expected: clean (all task commits landed; no stray `binaries/` entry).

- [ ] **Step 4: Open the PR and validate CI on a tag.**

    ```bash
    git push -u origin feat/desktop-bundle-aghub-api
    gh pr create --fill --base main
    ```

    Then, per the `releasing-aghub` runbook, push a `v*` tag on the CI-green commit and watch the `build-tauri` matrix prove the CI-only pieces (per-target staging, host-arch smoke across the two macOS targets, the `--config` overlay embedding `binaries/aghub-api[.exe]`, and runtime `BaseDirectory::Resource` resolution). State plainly in the PR description that Task 5's CI behavior is verification-only until that tag run is green. If the tag run is botched, follow the re-release-a-botched-tag flow (cancel the run, delete the tag, re-tag) rather than letting a half-published release stand. End the PR body with:

    ```
    🤖 Generated with [Claude Code](https://claude.com/claude-code)
    ```

---

## Honesty notes on testability

- **Fully unit-tested (real red→green):** Gap B's `ensure_remote_api` branch logic (Task 1, `MockRunner`) and Gap A's bundled→env→cargo-git precedence (Task 2, pure `pick_install_source`, 5 tests incl. blank-env).
- **MockRunner side-effect assertions (stated honestly):** `MockRunner` keys on exact `(program, args)` and the two `ensure_remote_api` probes build identical argv, so the upgrade test scripts the probe key **incompatible-only** and asserts on side-effects (an `scp` + `finish` ran, `install_attempted == true`) — NOT a version flip across the two probes. The absent+same-platform test asserts the install ran then a `DeployFailed` (the re-probe replays the same `127`). The stage→finish→re-probe version contract itself stays covered by the pre-existing `force_redeploy_stages_then_finishes_then_probes` (its probe runs once). `bundled_api_path` is a thin `.exists()` I/O wrapper with no branching to test — the testable logic was extracted out of it into `pick_install_source`.
- **Verification-only (no unit test possible):** the `tauri.bundle.conf.json` overlay `--config` merge, the `release.yml` per-target staging / host-arch smoke / per-target naming / macOS Rosetta skip, and the runtime `BaseDirectory::Resource` resolution inside a packaged bundle. These are validated locally by JSON/YAML validity, the dry-run of the smoke logic (Task 5 Step 5), the `just desktop-bundle` end-to-end build (Task 4 Step 4), and a plain dev build regression (Task 6 Step 2) — and confirmed in full only by a `v*` tag push, called out explicitly in Tasks 5 and 6.
