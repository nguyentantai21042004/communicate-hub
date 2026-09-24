.PHONY: check fmt fmt-check lint test lint-comments build gc

check: fmt-check lint test lint-comments ## everything CI would run
	@scripts/target-gc.sh

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
	@scripts/target-gc.sh

gc: ## trim target/ now if it is over the cap
	scripts/target-gc.sh
