# Phase 0 status

**Phase 0 gate: passed and merged to `main`.** [PR #2](https://github.com/nexuss0781/JSM/pull/2) was squash-merged at `16cefcac425954094e135fe89b4b4e9cc7542715` on 2026-10-06 18:21:07 UTC. [GitHub Actions run 37509059359](https://github.com/nexuss0781/JSM/actions/runs/37509059359) passed all five jobs: Ubuntu, macOS, Windows, Linux coverage/dependency policy, and benchmark smoke. The Phase 0 checklist and gate are closed in `TODO.md`; later phases remain open.

## Delivered

The workspace contains all 13 specified crates, one binary (`jsm`), the pinned Rust 1.95.0 toolchain, workspace-boundary enforcement, contributor/task/nextest tooling, CI, issue/PR templates, and ADR scaffolding. Workspace crates remain unpublished while the project license is undecided. `cargo-deny` checks third-party licenses and has a package-scoped `CDLA-Permissive-2.0` exception for `webpki-roots`; no project license is selected.

`jsm-core` supplies validated package names/IDs, versions, ranges, dist-tags, integrity/specifier/platform/ABI types, serialization tests, stable structured errors and exit statuses, an error-taxonomy snapshot, credential redaction, and a metrics interface. The CLI provides resolve/fetch/extract/write/link/build sample spans, Chrome trace export, `JSM_LOG`, verbosity controls, and redaction tests.

`jsm-testkit` supplies deterministic package fixtures and clock/RNG helpers, a loopback-only fake registry with latency/bandwidth/status/truncation/auth controls, and an install path that verifies integrity and rejects unsafe archive paths before atomic publication. Integration tests cover controlled failures, lifecycle-script non-execution, traversal rejection, and truncated content. `cargo fuzz list` discovers `package_name` and `archive_path`; this confirms target registration, not a fuzz-duration run.

## Benchmark evidence

The clean baseline has `working_tree_clean: true` and records **35 passing tool/scenario rows** (npm, pnpm, Yarn, Bun, and `jsm-stub` across seven small-fixture scenarios), with **three samples per row: 105 samples total**. The benchmark source revision is `13a81c5c42c7d852d97dfc675faf0ae3eaa53939`. Tool versions, fixture hash, environment, container tags/digests, and timing methodology are recorded in the [Markdown report](benchmarks/phase0-baseline/report.md), [JSON report](benchmarks/phase0-baseline/report.json), and [trend history](benchmarks/phase0-baseline/history.jsonl).

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

`just test` and `just coverage` each ran **21 passing tests**; coverage produced `coverage/lcov.info`. The Docker user mapping was checked in the generated command, and the six-row isolated Podman smoke passed. `cargo-deny` passed with duplicate-version warnings under the configured `multiple-versions = "warn"` policy. `cargo audit` exited successfully after scanning 205 locked dependencies. The Rust release build and warning-free rustdoc were also verified locally.

## Decisions intentionally left open

ADR 0001 remains **Proposed for review**. The project license and public conduct-reporting contact remain undecided; workspace crates must not be published until the license decision is resolved. Phase 0 uses provisional shared types and does not freeze lockfile, store, CLI JSON, or other public formats. These decisions do not block the internal Phase 0 engineering gate and must be resolved before the relevant formats or distribution policies are frozen.
