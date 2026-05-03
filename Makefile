.PHONY: ci fix fmt clippy test audit deny build deb clean

# Run everything CI runs, in the same order.
# This is the thing to run before pushing.
ci: fmt clippy test audit deny

# Auto-fix formatting and clippy lint suggestions, then run the full CI suite.
fix:
	cargo fmt
	cargo clippy --fix --allow-dirty --allow-staged -- -D warnings
	$(MAKE) ci

fmt:
	cargo fmt --check

clippy:
	cargo clippy -- -D warnings

test:
	cargo test

audit:
	cargo audit

deny:
	cargo deny check

# Build the release binary (what CI and `make deb` produce).
build:
	cargo build --release

# Build a .deb package.  Depends on build so the binary is always current.
deb: build
	cargo deb --no-build

clean:
	cargo clean
