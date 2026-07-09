# aghub — 本地開發常用命令
#
# 最常用：`make app`  → 啟動桌面 App（Tauri dev，含後端）
#         `make`      → 顯示所有指令
#
# 需求：cargo、bun（本專案禁用 npm/yarn/pnpm）。
#       桌面 App 在 Linux 需 GTK dev libs。
#
# 註：專案另有 justfile（`just ...`）與 bun scripts；本 Makefile 是不依賴
#     just/nr 的等價捷徑，命令與它們對齊。

DESKTOP := crates/desktop

.DEFAULT_GOAL := help
.PHONY: help app web build-desktop dev cli build install \
        test test-fe check lint fmt preflight dto clean

help: ## 顯示這份說明
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

# ─────────────────────────── 桌面 App（Tauri）───────────────────────────

app: ## 啟動桌面 App（Tauri dev，含後端）← 最常用，取代 cd + bun run start
	bun run --cwd $(DESKTOP) start

web: ## 只啟前端（Vite dev，無 Tauri 後端）
	bun run --cwd $(DESKTOP) dev

build-desktop: ## 打包桌面 App（Tauri build，產出安裝檔）
	bun run --cwd $(DESKTOP) tauri build

# ────────────────────────────────── CLI ─────────────────────────────────

dev: ## debug build CLI（aghub-cli）
	cargo build -p aghub-cli

cli: ## 跑 CLI（傳參：make cli ARGS="-a claude get skills"）
	cargo run -p aghub-cli -- $(ARGS)

build: ## release build CLI
	cargo build --release -p aghub-cli

install: build ## 把 aghub-cli 裝到 ~/.cargo/bin
	cp target/release/aghub-cli ~/.cargo/bin/

# ──────────────────────────── 測試 / 檢查 / 格式 ──────────────────────────

test: ## 跑全 workspace 測試（Rust）
	cargo test --workspace

test-fe: ## 跑前端測試（node:test，src/**/*.test.ts）
	bun run --cwd $(DESKTOP) test

check: ## 前端 typecheck（tsc）
	bun run --cwd $(DESKTOP) typecheck

lint: ## clippy（-D warnings）+ eslint
	cargo clippy --workspace -- -D warnings
	cd $(DESKTOP) && bunx eslint src --max-warnings=0

fmt: ## 格式化（rustfmt + prettier）
	cargo fmt --all
	bun run --cwd $(DESKTOP) format

preflight: ## push/tag 前完整 gate（fmt-check + clippy + typecheck + test + doc）
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	bun run --cwd $(DESKTOP) typecheck
	cargo test --workspace
	cargo test --workspace --doc

dto: ## 改 Rust API 型別後，重新產生前端 DTO
	bun run --cwd $(DESKTOP) generate:dto

clean: ## 清 build 產物
	cargo clean
