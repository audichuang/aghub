# aghub - Code Agent Management Tool
# https://github.com/akarachen/aghub

set windows-shell := ["cmd.exe", "/c"]

# Default recipe - build the CLI
default: build

# Build the CLI binary (aghub-cli)
build:
    cargo build --release -p aghub-cli

# Build for development
dev:
    cargo build -p aghub-cli

# Run all tests
# --no-fail-fast: without it cargo stops at the FIRST failing test binary, so a
# red CI leg reveals one problem per run and a platform-specific batch costs one
# full cycle each. (Comments must live OUTSIDE the recipe body — an indented `#`
# is handed to the shell, and cmd.exe on the Windows leg does not know it.)
test:
    cargo test --workspace --no-fail-fast

# Run integration tests only
integration-test:
    cargo test -p aghub-core --test integration_tests

# Run tests with agent validation (requires claude/opencode CLIs)
test-with-validation:
    cargo test --workspace --features agent-validation

# Format code
fmt:
	cargo fmt --all
	bun run format

# Run clippy linter
lint:
    cargo clippy --workspace -- -D warnings
    cd ./crates/desktop && nr lint

# Pre-release gate: run everything the CI / release test gate runs, locally.
# Run this BEFORE you push or tag. The pre-push hook only does
# prettier/clippy/eslint/tsc — it does NOT run tests, so tests can still fail
# in CI after a clean push. This closes that gap.
#
# IMPORTANT: this runs on YOUR platform only. It CANNOT reproduce
# macOS/Windows-specific behavior (e.g. /var -> /private symlink canonicalize,
# path separators, case-insensitive FS). For real cross-platform confidence,
# push and let CI's 3-OS matrix run — the Release is now gated on it. And when
# you touch path/fs code, add a test that SIMULATES the platform condition on
# Linux (e.g. operate through a symlinked temp dir to mimic macOS /private).
preflight:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd ./crates/desktop && bun run typecheck
    cargo test --workspace
    cargo test --workspace --doc

# Clean build artifacts
clean:
    cargo clean

# Install aghub-cli to ~/.cargo/bin
install: build
    cp target/release/aghub-cli ~/.cargo/bin/

# Run aghub-cli with --help
help: dev
    ./target/debug/aghub-cli --help

# Run with cargo (pass args: just start -- --arg)
start *args:
    cargo run -p aghub-cli -- {{args}}

desktop:
    cd ./crates/desktop && nr start

# Bump version across all manifests (perl -i is portable across GNU/BSD;
# `sed -i ''` was macOS-only and errored on Linux)
bump version:
    perl -i -pe 's/^version = .*/version = "{{version}}"/' Cargo.toml
    perl -i -pe 's/"version": "[^"]*"/"version": "{{version}}"/' crates/desktop/package.json
    perl -i -pe 's/"version": "[^"]*"/"version": "{{version}}"/' crates/desktop/src-tauri/tauri.conf.json

# Cut a release: YOU pick the version, the script does the rest — verifies the
# HEAD commit's ci.yml is green on the fork, tags vX.Y.Z, pushes it to `fork`
# (never origin/upstream), watches release.yml (auto-reruns once on a transient
# CI flake), then verifies the published artifacts. Pass --yes to skip the
# confirm prompt. See scripts/release.sh + the releasing-aghub skill.
release version *flags:
    bash scripts/release.sh {{version}} {{flags}}

# Detailed notes: stages crates/desktop/src-tauri/binaries/aghub-api[.exe]
# for the HOST triple, then runs the bundle build with the committed
# --config overlay (mirrors the release CI staging). The committed
# tauri.conf.json is never modified; plain `just desktop` / `bun run dev`
# stay on the cargo-git fallback (no staged sidecar).

# Build an installable desktop bundle with the version-locked aghub-api embedded.
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
    # even if the tauri build fails partway — a leftover staging dir would make
    # a later `bun run dev` wrongly resolve a stale `.exists()`-gated bundled
    # source. Absolute so the trap works regardless of the `cd` below.
    STAGE="$(pwd)/crates/desktop/src-tauri/binaries"
    trap 'rm -rf "$STAGE"' EXIT
    rm -rf "$STAGE"
    mkdir -p "$STAGE"
    cargo build -p aghub-api --release
    cp "target/release/$BIN" "$STAGE/$BIN"
    echo "Staged $STAGE/$BIN"
    cd crates/desktop
    # Two `--config` overlays merge over tauri.conf.json (in order):
    #  1. the committed resources overlay (embeds the staged aghub-api), and
    #  2. createUpdaterArtifacts=false so a LOCAL build does not require the
    #     TAURI_SIGNING_PRIVATE_KEY secret (the committed config sets it true +
    #     a pubkey for the release updater; CI signs, local installs don't need
    #     it). The produced .deb/.rpm/.AppImage are identical either way.
    bun run tauri build --config src-tauri/tauri.bundle.conf.json --config '{"bundle":{"createUpdaterArtifacts":false}}'
    # The bundle now embeds the sidecar; the EXIT trap removes the staging dir
    # so the working tree stays clean and `bun run dev` stays on the fallback.
    echo "Bundle built; staging dir will be removed on exit."

# Build a LOCAL macOS (Apple Silicon) .dmg and install it to /Applications for
# fast manual testing — skips the whole tag -> CI -> download round-trip. Mirrors
# `desktop-bundle` sidecar staging but emits ONLY the dmg for the host arch, then
# installs the app and prints the dmg path.
#
# macOS / Apple Silicon only (host build == arm64 there). Needs the release
# toolchain (rustup, bun, sccache). The build is UNSIGNED and has no updater
# artifacts — it is for LOCAL testing only; ship real builds via `just`-tagged
# releases, never this dmg.
[macos]
desktop-dmg:
    #!/usr/bin/env bash
    set -euo pipefail
    STAGE="$(pwd)/crates/desktop/src-tauri/binaries"
    trap 'rm -rf "$STAGE"' EXIT
    rm -rf "$STAGE"
    mkdir -p "$STAGE"
    cargo build -p aghub-api --release
    cp "target/release/aghub-api" "$STAGE/aghub-api"
    echo "Staged sidecar: $STAGE/aghub-api"
    (cd crates/desktop && bun run tauri build --bundles dmg \
      --config src-tauri/tauri.bundle.conf.json \
      --config '{"bundle":{"createUpdaterArtifacts":false}}')
    APP="target/release/bundle/macos/aghub.app"
    DMG="$(ls -t target/release/bundle/dmg/aghub_*.dmg | head -1)"
    echo "Installing $APP -> /Applications/aghub.app (replacing any existing copy)"
    rm -rf "/Applications/aghub.app"
    cp -R "$APP" "/Applications/aghub.app"
    # Locally-built apps are not quarantined, but strip it defensively so the
    # first launch does not hit Gatekeeper.
    xattr -dr com.apple.quarantine "/Applications/aghub.app" 2>/dev/null || true
    echo "Installed: /Applications/aghub.app"
    echo "DMG:       $(pwd)/$DMG"
    open "/Applications/aghub.app"
