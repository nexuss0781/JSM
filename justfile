set dotenv-load := false

# Build all crates, including all targets.
build:
    cargo build --workspace --all-targets --locked

# Run the repository test suite with nextest.
test:
    cargo nextest run --workspace --all-features --locked

# Validate formatting and lint every target.
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Run dependency license, advisory, and vulnerability checks.
audit:
    cargo deny check
    cargo audit

# Produce coverage using llvm-cov and nextest.
coverage:
    cargo llvm-cov nextest --workspace --all-features --locked --lcov --output-path coverage/lcov.info

# Run the quick npm + stub-jSM baseline harness.
bench:
    python3 benches/run.py --smoke

# Build the release binary and all libraries.
release:
    cargo build --workspace --all-targets --release --locked
