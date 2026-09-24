.PHONY: check fmt fmt-check lint test lint-comments build

check: fmt-check lint test lint-comments ## everything CI would run

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

lint-comments:
	scripts/lint-comments.sh

build:
	cargo build --release -p captain-cli
