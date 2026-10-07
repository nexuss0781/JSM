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
    python3 -c "from pathlib import Path; Path('coverage').mkdir(parents=True, exist_ok=True)"
    cargo llvm-cov nextest --workspace --all-features --locked --lcov --output-path coverage/lcov.info

# Run the quick npm + stub-jSM baseline harness.
bench:
    python3 benches/run.py --smoke

# Test the Phase 1 readiness and benchmark harnesses.
phase1-harness-test:
    python3 scripts/phase1_gate.py --self-test
    python3 -m unittest benches.test_run -v

# Verify the Phase 1 gate and write docs/phase-1-status.{json,md}.
phase1-verify:
    python3 scripts/phase1_gate.py --verify

# Compare the release JSM binary with npm and pnpm on the same local fixture.
phase1-bench:
    cargo build --release -p jsm-cli --locked
    python3 benches/run.py --phase1 --jsm-binary target/release/jsm --container-runtime none --repeat 3

# Build the release binary and all libraries.
release:
    cargo build --workspace --all-targets --release --locked
