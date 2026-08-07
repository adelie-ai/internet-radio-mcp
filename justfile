set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# --- Local verification ("local CI") ---
# Run locally instead of GitHub Actions. `install-hooks` wires `check-all` into a
# git pre-push hook so it runs automatically before every push.

# The gate for the default feature set.
check: fmt-check lint build test
fmt-check:
    cargo fmt --check
fmt:
    cargo fmt
lint:
    cargo clippy --all-targets -- -D warnings
build:
    cargo build
test:
    cargo test
test-integration:
    cargo test -- --ignored

# The gate for the `otel` feature set. This crate ships two configurations
# (mcp-core#40), so both must pass before a push.
check-otel: lint-otel build-otel test-otel
lint-otel:
    cargo clippy --all-targets --features otel -- -D warnings
build-otel:
    cargo build --features otel
test-otel:
    cargo test --features otel

# Every configuration this crate ships in. This is what the pre-push hook runs.
check-all: check check-otel

premerge:
    git fetch origin
    git rebase origin/main
    just check-all
install-hooks:
    git config core.hooksPath .githooks
    @echo "pre-push hook active — bypass once with: git push --no-verify"
