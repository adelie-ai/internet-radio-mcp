set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# --- Local verification ("local CI") ---
# Run locally instead of GitHub Actions. `install-hooks` wires `check` into a
# git pre-push hook so it runs automatically before every push.
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

# The second required gate configuration: OTLP export compiled in (still off
# at runtime unless OTEL_* variables are set). `check` alone never builds this
# path, so a change that only compiles with `otel` off would pass silently.
check-otel: fmt-check
    cargo clippy --all-targets --features otel -- -D warnings
    cargo build --features otel
    cargo test --features otel
premerge:
    git fetch origin
    git rebase origin/main
    just check
install-hooks:
    git config core.hooksPath .githooks
    @echo "pre-push hook active — bypass once with: git push --no-verify"
