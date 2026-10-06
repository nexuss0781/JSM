# Contributing to JSM

JSM is currently a draft specification with a Phase 0 foundation scaffold. Before implementing a feature, read `PROJECT.md`, `SPECS.md`, `PHASE.md`, and the matching section in `TODO.md`. Keep the sub-phase identifier in commits, tests, and pull requests.

## Toolchain and local setup

Install Rust 1.95.0 with `rustfmt` and `clippy`, Python 3, Node.js/npm for benchmark smoke tests, and `just`. The repository pins the Rust toolchain in `rust-toolchain.toml`; the stated MSRV policy is latest stable minus two releases. A lockfile is committed for reproducible dependency resolution.

Common commands:

| Task | Command |
|---|---|
| Build | `just build` |
| Tests | `just test` |
| Format and Clippy | `just lint` |
| Dependency policy | `just audit` |
| Coverage | `just coverage` |
| Benchmark smoke | `just bench` |

The equivalent commands are documented in `justfile`. `cargo nextest`, `cargo llvm-cov`, `cargo deny`, and `cargo audit` are installed by the Linux CI quality job. Fuzz targets live under `fuzz/` and are run with `cargo fuzz run <target>` when cargo-fuzz is installed.

## Workspace boundaries

`jsm-core` must not depend on another JSM crate. Cargo rejects dependency cycles, and `jsm-cli` is the only binary target. `scripts/check_workspace.py` enforces the Phase 0 dependency allowlist; adding an internal crate edge requires updating the policy in a reviewed change. The feature crates are intentionally empty scaffolds until their phase is reached.

## Pull requests

Include the relevant phase/sub-phase IDs, behavior and compatibility impact, tests and documentation, security considerations (especially credential handling and script execution), and platform evidence. Do not mark a TODO item complete until its implementation, automated verification, documentation, and applicable security/platform evidence are merged. Phase gates are not relaxed to fit a PR.

CI job names are `test (ubuntu-latest)`, `test (macos-latest)`, `test (windows-latest)`, `Linux coverage and dependency policy`, and `Benchmark harness smoke`. Repository administrators should require these checks on `main`; this commit adds workflow files but does not alter GitHub branch-protection settings.

## Design decisions

Phase 0 serde representations and core APIs are provisional. Do not use them as frozen lockfile/store/CLI formats. Record format, security-policy, compatibility, and release decisions in `docs/adr/` before implementation. The project license and several product decisions remain open; no license is implied by the dependency allowlist.
