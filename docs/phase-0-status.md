# Phase 0 status

**Implementation is on `phase0-foundations` at source commit `fbbd8c58e0badd0381faab3c3306ecc478669488`. The Phase 0 gate remains open** until the pull-request checks pass on Linux, macOS, and Windows and the reviewed work is merged. The feature crates are foundations, not a usable package manager; provisional core types do not freeze a lockfile, store, or public CLI format. Per `TODO.md`, checklist items remain unchecked until implementation, verification, documentation, and applicable evidence are merged.

## Delivered

The workspace contains all 13 specified crates, one binary (`jsm`), a pinned Rust 1.95.0 toolchain, workspace-boundary enforcement, contributor/task/nextest tooling, CI, issue/PR templates, and ADR scaffolding. Workspace crates remain unpublished while the project license is undecided. `cargo-deny` checks third-party licenses and uses a package-scoped `CDLA-Permissive-2.0` exception for `webpki-roots`; no project license is selected.

`jsm-core` supplies validated package/version/range/integrity/specifier/platform/ABI types, serialization tests, stable structured errors and exit statuses, an error-taxonomy snapshot, credential redaction, and a metrics interface. The CLI provides stage tracing, Chrome trace export, `JSM_LOG`, verbosity controls, and redaction tests.

`jsm-testkit` supplies deterministic package fixtures and clock/RNG helpers, a loopback-only fake registry with latency/bandwidth/status/truncation/auth controls, and an install path that verifies integrity and rejects unsafe archive paths before atomic publication. Integration tests cover controlled failures, lifecycle-script non-execution, traversal rejection, and truncated content. `cargo fuzz list` discovers `package_name` and `archive_path`; this confirms target registration, not a fuzz-duration run.

## Benchmark evidence

The clean baseline is from source commit `fbbd8c58e0badd0381faab3c3306ecc478669488`, with `working_tree_clean: true`. It contains **35 passing tool/scenario rows** (npm, pnpm, Yarn, Bun, and `jsm-stub` across seven small-fixture scenarios), with **three samples per row: 105 samples total**. Tool versions, fixture hash, environment, container tags/digests, and timing methodology are recorded in the [Markdown report](benchmarks/phase0-baseline/report.md), [JSON report](benchmarks/phase0-baseline/report.json), and [trend history](benchmarks/phase0-baseline/history.jsonl).

Additional local checks passed: all six npm/`jsm-stub` smoke rows under a shaped profile of 25 ms request latency and 1 MiB/s; npm’s monorepo fixture; and npm’s native-package fixture. The runner uses isolated Podman containers locally and uses Docker explicitly in CI. These are **harness/methodology checks, not JSM performance claims**; `jsm-stub` is deliberately not an installer.

## Local verification

On Linux with Rust 1.95.0, the following passed:

```sh
cargo +1.95.0 fmt --all -- --check
cargo +1.95.0 build --workspace --all-targets --locked
cargo +1.95.0 build --workspace --all-targets --release --locked
cargo +1.95.0 clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +1.95.0 test --workspace --all-targets --all-features --locked
cargo +1.95.0 nextest run --workspace --all-features --locked
cargo +1.95.0 llvm-cov nextest --workspace --all-features --locked --lcov --output-path /tmp/jsm-phase0-coverage/lcov.info
cargo +1.95.0 deny check
cargo +1.95.0 audit
RUSTDOCFLAGS='-D warnings' cargo +1.95.0 doc --workspace --no-deps --all-features --locked
python scripts/check_workspace.py
python3 -m py_compile benches/run.py benches/jsm_stub.py scripts/check_workspace.py
```

The workspace test and nextest runs each passed **21 tests**; LCOV was generated. `cargo-deny` passed with duplicate-version warnings under the configured `multiple-versions = "warn"` policy. `cargo audit` exited successfully after scanning 205 locked dependencies. The fake-registry integration tests, benchmark reports, fuzz-target discovery, and `just --list` were also verified.

## Still needed to close the gate

The pull-request workflow is configured to run the build/test/lint/doc matrix on Linux, macOS, and Windows, plus Linux coverage, dependency-license/advisory, audit, and Docker benchmark-smoke jobs. Those GitHub Actions results must be reviewed before closing the gate. Branch protection was not changed.

ADR 0001 remains **Proposed for review**. The project license and public conduct-reporting contact remain undecided; workspace crates must not be published until the license decision is resolved. Keep the Phase 0 checklist open until the reviewed changes are merged and the applicable CI/platform/security evidence is attached.
