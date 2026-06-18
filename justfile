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
test:
    cargo test --workspace

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

# Bump version across all manifests
bump version:
    sed -i '' 's/^version = .*/version = "{{version}}"/' Cargo.toml
    sed -i '' 's/"version": ".*"/"version": "{{version}}"/' crates/desktop/package.json
    sed -i '' 's/"version": ".*"/"version": "{{version}}"/' crates/desktop/src-tauri/tauri.conf.json || true
