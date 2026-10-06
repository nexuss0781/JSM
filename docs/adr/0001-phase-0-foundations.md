# ADR 0001: Phase 0 engineering foundations

- **Status:** Proposed for review; applies only to the internal Phase 0 scaffold
- **Date:** 2026-10-07
- **Related:** `PHASE.md` 0.1–0.5; `SPECS.md` 0.1–0.5

## Context

The repository contains product and engineering drafts but no implementation. Phase 0 requires a pinned toolchain, a workspace with the documented crate boundaries, shared typed contracts, hermetic test support, and benchmark tooling. The project explicitly leaves licensing, storage policy, database choice, linker defaults, and other product decisions open.

## Decision proposed

- Use the Cargo workspace and crate list specified in `PROJECT.md` / `SPECS.md`.
- Pin Rust 1.95.0 as the current latest-stable-minus-two baseline; keep the stated policy in `rust-toolchain.toml` and update the exact pin only with an explicit reviewed change.
- Keep Phase 0 serde representations and `jsm-core` APIs provisional. They support validation and testkit use only; they do not define a lockfile, store, public JSON, or stable external API format.
- Enforce `jsm-core` independence from other JSM crates, Cargo's cycle rejection, and `jsm-cli` as the sole binary target. Tighten individual dependency edges only when the architecture is ratified.
- Do not resolve the open project-license question by implication. `deny.toml` governs third-party dependency licenses only; no project license is asserted here.
- Keep workspace crates unpublished until the project-license decision is reviewed. `cargo-deny` skips license checks only for those unpublished workspace members, and its additional `CDLA-Permissive-2.0` allowance is scoped to the transitive `webpki-roots` crate.
- Keep any benchmark baseline labeled as a harness smoke/baseline, not as a product performance claim. Phase 0 results calibrate targets; they do not validate Phase 1 performance goals.

## Consequences

The initial implementation can be built and tested without freezing an on-disk format or resolving unrelated product questions. Before Phase 1 persists types or publishes a CLI/JSON contract, the relevant format and compatibility decisions require separate reviewed ADRs. The license question remains open, workspace crates cannot be published under the current policy, and no release may be made until it is resolved.

## Evidence to attach before acceptance

- CI results for Linux, macOS, and Windows
- Clean-checkout `cargo build`, test, format, lint, dependency, and advisory results
- Fake-registry end-to-end test output proving no public registry/network dependency
- Benchmark JSON/Markdown, fixture revision, versions, environment, and shaping mode
