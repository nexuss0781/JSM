# Phase 0 status

**The implementation is ready for the refreshed review in [PR #2](https://github.com/nexuss0781/JSM/pull/2), but the Phase 0 gate remains open until the corrected GitHub Actions run passes and the change is merged.** The clean benchmark baseline is from benchmark-source commit `13a81c5c42c7d852d97dfc675faf0ae3eaa53939`. Per `TODO.md`, Phase 0 checklist entries remain unchecked until implementation, tests, documentation, and applicable platform/security evidence are merged.

## Delivered

The workspace contains all 13 specified crates, one binary (`jsm`), the pinned Rust 1.95.0 toolchain, workspace-boundary enforcement, contributor/task/nextest tooling, CI, issue/PR templates, and ADR scaffolding. Workspace crates remain unpublished while the project license is undecided. `cargo-deny` checks third-party licenses and has a package-scoped `CDLA-Permissive-2.0` exception for `webpki-roots`; no project license is selected.

`jsm-core` supplies validated package names/IDs, versions, ranges, dist-tags, integrity/specifier/platform/ABI types, serialization tests, stable structured errors and exit statuses, an error-taxonomy snapshot, credential redaction, and a metrics interface. The CLI provides resolve/fetch/extract/write/link/build sample spans, Chrome trace export, `JSM_LOG`, verbosity controls, and redaction tests.

`jsm-testkit` supplies deterministic package fixtures and clock/RNG helpers, a loopback-only fake registry with latency/bandwidth/status/truncation/auth controls, and an install path that verifies integrity and rejects unsafe archive paths before atomic publication. Integration tests cover controlled failures, lifecycle-script non-execution, traversal rejection, and truncated content. `cargo fuzz list` discovers `package_name` and `archive_path`; this confirms target registration, not a fuzz-duration run.

## Benchmark evidence

The clean baseline has `working_tree_clean: true` and records **35 passing tool/scenario rows** (npm, pnpm, Yarn, Bun, and `jsm-stub` across seven small-fixture scenarios), with **three samples per row: 105 samples total**. Tool versions, fixture hash, environment, container tags/digests, and timing methodology are recorded in the [Markdown report](benchmarks/phase0-baseline/report.md), [JSON report](benchmarks/phase0-baseline/report.json), and [trend history](benchmarks/phase0-baseline/history.jsonl).

Additional checks covered all six npm/`jsm-stub` smoke rows under a shaped profile of 25 ms request latency and 1 MiB/s, npm’s monorepo and native-package fixtures, plus the 35-row Podman matrix. The runner uses isolated Podman containers locally and Docker explicitly in CI; Docker runs as the host UID/GID so bind-mounted cache files remain removable. These are **harness/methodology checks, not JSM performance claims**; `jsm-stub` is deliberately not an installer.

## Local verification

On Linux with Rust 1.95.0, these checks passed:

```sh
just lint
just test
just coverage
cargo +1.95.0 build --workspace --all-targets --release --locked
cargo +1.95.0 deny check
cargo +1.95.0 audit
RUSTDOCFLAGS='-D warnings' cargo +1.95.0 doc --workspace --no-deps --all-features --locked
python scripts/check_workspace.py
python3 -m py_compile benches/run.py benches/jsm_stub.py scripts/check_workspace.py
```

`just test` and `just coverage` each ran **21 passing tests**; the latter produced `coverage/lcov.info`. The Docker user mapping was checked in the generated command, and the six-row isolated Podman smoke passed. `cargo-deny` passed with duplicate-version warnings under the configured `multiple-versions = "warn"` policy. `cargo audit` exited successfully after scanning 205 locked dependencies. The Rust release build and warning-free rustdoc were also verified locally.

## CI gate status

The first PR run passed the Ubuntu and macOS build/test/lint/doc matrices. It exposed three issues, all now corrected on the review branch: Windows converted the embedded snapshot to CRLF, Docker created root-owned files in the bind-mounted temporary cache, and coverage was asked to write `coverage/lcov.info` before that directory existed. The snapshot comparison now normalizes CRLF; Docker containers use the host UID/GID; and both CI and `just coverage` create the output directory. A fresh GitHub Actions run is required to verify these corrections, especially Windows tests, Docker benchmark smoke, and Linux coverage/dependency/audit checks.

ADR 0001 remains **Proposed for review**. The project license and public conduct-reporting contact remain undecided; workspace crates must not be published until the license decision is resolved. Keep the Phase 0 checklist open until the corrected checks pass and the reviewed changes are merged.
