export CARGO_BUILD_JOBS ?= 4

.DEFAULT_GOAL := help
.PHONY: help build release check check-all fmt lint test test-unit test-integration test-typst-html test-remote-images coverage verify demo clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
	  | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-18s\033[0m %s\n",$$1,$$2}'

build: ## Debug build (default features = pandoc + typst)
	cargo build

release: ## Release build
	cargo build --release

check: ## Fast typecheck (default features)
	cargo check

check-all: ## Typecheck all feature combinations (the CI contract)
	cargo check --no-default-features
	cargo check --no-default-features --features pandoc
	cargo check --no-default-features --features typst
	cargo check --no-default-features --features pandoc,typst,rt-multi-thread
	cargo check
	cargo check --features typst-html
	cargo check --features remote-images

fmt: ## Apply formatting
	cargo fmt

lint: ## fmt-check + clippy (default + typst-html + remote-images features), warnings are errors (mirrors CI)
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo clippy --all-targets --features typst-html -- -D warnings
	cargo clippy --all-targets --features remote-images -- -D warnings

test-unit: ## Unit tests (in-module #[cfg(test)])
	cargo test --lib --bins

test-integration: ## Integration tests (tests/ — incl. engine smoke tests; pandoc-backed ones skip if pandoc is absent)
	cargo test --test '*'

test: ## All tests (default features)
	cargo test

test-typst-html: ## Tests for the off-by-default typst-html feature (issue #53's HTML export)
	cargo test --features typst-html

test-remote-images: ## Tests for the off-by-default remote-images feature (issue #54's http(s) image fetch)
	cargo test --features remote-images

coverage: ## Test coverage: lcov.info + terminal summary (needs cargo-llvm-cov)
	cargo llvm-cov --lcov --output-path lcov.info
	cargo llvm-cov report --summary-only

verify: lint check-all test test-typst-html test-remote-images ## Pre-"done" gate: lint + all feature combos + all tests

demo: build ## Render the golden fixture to target/demo/ (html-reveal + pdf)
	./target/debug/mdcast render tests/golden/cover-deck.md --target html-reveal --out target/demo/cover-deck.html
	./target/debug/mdcast render tests/golden/cover-deck.md --target pdf --out target/demo/cover-deck.pdf

clean: ## Remove build artifacts
	cargo clean
