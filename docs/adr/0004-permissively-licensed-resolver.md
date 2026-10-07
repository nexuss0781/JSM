# ADR 0004: Permissively licensed dependency solver

- **Status:** Accepted
- **Date:** 2026-10-07
- **Related:** `PHASE.md` §1.9; `SPECS.md` §§1.9, 3.9

## Context

The Phase 1 dependency audit rejected the previous resolver tree: `pubgrub` and `version-ranges` use MPL-2.0, and `priority-queue` uses LGPL-3.0. The repository's `cargo-deny` policy does not allow those licenses. The user chose to replace the dependencies rather than add license exceptions, while keeping npm-compatible resolution semantics and the current JSM resolver API.

## Decision

Replace PubGrub with [`resolvo` 0.12.1](https://crates.io/crates/resolvo), licensed BSD-3-Clause. Disable default features and use JSM's existing `Spec`/`matches_spec` logic to build exact candidate-version sets for ranges, exact versions, and dist-tags. This avoids enabling Resolvo's optional range integration and keeps npm semver semantics owned by `jsm-core`.

The JSM adapter preserves highest-compatible-version selection, the non-deprecated preference, deterministic tie-breaking, lazy parallel metadata retrieval, per-edge package instances, cycle handling, optional dependency behavior, and the public `Resolution`/`ResolveError` data shapes. Solver conflict text is mapped into JSM's structured conflict record; richer presentation remains in scope for `SPECS.md` §3.9.

`deny.toml` remains unchanged. No MPL/LGPL allowlist entry or crate-specific exception is added.

## Alternatives considered

1. **Allowlist the existing PubGrub dependencies.** Rejected by the user; it would weaken the stated license policy.
2. **Write and maintain a JSM-specific SAT/conflict solver.** Rejected because it would create a large correctness and maintenance burden when a maintained Rust solver can be adapted.
3. **Use Resolvo with JSM-owned version sets.** Selected: the solver supplies conflict-driven search while JSM retains control of registry metadata, npm range/tag matching, candidate ordering, and package-instance identity.

## Consequences

- The normal dependency graph no longer includes `pubgrub`, `version-ranges`, or `priority-queue`; there are no license-policy exceptions to review.
- Resolvo's provider adapter is an internal implementation detail; lockfile identity and the public resolver result types do not change.
- JSM remains responsible for testing range matching, deterministic ordering, conflict attribution, cycles, and provider errors independently of the solver backend.

## Evidence and review

- `cargo +1.95.0 deny check`: advisories, bans, licenses, and sources pass with the repository policy unchanged.
- `cargo +1.95.0 test -p jsm-resolver --locked`: 16/16 unit and property tests pass.
- `cargo +1.95.0 test --workspace --all-features --locked`: workspace tests pass, including the 25-test fake-registry CLI suite.
- `cargo +1.95.0 clippy --workspace --all-targets --all-features --locked -- -D warnings`: passes.
- `cargo +1.95.0 clippy --target x86_64-pc-windows-gnu -p jsm-linker -p jsm-resolver -p jsm-store --all-targets --all-features --locked -- -D warnings`: passes. Hosted runtime validation on Linux, macOS, and Windows is recorded in the Phase 1 Actions run before the bin-link checklist item is closed.
- API reference: [Resolvo `DependencyProvider`](https://docs.rs/resolvo/0.12.1/resolvo/trait.DependencyProvider.html).
