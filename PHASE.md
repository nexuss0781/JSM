# JSM Engineering Phases

> **Status:** Engineering execution plan, derived from `PROJECT.md` and `SPECS.md` (both draft v0.1).
> **Product:** `jsm`, a Rust JavaScript package manager with a shared, multi-version content-addressed store.
> **Scope:** Phases 0–5 define the path to v1.0; Phases 6–7 are post-v1.0, opt-in expansion.
> **Normative detail:** `SPECS.md` supplies the detailed requirements and exit criteria. This file defines sequencing and release gates; `TODO.md` is the live implementation checklist.

## How to use this plan

- Keep the source sub-phase identifiers in `SPECS.md` stable. Reference them in issues, pull requests, commits, tests, and decision records (for example, `2.5` for cross-process concurrency).
- A checklist item is complete only when its implementation, automated verification, documentation, and applicable platform/security evidence are present. A passing unit test alone does not close a sub-phase.
- Close a phase only after **every** sub-phase exit criterion in `SPECS.md` and the phase gate below passes. Record deviations and evidence in the pull request or an ADR under `docs/adr/`.
- Preserve the stated security and compatibility defaults. In particular, fetched bytes are verified before becoming project-visible, dependency lifecycle scripts remain disabled until explicitly approved, and the store is never silently reduced by an install or version switch.
- Treat performance numbers in `SPECS.md` Appendix D as initial targets: establish a reproducible Phase 0 baseline, document any calibration, then enforce ratified budgets as CI gates. Do not claim a benchmark win without publishing its tool versions, fixture, environment, and method.
- Before committing to a persisted format, public CLI/JSON contract, or security policy, resolve the relevant open question in `PROJECT.md` and record the decision in an ADR. Do not silently choose between draft alternatives.

## Delivery sequence

| Phase | Outcome | Depends on | Release boundary |
|---|---|---|---|
| 0 — Foundations | Reproducible workspace, core contracts, deterministic tests and benchmark baseline | None | Internal engineering gate |
| 1 — Core Engine | Correct registry install flow backed by the shared store and lockfile | Phase 0 gate | Walking skeleton; not yet production-ready |
| 2 — Store Management and Safety | Inspectable store, safe GC, cross-process operations, recovery and diagnostics | Phase 1 gate | Store-management gate |
| 3 — Performance and Time Travel | Streaming scheduling, measured fast paths, history and version switching | Phase 2 gate | Performance/differentiator gate |
| 4 — Real-World Compatibility | Peer/platform/protocol behavior, secure scripts/builds, compatibility evidence | Phase 3 gate | Ecosystem-compatibility gate |
| 5 — Production Readiness | Workspaces, migration, supply-chain security, release and user documentation | Phase 4 gate | **v1.0 gate** |
| 6 — Ecosystem and Scale | Secure team/CI sharing and optional background/analysis features | Phase 5 gate | Post-v1.0; separately enabled capabilities |
| 7 — Advanced Extensions | Chunk store, TUI, runtime management, virtual linker and plugins | Phase 5 gate; individual prerequisites below | Post-v1.0; opt-in extensions |

A phase may be developed in smaller reviewable increments, but later phases do not waive earlier gates. Work on Phases 6–7 must not destabilize the v1.0 core; each extension remains optional and must retain a tested fallback where specified.

## Phase 0 — Foundations

**Objective:** establish the engineering substrate and measurable contracts before building package-management behavior.

**Work packages:**

- **0.1 Repository and tooling:** Cargo workspace and crate boundaries; pinned Rust toolchain and MSRV policy; formatting, lint, dependency/license and advisory checks; test, coverage, task, contributor, and ADR scaffolding.
- **0.2 Core types and errors:** validated package/version/range/integrity/specifier/platform types; stable error codes and structured causes; serialization tests for persisted types.
- **0.3 Observability:** structured tracing and metrics across install stages, trace export, CLI verbosity, and credential redaction.
- **0.4 Test infrastructure:** hermetic fake registry, controllable package fixtures and failure modes, temp store/project helpers, deterministic clock/RNG, property-test and fuzz harnesses.
- **0.5 Benchmark skeleton:** pinned fixture sizes and benchmark scenarios, competitor-version capture, network shaping, machine-readable and human-readable reports.

**Gate:** clean checkout builds, tests, and passes required checks on Linux, macOS, and Windows; the fake-registry end-to-end path works without the public network; the benchmark harness produces a reproducible baseline report. Record Tier 1 platform coverage and known filesystem limitations.

## Phase 1 — Core Engine

**Objective:** deliver the smallest end-to-end usable `jsm` while preserving the shared-store, reproducibility, and safe-by-default design.

**Work packages:**

- **1.1–1.3 CLI, configuration, and manifests:** stable command/flag behavior, layered configuration and `.npmrc` compatibility, format-preserving `package.json` edits.
- **1.4–1.5 Semver and registry:** npm-compatible ranges and prerelease behavior; resilient, cached npm-compatible metadata and tarball access.
- **1.6–1.8 Store and fetching:** versioned CAS, validated package manifests, atomic publication, bounded streaming fetch/extract, integrity checks, and resource limits.
- **1.9–1.10 Resolution and lockfile:** deterministic PubGrub resolution and stable lockfile serialization/staleness rules.
- **1.11–1.12 Linking and bins:** isolated linker, incremental materialization, executable shims, and safe cross-platform links.
- **1.13–1.14 Commands and security baseline:** `init`, `add`, `install`, `remove`, `run`, `exec`, basic inspection, integrity enforcement, and a Phase 1 binary with no dependency-script execution path.

**Gate:** all Phase 1 exit criteria pass; fixture-based end-to-end tests cover each baseline command and failure path; the real-registry top-100 acceptance set is exercised; no unverified content or implicit dependency script execution reaches a project; publish the first comparable benchmark against npm and pnpm. Any unsupported platform behavior is explicit and tested, not silently degraded.

**Acceptance tooling:** [`docs/phase1-harness.md`](docs/phase1-harness.md) documents the readiness auditor, replayable npm package corpus, and real-binary benchmark command. Harness self-tests are tooling checks, not evidence that unfinished product requirements pass.

## Phase 2 — Store Management and Safety

**Objective:** make the shared store safe to operate, inspect, clean, and recover across projects and processes.

**Work packages:**

- **2.1–2.2 References and store CLI:** transactional project/package references and complete local version inspection and management.
- **2.3–2.4 GC, pinning, verification, and repair:** reference-aware eviction, blob mark-and-sweep under a maintenance lease, corruption detection, quarantine, and re-fetch behavior.
- **2.5–2.6 Concurrency and transactions:** documented lock hierarchy, cross-process single-flight, staged project updates, install journals, and crash recovery.
- **2.7–2.8 Automation and diagnostics:** versioned JSON/NDJSON contracts, completions, help, and actionable `doctor` checks with safe repair/reporting.
- **2.9 Lockfile merge:** deterministic merge/verify operations and tested Git merge-driver setup.

**Gate:** all Phase 2 exit criteria pass; reference accounting matches ground truth; 50-process overlapping-install stress completes without corruption and fetches each identical tarball once; crash injection at every defined transaction boundary recovers to a valid prior or committed state; GC cannot invalidate an in-flight install or live project; `doctor` checks have positive and negative fault-injection coverage.

## Phase 3 — Performance and Time Travel

**Objective:** make the performance and multi-version differentiators measurable, repeatable, and safe.

**Work packages:**

- **3.1–3.3 Pipeline and write avoidance:** bounded streaming DAG scheduler, per-host adaptive concurrency/hedging, and indexed skip-write extraction.
- **3.4–3.5 Materialization and platform I/O:** validated cache keys/invalidation, directory cloning, and capability-probed platform-specific I/O with portable fallback.
- **3.6 Resolution policy:** store-preferred resolution without violating ranges or lockfile pins; explicitly defined offline/update interactions.
- **3.7–3.8 Time travel and compatibility:** transactional history, undo/redo, snapshots, `switch`, plus deterministic hoisted linking.
- **3.9 Explainable conflicts:** readable resolver derivations, structured conflict output, and reviewed suggestions.

**Gate:** correctness and determinism tests pass; no-op, branch-switch, and materialization operations preserve project/store invariants; Phase 3's measured medium-fixture pipeline improvement meets its specified minimum; calibrated Appendix D budgets are met for cold, warm-store, and cache-hit scenarios or have an approved, evidence-backed recalibration. Undo/redo/snapshot sequences restore byte-identical lockfiles and valid trees.

## Phase 4 — Real-World Compatibility

**Objective:** handle common npm graph and platform behavior and provide a usable, secure script/build model.

**Work packages:**

- **4.1–4.3 Resolution compatibility:** peer contexts, optional/platform dependencies, engine policy, aliases and local/git/tarball dependency protocols.
- **4.4–4.6 Scripts and builds:** default-deny dependency scripts, content-bound approvals, local deterministic build cache, bounded parallel build scheduler, cancellation and rollback.
- **4.7–4.9 Package insight and policy:** `diff`, `why`, `dupes`, `unused`, `outdated`, cached package execution, release cooldown, lowest-version and time-based resolution.
- **4.10–4.11 Evidence and enterprise networking:** reproducible compatibility corpus/dashboard and authenticated registry, proxy, CA, mirror, and fallback behavior.

**Gate:** all Phase 4 exit criteria pass; peer and optional dependency reference suites pass; with deny policy no dependency lifecycle script executes; build cache keys and results are verified; compatibility Corpus A reaches at least 95% at this gate; authenticated fake-registry, proxy, and custom-CA tests pass. Compatibility exceptions are classified and tracked.

## Phase 5 — Production Readiness (v1.0)

**Objective:** complete adoption, supply-chain, release, and documentation requirements needed for a production recommendation.

**Work packages:**

- **5.1–5.3 Monorepos and local governance:** workspace discovery/filtering and script orchestration, catalogs/overrides, and non-mutating patches.
- **5.4 Migration:** supported npm/Yarn/pnpm/Bun lockfile importers and dry-run migration/export workflows.
- **5.5–5.6 Supply-chain visibility:** registry signature/provenance verification and offline-capable vulnerability audit/advisory policy.
- **5.7–5.9 Script security and native packages:** platform-specific sandboxing, prebuilt binary resolution, and script-change approval revocation.
- **5.10–5.12 Safe operations:** update review, portable store bundles, and production slimming/deploy output.
- **5.13–5.14 Release and docs:** reproducible signed distributions, format migrations, security disclosure process, complete user/contributor docs, and validated examples.

**v1.0 gate:** all Phase 5 exit criteria pass; compatibility Corpus A reaches at least 98%; ratified performance budgets pass; security review and required fuzzing are complete; crash, concurrency, and platform suites meet their release targets; a tagged release candidate is built by CI with signed artifacts, checksums, and SBOM; migration and documentation checks pass. Do not label earlier phases production-ready or enable post-v1.0 experiments by default.

## Phase 6 — Ecosystem and Scale (post-v1.0)

**Objective:** enable safe sharing across teams and CI without making network services or a daemon mandatory.

**Work packages:**

- **6.1–6.4 Shared infrastructure:** registry-compatible `jsm serve`, authenticated and signed remote build cache, sparse metadata index with fallback, Merkle verification and store synchronization.
- **6.5 Optional daemon:** user-scoped local protocol, resource limits, opt-in startup, and parity with daemon-free CLI behavior.
- **6.6–6.7 Supply-chain signals:** cached capability analysis and configurable typosquat/anomaly warnings with measured false-positive rates.
- **6.8–6.9 Ephemeral execution and deploy:** inline-dependency script environments and deterministic container/deploy/SBOM workflows.

**Gate:** every Phase 6 sub-phase acceptance test passes; serve, remote cache, sparse index, and daemon have documented threat models and security integration coverage; tampered or unauthorized remote content is rejected; every service can be disabled without affecting core install correctness. Publish performance and compatibility results before recommending team-wide rollout.

## Phase 7 — Advanced Extensions (post-v1.0)

**Objective:** pursue high-complexity differentiators only on top of stable formats and interfaces.

**Work packages:**

- **7.1 Chunk store:** resumable whole-file-to-chunk migration, compression, integrity, rollback, and materialization performance.
- **7.2 TUI:** accessible, keyboard-operable store, update, advisory, and history views with tested non-TTY fallback.
- **7.3 Runtime management:** verified runtime downloads, pinning/switching, and ABI-aware build keys.
- **7.4 Virtual linker:** opt-in overlay implementation, explicit platform matrix, compatibility tests, and automatic fallback to the isolated linker.
- **7.5 Editor/tooling integrations:** versioned schemas, diagnostics, editor extension, CI action, and ecosystem integration guidance.
- **7.6 Extension API:** capability-limited plugin contract, signatures, reference plugin, and enforcement that plugins cannot bypass integrity, script approval, or sandbox policy.

**Gate:** each extension has its own feature flag or opt-in, documented compatibility and security model, acceptance suite, performance evidence, and rollback/fallback plan. No extension may change default behavior or on-disk formats without a reviewed ADR and migration plan.

## Cross-phase release invariants

These requirements apply at every phase gate, not only when a named feature is implemented:

1. **Integrity:** verify registry tarball integrity and stored blob hashes before content becomes visible; reject unsafe archive paths and symlink escapes.
2. **Store safety:** content-addressed payloads are immutable; package manifests are the package commit point; writes are atomic and idempotent; installs do not implicitly delete retained versions.
3. **Script safety:** dependency scripts do not run without explicit approval; approval is tied to package identity/range and script content; later sandboxing does not weaken this rule.
4. **Determinism:** identical declared inputs produce deterministic resolution and lockfile bytes, regardless of network timing or task scheduling.
5. **Recoverability:** failure or cancellation preserves the last valid project state; cleanup and repair never assume ownership of another process's in-flight data.
6. **Secrets:** credentials never appear in logs, lockfiles, package/store metadata, or diagnostic bundles; diagnostics are redacted by construction.
7. **Compatibility and portability:** capability probes select optimizations; portable fallbacks remain correct; platform/filesystem limitations are surfaced explicitly.
8. **Public contracts:** CLI exit codes, JSON schemas, lockfile/store formats, and crate APIs are versioned and covered by compatibility and migration tests.
