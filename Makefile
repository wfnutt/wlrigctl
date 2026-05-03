.PHONY: check fix fmt clippy test audit deny

# Run everything CI runs, in the same order.
# This is the thing to run before pushing.
check: fmt clippy test audit deny

# Auto-fix formatting and clippy lint suggestions, then run the full check.
fix:
	cargo fmt
	cargo clippy --fix --allow-dirty --allow-staged -- -D warnings
	$(MAKE) check

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
