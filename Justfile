# Starlet task runner. `just` with no arguments lists everything.
#
# The toolchain comes from rust-toolchain.toml; nothing here names a version.

set shell := ["bash", "-uc"]

# Where `just seed` and `just demo` put their scratch database.
demo_db := "/tmp/starlet-demo.db"
# How many synthetic repositories `just seed` writes.
demo_repos := "5000"

_default:
    @just --list --unsorted

# ---------------------------------------------------------------- build

# Debug build of the whole workspace.
build:
    cargo build --workspace --locked

# Optimised build. This is what `just install` and the packagers use.
release:
    cargo build --workspace --locked --release

# Run the app against your real database.
run *ARGS:
    cargo run -p starlet-app -- {{ARGS}}

# Run the optimised binary against your real database.
run-release *ARGS:
    cargo run -p starlet-app --release -- {{ARGS}}

# Install the binary into ~/.cargo/bin.
install:
    cargo install --path crates/app --locked

# ---------------------------------------------------------------- demo

# Fill a throwaway database with synthetic stars.
seed count=demo_repos db=demo_db:
    cargo run -q -p starlet-store --example seed -- {{db}} {{count}}

# Seed, then launch against the throwaway database. No GitHub account needed.
demo count=demo_repos db=demo_db: (seed count db)
    STARLET_DB={{db}} cargo run -p starlet-app

# Delete the throwaway database and its WAL sidecars.
demo-clean db=demo_db:
    rm -f {{db}} {{db}}-shm {{db}}-wal

# ---------------------------------------------------------------- checks

# Everything CI runs, in the order CI runs it. Run this before pushing.
ci: fmt-check lint test build

# Type-check without producing binaries. The fastest useful feedback.
check:
    cargo check --workspace --all-targets --locked

# Clippy, warnings denied.
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Apply rustfmt.
fmt:
    cargo fmt --all

# Fail if anything is unformatted.
fmt-check:
    cargo fmt --all --check

# Apply the clippy fixes that can be applied mechanically.
fix:
    cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

# ---------------------------------------------------------------- tests

# The whole suite.
test:
    cargo test --workspace --locked

# One crate: `just test-crate core`.
test-crate crate:
    cargo test -p starlet-{{crate}} --locked

# Tests matching a name: `just test-one escape`.
test-one filter:
    cargo test --workspace --locked -- {{filter}} --nocapture

# The GPUI interaction and overlay tests. Headless; no display required.
test-ui:
    cargo test -p starlet-ui --test workspace --test overlays --locked

# The wall-clock budgets, with the measured numbers printed.
bench:
    cargo test --release -p starlet-core --test ranking_performance -- --nocapture
    cargo test --release -p starlet-store --test search_performance -- --nocapture

# ---------------------------------------------------------------- docs

# Build and open the API docs for the workspace crates.
docs:
    cargo doc --workspace --no-deps --open

# ---------------------------------------------------------------- upkeep

# Remove build artifacts.
clean:
    cargo clean

# Show the dependency tree for one crate: `just tree gpui`.
tree crate:
    cargo tree -i {{crate}}

# Report outdated dependencies. Requires cargo-outdated.
outdated:
    cargo outdated --workspace --root-deps-only

# Audit dependencies for advisories. Requires cargo-audit.
audit:
    cargo audit
