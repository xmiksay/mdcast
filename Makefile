export CARGO_BUILD_JOBS ?= 4

.DEFAULT_GOAL := help
.PHONY: help build release check check-all fmt lint test test-unit test-integration test-typst-html test-remote-images test-server coverage verify demo run-server clean

# Dev defaults for `make run-server` — an exported environment variable wins.
# The token is a placeholder for local runs; a deployment must set its own.
MDCAST_TOKEN ?= dev-token
MDCAST_BLOB_DIR ?= target/mdcast-server/blobs

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
	  | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-18s\033[0m %s\n",$$1,$$2}'

# Workspace: `mdcast` (root package) + `mdcast-api` + `mdcast-client`. Feature
# flags belong to `mdcast` alone, so anything passing --features is scoped with
# `-p mdcast`; everything else runs across the whole workspace, which is what
# builds, lints and tests the two client-side members.
build: ## Debug build (default features = pandoc + typst)
	cargo build --workspace

release: ## Release build
	cargo build --workspace --release

check: ## Fast typecheck (default features)
	cargo check --workspace

check-all: ## Typecheck all feature combinations (the CI contract)
	cargo check -p mdcast-api
	cargo check -p mdcast-client
	cargo check -p mdcast --no-default-features
	cargo check -p mdcast --no-default-features --features pandoc
	cargo check -p mdcast --no-default-features --features typst
	cargo check -p mdcast --no-default-features --features pandoc,typst,rt-multi-thread
	cargo check -p mdcast
	cargo check -p mdcast --features typst-html
	cargo check -p mdcast --features remote-images
	cargo check -p mdcast --features server
	cargo check -p mdcast --features server,typst-html

fmt: ## Apply formatting
	cargo fmt --all

lint: ## fmt-check + clippy (default + typst-html + remote-images + server features), warnings are errors (mirrors CI)
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy -p mdcast --all-targets --features typst-html -- -D warnings
	cargo clippy -p mdcast --all-targets --features remote-images -- -D warnings
	cargo clippy -p mdcast --all-targets --features server -- -D warnings
	cargo clippy -p mdcast --all-targets --features server,typst-html -- -D warnings

test-unit: ## Unit tests (in-module #[cfg(test)])
	cargo test --workspace --lib --bins

test-integration: ## Integration tests (tests/ — engine smoke tests, which skip if pandoc is absent, plus the client's HTTP flow)
	cargo test -p mdcast -p mdcast-client --test '*'

test: ## All tests, whole workspace (default features)
	cargo test --workspace

test-typst-html: ## Tests for the off-by-default typst-html feature (issue #53's HTML export)
	cargo test -p mdcast --features typst-html

test-remote-images: ## Tests for the off-by-default remote-images feature (issue #54's http(s) image fetch)
	cargo test -p mdcast --features remote-images

test-server: ## Tests for the off-by-default server feature (mdcast-server: blob store, auth, routes)
	cargo test -p mdcast --features server
	cargo test -p mdcast --features server,typst-html --bin mdcast-server

coverage: ## Test coverage: lcov.info + terminal summary (needs cargo-llvm-cov)
	cargo llvm-cov --workspace --lcov --output-path lcov.info
	cargo llvm-cov report --summary-only

verify: lint check-all test test-typst-html test-remote-images test-server ## Pre-"done" gate: lint + all feature combos + all tests

demo: build ## Render the golden fixture to target/demo/ (html-reveal + pdf)
	./target/debug/mdcast render tests/golden/cover-deck.md --target html-reveal --out target/demo/cover-deck.html
	./target/debug/mdcast render tests/golden/cover-deck.md --target pdf --out target/demo/cover-deck.pdf

run-server: ## Run the HTTP service (override MDCAST_TOKEN / MDCAST_BIND / MDCAST_BLOB_DIR)
	MDCAST_TOKEN=$(MDCAST_TOKEN) MDCAST_BLOB_DIR=$(MDCAST_BLOB_DIR) \
	  cargo run -p mdcast --features server --bin mdcast-server

clean: ## Remove build artifacts
	cargo clean
