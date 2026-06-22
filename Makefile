.PHONY: build release clean test

build:
	cargo build --bin translate-cli

release:
	cargo build --bin translate-cli --release

lint:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features --quiet -- -D warnings

clean:
	cargo clean

test:
	cargo test
