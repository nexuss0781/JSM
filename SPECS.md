# jsm Engineering Specifications

> **Document type:** Engineering specification
> **Product:** `jsm`, a JavaScript package manager with a shared multi-version store (Rust)
> **Version:** 0.1 (draft)
> **Companion document:** `Project.md` (vision, positioning, feature catalog)

---

## 0. Document Conventions

### 0.1 Requirement Language

- **MUST / MUST NOT**: mandatory requirement; a release is blocked if unmet.
- **SHOULD / SHOULD NOT**: strong recommendation; deviations need a documented reason.
- **MAY**: optional behavior.

### 0.2 Structure and Traceability

- The work is divided into **Phases** (0 to 7), each containing **Sub-phases** (for example `1.6`).
- Each sub-phase has an **Objective**, a **Scope** list (bullets), and **Exit Criteria** (bullets).
- Requirements are traced by sub-phase ID (for example "see 2.5") in issues, PRs, and tests.
- Each phase ends with a **Phase Gate**: all exit criteria of all sub-phases must pass before the phase is closed.

### 0.3 Phase Overview

| Phase | Name | Outcome |
|---|---|---|
| 0 | Foundations | Repo, CI, core types, test and benchmark scaffolding |
| 1 | Core Engine | Working `add` / `install` with store, lockfile, linked `node_modules` |
| 2 | Store Management and Safety | Store CLI, GC, locking, crash safety, doctor |
| 3 | Performance and Time Travel | Pipelining, adaptive concurrency, materialization cache, history |
| 4 | Real-World Compatibility | Peers, optionals, protocols, scripts policy, build cache, corpus |
| 5 | Production Readiness | Workspaces, importers, security, sandbox, release engineering (v1.0) |
| 6 | Ecosystem and Scale | Serve, remote cache, sparse index, daemon, zero-install |
| 7 | Advanced Extensions | Chunk store, TUI, runtime manager, virtual linker, editor tooling |

### 0.4 Global Definitions

- **Store**: machine-wide content-addressed directory holding package payloads and metadata.
- **Project**: a directory with a `package.json`.
- **Materialization**: creating a project's `node_modules` from store contents.
- **Package identity**: `name@version@integrity` (sha512 SRI).
- **Build key**: hash identifying a compiled variant of a package for an environment.

---

# PHASE 0: Foundations

**Goal:** establish the engineering substrate so that all later phases are measurable, testable, and releasable.

## 0.1 Repository and Tooling

**Objective:** a reproducible, lint-clean, multi-crate workspace.

- Create a Cargo workspace with crates: `jsm-cli`, `jsm-core`, `jsm-registry`, `jsm-resolver`, `jsm-store`, `jsm-fetch`, `jsm-linker`, `jsm-build`, `jsm-lockfile`, `jsm-workspace`, `jsm-security`, `jsm-daemon`, `jsm-testkit`.
- Pin the Rust toolchain via `rust-toolchain.toml`; define the MSRV policy (latest stable minus two releases).
- Enforce `rustfmt`, `clippy -D warnings`, `cargo deny` (licenses, advisories), and `cargo audit` in CI.
- Define crate dependency rules: `jsm-core` depends on no other jsm crate; no cyclic dependencies; `jsm-cli` is the only binary crate.
- Configure `cargo-nextest` for tests and `cargo-llvm-cov` for coverage.
- Provide `justfile` or `xtask` for common tasks (build, test, bench, lint, release).
- Add `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, issue and PR templates, and a decision log directory (`docs/adr/`).

**Exit criteria**
- Clean checkout builds and tests on Linux, macOS, and Windows in CI.
- Lint, format, and license checks gate every PR.

## 0.2 Core Types and Error Model

**Objective:** a single vocabulary of types used across crates.

- Define `PackageName` (validated, scoped-aware), `Version`, `Range`, `Integrity` (SRI sha512), `PackageId`, `DistTag`, `Spec` (all dependency specifier kinds).
- Define `Platform` (`os`, `cpu`, `libc`) and `NodeAbi` types.
- Use `thiserror` for library errors and `miette` for user-facing diagnostics in the CLI crate only.
- Define a stable error code taxonomy (`JSM_E_RESOLVE`, `JSM_E_NETWORK`, `JSM_E_INTEGRITY`, and so on) mapped to process exit codes (see Appendix C).
- Every error MUST carry: code, human message, cause chain, and an optional `help` hint.
- All public types MUST implement `Debug`, `Clone` (where cheap), and `serde` where persisted.

**Exit criteria**
- Round-trip tests for every persisted type.
- Error codes documented and covered by snapshot tests.

## 0.3 Observability

**Objective:** make performance and failures inspectable from the first commit.

- Use `tracing` with structured spans for each pipeline stage (resolve, fetch, extract, write, link, build).
- Support `JSM_LOG=<filter>` and `--verbose`/`--quiet`.
- Provide `--trace <file>` producing a Chrome-trace compatible timeline.
- Provide a metrics interface (counters, histograms) used by benchmarks and the concurrency controller.
- Logging MUST NOT include credentials or tokens; apply redaction at the logging layer.

**Exit criteria**
- A sample install produces a valid trace viewable in a trace viewer.
- Redaction tests pass.

## 0.4 Test Infrastructure

**Objective:** deterministic, hermetic tests.

- Build `jsm-testkit` containing a **fake registry server** (in-process HTTP) serving fixture packages with controllable latency, errors, truncation, and bandwidth limits.
- Provide a **fixture package generator** (create tarballs with specified files, dependencies, scripts, and integrity).
- Provide helpers for temp stores and temp projects, filesystem snapshot assertions, and CLI end-to-end harness (`assert_cmd`).
- Adopt `proptest` for property tests and `cargo-fuzz` targets scaffold.
- Provide a deterministic clock and RNG abstraction for tests.

**Exit criteria**
- An end-to-end test installs a fixture package from the fake registry without network access.

## 0.5 Benchmark Skeleton

**Objective:** performance regression detection from day one.

- Define benchmark scenarios (cold, warm store, warm lockfile, re-install, offline, branch switch, monorepo, native).
- Create pinned fixture projects (small: ~10 deps, medium: ~300, large: ~1,500, monorepo: 50 packages).
- Build a harness that runs npm, pnpm, Yarn, Bun, and jsm in isolated containers with recorded tool versions.
- Output results as JSON and a markdown table; store history for trend comparison.
- Provide network shaping (latency and bandwidth profiles).

**Exit criteria**
- Harness runs end to end for at least npm and a stub jsm command and produces a report.

**Phase 0 Gate:** CI green on three OSes; fake registry and benchmark harness operational.

---

# PHASE 1: Core Engine

**Goal:** a correct, reasonably fast `init`, `add`, `install`, `remove` for registry packages, backed by the shared store.

## 1.1 CLI Skeleton

**Objective:** command framework with consistent behavior.

- Implement with `clap` (derive): global flags `--cwd`, `--json`, `--quiet`, `--verbose`, `--offline`, `--store-dir`, `--registry`, `--no-color`, `--config`.
- Define command aliases (`i`, `rm`, `up`, `ls`).
- Detect CI via `CI` / `JSM_CI` and switch to non-interactive output and `--frozen-lockfile=auto`.
- Provide uniform progress and summary rendering (`indicatif`) with a non-TTY fallback.
- Show typo suggestions for unknown commands.
- Handle SIGINT/SIGTERM gracefully: cancel tasks, clean temp data, exit with a defined code.

**Exit criteria**
- Help output snapshot tests; signal handling test leaves no partial state.

## 1.2 Configuration System

**Objective:** layered, predictable configuration.

- Precedence: CLI flags, environment (`JSM_*`), project `jsm.toml`/`.jsmrc`, workspace root, user config, defaults.
- Parse `.npmrc` (registry, scoped registries, auth tokens, proxy, `strict-ssl`, `cafile`) at all levels with documented precedence.
- Support environment variable expansion (`${VAR}`) in config values.
- Validate configuration with precise error locations (file, line, key).
- Provide `jsm config get|set|list|delete` with `--global`/`--project` scopes.
- Publish a JSON Schema for `jsm.toml`.

**Exit criteria**
- Precedence matrix tests; invalid-config diagnostics snapshot tests.

## 1.3 Manifest Handling

**Objective:** correct parsing and editing of `package.json`.

- Parse all dependency fields: `dependencies`, `devDependencies`, `peerDependencies`, `peerDependenciesMeta`, `optionalDependencies`, `bundledDependencies`.
- Parse `scripts`, `bin`, `engines`, `os`, `cpu`, `libc`, `workspaces`, `overrides`, `resolutions`, `exports`, `main`, `files`.
- Preserve formatting, key order, indentation, and trailing newline when editing (`add`, `remove`).
- Support `--save-exact`, `--save-prefix`, and dependency-type flags.
- Reject or warn on invalid package names and malformed ranges with clear diagnostics.

**Exit criteria**
- Edit round-trip tests produce minimal diffs on a corpus of real manifests.

## 1.4 Semver Engine

**Objective:** a npm-compatible semver implementation.

- Support full range grammar: `^`, `~`, comparators, hyphen ranges, `x`/`*` ranges, `||` unions, prerelease and build metadata.
- Implement npm's prerelease inclusion rules (prerelease versions only match when the comparator has the same `[major, minor, patch]` tuple with a prerelease tag).
- Provide range intersection, subset, and simplification (needed by the resolver).
- Differential-test against the reference `semver` npm package over a large generated corpus.
- Fuzz the parser for panics and pathological inputs (regex-like blowups).

**Exit criteria**
- 100% agreement with the reference implementation on the corpus; no fuzz crashes in a 24-hour run.

## 1.5 Registry Client

**Objective:** efficient, resilient access to npm-compatible registries.

- HTTP client on `reqwest` with `rustls`, HTTP/2, connection pooling, and keep-alive.
- Request abbreviated metadata (`application/vnd.npm.install-v1+json`) with fallback to full packuments.
- Persist metadata cache with ETag and `Last-Modified` revalidation; honor `--offline` and `--prefer-offline`.
- Support scoped registries and bearer/basic auth from config.
- Implement retries with jittered exponential backoff for 5xx, timeouts, and connection resets; do not retry 4xx except 408/429 (honor `Retry-After`).
- Enforce request and total timeouts; support proxy settings (`HTTP(S)_PROXY`, `NO_PROXY`) and custom CA bundles.
- Validate and sanitize all registry-provided strings (names, versions, URLs) before use as paths.
- Abstract behind a `Registry` trait to allow alternative backends (sparse index, local store, mirror).

**Exit criteria**
- Tests against the fake registry for: 304 handling, flaky 5xx, slow responses, truncated bodies, auth failure.

## 1.6 Store: Content-Addressed Storage

**Objective:** the durable, shared file store.

- Layout: `files/sha512/<2-hex>/<rest>` for blobs; executable bit recorded in package manifests, not in blob path.
- Writes MUST be atomic: write to `tmp/` then `rename`; content verified against its hash before publish.
- Writes MUST be idempotent: if the blob exists, skip.
- Support store format versioning (`VERSION` file) with forward-compatible migration hooks.
- Use read-only permissions on blobs to reduce accidental mutation.
- Resolve default store location per platform; allow overrides via flag, env, and config.
- Detect per-volume placement: when the project and store are on different filesystems, create or select a per-volume store; record in config.
- Provide a `Store` API: `has_blob`, `put_blob_stream`, `get_blob_path`, `has_package`, `put_package_manifest`, `get_package_manifest`.

**Exit criteria**
- Concurrent writer tests (multi-thread) produce no corruption; killed-writer tests leave no partial blob visible.

## 1.7 Store: Package Manifests

**Objective:** map `name@version@integrity` to its file set.

- Layout: `packages/<name>/<version>/<integrity>.manifest`.
- Manifest records: relative path, blob hash, mode (executable flag), size, symlink targets (validated), and package `package.json` summary.
- Reject unsafe entries: absolute paths, `..` traversal, device files, symlinks escaping the package root, and duplicate paths differing only by case on case-insensitive targets (record collision metadata).
- Normalize the `package/` prefix in tarballs and handle non-standard roots.
- Manifest write is the **commit point**: a package is considered present only when its manifest exists.
- Provide a fast read path (memory-mapped or compact binary format) for large manifests.

**Exit criteria**
- Malicious tarball suite (traversal, symlink escape, absolute paths, bombs) is rejected safely.

## 1.8 Fetch and Extract Pipeline (Baseline)

**Objective:** stream-based download to store without temp tarballs.

- Pipeline stages: HTTP body, gunzip, tar parse, per-entry hash, blob write, manifest commit.
- Verify tarball integrity (sha512 from registry) over the compressed stream; fail and discard on mismatch.
- Enforce limits: maximum entry count, maximum uncompressed size, maximum path length, compression ratio guard.
- Bounded buffers and channels; memory per in-flight package MUST have a documented upper bound.
- Support resumable downloads (HTTP Range) where supported by the server.
- Emit per-package progress events consumed by the UI.

**Exit criteria**
- Peak memory stays under the documented bound on a 2,000-package install; integrity mismatch test fails closed.

## 1.9 Resolver (Baseline)

**Objective:** correct dependency resolution.

- Use a deterministic conflict-driven solver (current implementation: Resolvo) with package versions ordered per the resolution mode (default highest).
- Support dist-tags, ranges, and exact versions; honor `deprecated` as a soft penalty.
- Handle dependency cycles safely.
- Fetch metadata lazily and in parallel as the solver requests it (prefetching dependencies of candidates).
- Produce a deterministic resolution graph independent of network timing.
- Surface failures as structured conflict details (rendering improved in 3.9).

**Exit criteria**
- Property tests: resolutions satisfy all constraints; results are deterministic across runs.

## 1.10 Lockfile (Baseline)

**Objective:** deterministic `jsm.lock`.

- Format per Appendix A; includes importers, packages, integrity, resolved URL, dependency edges, engines, platform fields.
- Sorted keys and stable serialization so identical resolutions yield byte-identical files.
- Include `lockfile_version` and `generated_by`; refuse to read newer major versions with a clear message.
- Detect staleness by comparing manifest dependency specs to the lockfile importers.
- `--frozen-lockfile` MUST fail with exit code 6 if the lockfile would change; `--no-lockfile` MUST skip read and write.

**Exit criteria**
- Serialization is stable under shuffled input order; staleness detection tests cover add, remove, and range changes.

## 1.11 Isolated Linker (Baseline)

**Objective:** strict, space-efficient `node_modules`.

- Create a virtual store at `node_modules/.jsm/<name>@<version>[_peers]/node_modules/<name>`.
- Create top-level symlinks (junctions on Windows) for direct dependencies only; dependency edges are symlinks inside the virtual store.
- Link package files from the store with priority **reflink, then hardlink, then copy**, detected per volume and cached.
- Write a hidden state file (`node_modules/.jsm/state.json`) recording the applied lockfile hash and linker settings.
- Perform incremental relinking: add or remove only changed entries.
- Remove orphaned virtual store entries after a successful install.

**Exit criteria**
- Node can `require()` all direct dependencies; undeclared dependencies are not resolvable (no phantom deps).

## 1.12 Bin Links

**Objective:** correct executables.

- Honor `bin` (string and map) and `directories.bin`.
- Create symlinks on Unix with executable bit ensured; create `.cmd` and PowerShell shims on Windows.
- Resolve conflicts between packages exposing the same bin name deterministically (direct dependency wins, then lexical order) with a warning.
- Rewrite shebangs only when required (documented behavior).

**Exit criteria**
- `jsm exec <bin>` works on all three OSes in CI.

## 1.13 Baseline Commands

**Objective:** the minimum usable CLI.

- `jsm init [-y]`: create manifest.
- `jsm add <spec...> [-D|-O|-P] [--exact]`: resolve, fetch, link, update manifest and lockfile.
- `jsm install [--frozen-lockfile] [--prod]`: install from lockfile (or resolve if absent).
- `jsm remove <pkg...>`: update manifest, lockfile, links.
- `jsm run <script>` and `jsm exec <bin>`: run scripts with `node_modules/.bin` on `PATH`, forwarding exit codes and signals.
- `jsm list` and `jsm why <pkg>` (basic).
- Every command supports `--json` with a versioned schema.

**Exit criteria**
- End-to-end tests for each command against the fake registry; JSON schema snapshot tests.

## 1.14 Integrity and Safety Baseline

**Objective:** no unverified bytes reach the project.

- Verify sha512 for every tarball; for registries lacking sha512, accept sha1 only with an explicit warning and a flag.
- Verify blob hashes on write.
- Reject path traversal and symlink escape at extraction and again at link time (defense in depth).
- Lifecycle scripts are **not executed** in Phase 1 (policy introduced in 4.4).

**Exit criteria**
- Security test suite passes; no script execution path exists in the Phase 1 binary.

**Phase 1 Gate:** `init`, `add`, `install`, `remove` work against the real npm registry for the top-100 packages; first public benchmark published against npm and pnpm.

---

# PHASE 2: Store Management and Safety

**Goal:** make the shared store a managed, safe, inspectable product feature.

## 2.1 Reference Registry

**Objective:** know who uses what.

- Embedded database (`redb` or SQLite; decision recorded in an ADR) under `index/`.
- Record per project: absolute path, project id, lockfile hash, packages used, last install timestamp.
- Record per package version: first-stored time, last-used time, size (logical and physical), reference count.
- Register references transactionally at install commit; update `last_used_at` on link.
- Detect stale projects (path missing, lockfile changed) lazily and during prune.
- Handle project moves by content (lockfile hash plus project id file) where possible.

**Exit criteria**
- Reference counts match ground truth after randomized add, remove, move, and delete operations.

## 2.2 Store CLI

**Objective:** complete version management from the terminal.

- `jsm store path`: print location.
- `jsm store status`: total size, package count, version count, dedupe ratio, reference count.
- `jsm store list [--sort size|name|used] [--filter <glob>]`: packages, versions, sizes, usage counts.
- `jsm store versions <pkg>`: versions available locally with size and usage.
- `jsm store info <pkg>@<ver>`: metadata, file count, size, references, build variants, integrity.
- `jsm store add <pkg>@<range>`: resolve and pre-fetch into the store (supports multiple specs and `--from-lockfile`).
- `jsm store remove <pkg>@<ver>`: remove one version; refuse when referenced unless `--force`; `--all` removes all versions of a package.
- `jsm store usage <pkg>@<ver>`: list referencing projects.
- `jsm versions <pkg>`: remote registry versions, marking those present locally.
- All commands support `--json`; destructive commands support `--dry-run` and confirmation prompts (skipped with `--yes`).

**Exit criteria**
- Removing a referenced version without `--force` fails with exit code 1 and an actionable message; dry-run output equals actual effect.

## 2.3 Garbage Collection and Pinning

**Objective:** reclaim space safely.

- `jsm store prune`: remove versions with zero live references.
- `jsm store gc --older-than <dur> --max-size <size>`: LRU eviction using `last_used_at`; never remove referenced or pinned entries.
- `jsm store pin|unpin <pkg>@<ver>` and `jsm store pinned`.
- Blob GC: mark-and-sweep over manifests to delete unreferenced blobs; run only under a store-wide maintenance lease so concurrent installs are safe.
- Installs MUST take read leases on in-flight packages so GC cannot delete them mid-install.
- Report reclaimed bytes (logical and physical).

**Exit criteria**
- Stress test: concurrent installs and GC never produce a missing blob or a broken `node_modules`.

## 2.4 Verify and Repair

**Objective:** detect and heal corruption.

- `jsm store verify [pkg@ver]`: re-hash blobs, check manifests, report missing, extra, or mismatched files.
- `jsm store verify --fix`: quarantine corrupt blobs, drop broken package manifests, and mark for re-fetch.
- Detect in-place mutation of linked files (hardlink hazard) by comparing size, mtime, and optionally hash.
- Provide fast mode (metadata-only) and full mode (hash everything), with progress and parallelism.
- On install, a package flagged corrupt MUST be re-fetched transparently.

**Exit criteria**
- Corruption injection tests (bit flips, truncation, deletion) are detected and repaired.

## 2.5 Cross-Process Concurrency

**Objective:** many `jsm` processes, one store, no corruption.

- No global store lock; use fine-grained advisory locks (`fs4`) per package and per maintenance operation.
- **Single-flight downloads across processes:** a lock file per `name@version@integrity`; waiting processes reuse the winner's result; stale locks recovered via PID plus heartbeat.
- Lock acquisition has timeouts, fairness, and clear diagnostics ("waiting for process 4321").
- Detect unsafe filesystems (network mounts) and warn about locking guarantees.
- Provide a documented lock hierarchy to prevent deadlocks.

**Exit criteria**
- 50 parallel processes installing overlapping dependency sets produce identical, valid results and each tarball is downloaded exactly once.

## 2.6 Transactional Installs and Crash Recovery

**Objective:** failures never leave a half-installed project.

- Stage `node_modules` changes in a staging directory and apply via atomic swap/rename where the platform allows; otherwise journal operations for rollback.
- Write an install journal; on start, detect an incomplete journal and roll forward or back.
- Interrupted installs leave the last good state intact.
- Clean orphaned `tmp/` data older than a threshold on startup.
- Crash injection tests terminate the process at every pipeline boundary.

**Exit criteria**
- After a kill at any injected point, the next `jsm install` succeeds and yields a valid tree.

## 2.7 Machine-Readable Output and Shell Support

**Objective:** automation-friendly CLI.

- `--json` for every command; NDJSON event streams for long-running commands (`--json` plus `--progress=ndjson`).
- Document schemas as `jsm.v1.<command>` and freeze them under semver.
- Generate shell completions for bash, zsh, fish, PowerShell, and elvish; include dynamic completion for package names from local store and manifest.
- Provide `jsm help <topic>` pages (store, lockfile, config, scripts).

**Exit criteria**
- Schema tests prevent accidental breaking changes.

## 2.8 Doctor

**Objective:** self-diagnosis.

- `jsm doctor` checks: store integrity (fast), link capability (reflink/hardlink/symlink), cross-device issues, long-path support, case-insensitivity, file-lock reliability, proxy and CA configuration, registry reachability, clock skew, disk space, Windows Defender and Dev Drive status, Node and jsm versions.
- Each check outputs status, explanation, and a suggested remedy.
- `jsm doctor --fix` applies safe automatic repairs (clean temp, rebuild index, relink).
- `jsm doctor --report` emits a redacted, shareable diagnostic bundle.

**Exit criteria**
- Each check has a positive and negative test using injected environment faults.

## 2.9 Merge-Friendly Lockfile and Merge Driver

**Objective:** painless lockfile conflicts.

- Lockfile layout designed for line-based merges (one entry per block, sorted).
- `jsm lock merge %O %A %B` git merge driver: parse both sides, union importers, re-resolve conflicts against manifests, write a valid lockfile.
- `jsm lock install-merge-driver` configures `.gitattributes` and git config.
- `jsm lock verify` validates structure, integrity, and consistency with manifests.

**Exit criteria**
- Scripted merge scenarios (parallel add, conflicting upgrades) produce a valid lockfile without manual edits.

**Phase 2 Gate:** store CLI complete; multi-process stress and crash-injection suites pass; `doctor` ships.

---

# PHASE 3: Performance and Time Travel

**Goal:** deliver the speed pillar and the multi-version "time travel" experience.

## 3.1 Pipelined Install Scheduler

**Objective:** replace phased install with a streaming DAG.

- Start a package's fetch immediately when the resolver commits to its version.
- Connect stages via bounded channels with backpressure; configurable queue sizes.
- Prioritize the critical path (largest remaining transitive subtree first) using a priority queue.
- Separate thread pools: async network (tokio), CPU-bound decompress/hash (rayon or blocking pool), I/O (dedicated).
- Ensure linking begins as soon as a package and its dependencies are in the store, without waiting for the full graph.
- Provide deterministic outcomes regardless of scheduling order.

**Exit criteria**
- Install time on the medium fixture improves at least 25% versus the phased baseline; determinism tests pass.

## 3.2 Adaptive Concurrency and Hedging

**Objective:** self-tuning network behavior.

- AIMD controller per host: increase window on success with low latency, decrease multiplicatively on timeouts, 429/503, or latency inflation.
- Track per-host latency percentiles (p50, p95, p99), throughput, error rates, and HTTP/2 stream saturation.
- **Hedged requests:** after a dynamic threshold (for example p95), send a duplicate (same host or configured mirror); cancel the loser; cap hedging rate (for example 5% of requests).
- Respect `Retry-After` and server-communicated limits.
- User overrides: `--network-concurrency`, `--child-concurrency`, `--io-concurrency`; `auto` is the default.
- Controller state is observable through metrics and trace.

**Exit criteria**
- Under simulated high latency and packet loss, p99 per-package fetch time drops versus fixed concurrency; no request storms under 429 conditions.

## 3.3 Skip-Write Extraction and Fast Existence Index

**Objective:** do not write what already exists.

- Maintain an in-memory index of known blob hashes, loaded lazily and backed by the on-disk index.
- Put a Bloom filter in front of the index to answer "definitely not present" cheaply.
- During extraction, hash each entry and skip writing known blobs.
- Provide an optimized path when upgrading between versions (most files identical).
- Measure and report bytes skipped per install.

**Exit criteria**
- Upgrading a package between adjacent versions writes only changed files (verified through I/O counters).

## 3.4 Materialization Cache

**Objective:** warm installs in milliseconds.

- Cache key: hash of lockfile, linker mode, platform, patch set, and build keys.
- Store a fully linked layout (or a recipe) under a cache directory on the same volume.
- On hit, create `node_modules` using directory-level clone (`clonefile`, reflink, hardlink tree) where possible.
- Validate cache entries cheaply (state file plus spot checks) and invalidate on store changes affecting contained packages.
- Bound cache size with LRU eviction; integrate with `store gc`.
- Provide `jsm cache materialization list|clear`.

**Exit criteria**
- Materialization hit on the large fixture completes within the Phase 3 performance budget (Appendix D).

## 3.5 Platform-Specific I/O Optimization

**Objective:** use the fastest primitive each OS offers.

- Linux: `FICLONE` reflinks, `copy_file_range`, optional `io_uring` batching for file creation behind a feature flag, parallel directory creation.
- macOS: `clonefile` for files and directories; `copyfile` with clone flag.
- Windows: block cloning on ReFS/Dev Drive, `CreateHardLink`, long-path-aware APIs (`\\?\`), minimized small-file operations, Defender exclusion guidance via `doctor`.
- Probe link capability once per (store volume, project volume) pair and cache the result.
- Batch syscalls and directory operations; avoid redundant `stat` calls.

**Exit criteria**
- Per-platform microbenchmarks demonstrate improvement over the portable fallback.

## 3.6 Store-Aware Resolution

**Objective:** prefer what is already local.

- `--prefer-store` (and config `install.prefer_store`): among versions satisfying a range, choose the highest version present in the store, if one exists, otherwise highest overall.
- Never violate lockfile pins or explicit ranges.
- Offline mode implies store-preferred and cached-metadata-only resolution.
- Report in the summary how many packages were satisfied from the store.

**Exit criteria**
- Deterministic results given identical store contents; documented interaction with `update` (explicit `update` overrides preference).

## 3.7 History, Undo, Snapshots, and Switch

**Objective:** instant version time travel.

- Every mutating command records a history entry: timestamp, command, lockfile before and after (content-addressed), summary of changes.
- `jsm history [--limit N]`: list entries.
- `jsm undo` and `jsm redo`: restore the previous or next state by relinking from the store; MUST NOT require network if the store contains the packages.
- `jsm snapshot save|restore|list|delete <name>`: named states.
- `jsm switch <pkg> <version>`: change one dependency to a specific version; update manifest (respecting range or pin policy) and lockfile; relink; fetch only if missing.
- History retention policy configurable (count and age); history entries pin their packages against GC while retained (configurable).
- Operations are transactional (see 2.6).

**Exit criteria**
- Sequence tests (add, upgrade, undo, redo, switch, snapshot restore) restore exact byte-identical lockfiles and valid trees.

## 3.8 Hoisted Linker

**Objective:** compatibility with tools that need a flat layout.

- Implement an npm-compatible hoisting algorithm with deterministic placement and conflict handling.
- Support `--linker=hoisted`, with per-project config.
- Provide `nohoist`-style exclusions and public hoist patterns for isolated mode (`public-hoist-pattern`).
- Reuse the same store linking primitives.

**Exit criteria**
- A set of known "needs flat node_modules" projects (for example certain React Native or Electron setups) builds under hoisted mode.

## 3.9 Conflict Explanation and Suggestions

**Objective:** failures that teach.

- Render solver conflict information as readable trees with package, range, and dependent chain.
- Compute suggested fixes: compatible upgrades or downgrades of intermediate packages, override suggestions, peer range adjustments.
- Provide `--explain` for verbose derivations and `--json` structured conflict data.
- Link to documentation anchors per error class.

**Exit criteria**
- A curated suite of 30 real conflict cases yields explanations judged accurate and actionable in review.

**Phase 3 Gate:** benchmark targets in Appendix D for cold, warm store, and materialization scenarios met; undo/switch/snapshot suites pass.

---

# PHASE 4: Real-World Compatibility

**Goal:** handle the long tail of npm package behavior and introduce the security-first script and build model.

## 4.1 Peer Dependencies

**Objective:** correct peer resolution.

- Resolve peers in the context of the dependent; create distinct virtual store instances keyed by `name@version` plus peer set hash.
- Support `peerDependenciesMeta.optional`, auto-install of peers (configurable: `auto-install-peers`), and strict mode (`strict-peer-dependencies`).
- Deduplicate peer-dependent instances where the peer sets are identical.
- Warn (or error under strict mode) on unmet or mismatched peers with explanation.
- Keep virtual store path lengths bounded (hash suffix beyond a threshold, with a mapping file).

**Exit criteria**
- Matches reference behavior on a curated peer-dependency test corpus (React ecosystem, ESLint plugins, Babel presets).

## 4.2 Optional and Platform-Filtered Dependencies

**Objective:** install only what applies.

- Evaluate `os`, `cpu`, `libc` before download; skip non-matching optional dependencies without error.
- Failure of an optional dependency (including build failure) MUST NOT fail the install; record in lockfile and summary.
- Fail with a clear error if a non-optional package's platform constraints exclude the current platform (unless `--force`).
- Support cross-platform lockfile generation (`--platform` flags) and per-platform install selection (`supportedArchitectures` config).
- Enforce `engines` according to `engine_strict` (`off`, `warn`, `error`).

**Exit criteria**
- Install of packages with native optional binaries (for example `esbuild`, `swc`, `sharp`) selects the correct platform package only.

## 4.3 Dependency Protocols

**Objective:** support every specifier kind.

- `npm:` aliases (including scoped aliases and version ranges).
- `file:` (directory and tarball), `link:` (symlink without copy), `workspace:` (resolved in 5.1).
- `git+https`, `git+ssh`, `github:` shorthand with commit, tag, branch, and semver ranges; run `prepare` in an isolated checkout subject to script policy.
- Direct tarball URLs with integrity recording.
- Store git and tarball sources in the same CAS keyed by resolved integrity or commit.
- Validate and canonicalize all specifiers; reject ambiguous ones with guidance.

**Exit criteria**
- Each protocol has end-to-end tests; git dependencies reproduce deterministically from the lockfile.

## 4.4 Script Policy

**Objective:** secure-by-default lifecycle script handling.

- Default policy `deny`: dependency lifecycle scripts (`preinstall`, `install`, `postinstall`, `prepare` of dependencies) are not run.
- Root project lifecycle scripts (`prepare`, `postinstall`, etc.) run per configuration (`--ignore-scripts` to disable).
- `jsm approve-scripts [pkg...]`: interactive and non-interactive approval; stored in `package.json` (`jsm.allowScripts`) or `jsm-approvals.json`, keyed by `name@version-range` and script content hash.
- Warn after install listing packages whose scripts were skipped, with the approve command.
- `scripts.policy = allowlist | allow | deny` in config.
- Detect and flag packages that need scripts to function (known-package hints database; extensible).

**Exit criteria**
- With policy deny, no dependency script executes in any test; approval workflow tests pass.

## 4.5 Local Build Cache

**Objective:** compile once, reuse everywhere on the machine.

- Compute **build key** = hash(package integrity, script content hash, Node ABI, platform, arch, libc, toolchain fingerprint (compiler, node-gyp, Python, MSVC/Xcode versions), allowlisted env vars, relevant dependency build keys).
- Store build outputs under `builds/<build-key>/` as content-addressed trees.
- On approved script execution, check the cache first; on miss, build and capture the resulting file set (diff against pre-build state).
- Link or reflink cached outputs into the project's virtual store entry (never mutate shared store files).
- Provide `jsm cache build list|info|clear|verify`.
- Record cache hit/miss statistics in the install summary.

**Exit criteria**
- A second project installing the same native package builds nothing and links the cached output.

## 4.6 Parallel Build Scheduler

**Objective:** fast, safe builds.

- Build in topological order (dependencies before dependents) with a job pool sized from CPU count, available memory, and `--child-concurrency`.
- Per-package build logs saved under the store (`jsm build-log <pkg>`), streamed with prefixes when `--verbose`.
- Timeouts per script (configurable) and process-tree cleanup on cancel or failure.
- Failure of a required build aborts the install with a transactional rollback; optional build failure is recorded and skipped.
- Propagate sanitized environment variables; set standard `npm_package_*`/`npm_config_*` variables for compatibility.

**Exit criteria**
- Build of a project with 20 native dependencies uses all permitted cores and produces correct output; cancel leaves no orphan processes.

## 4.7 Insight and Diff Commands

**Objective:** make the store's multi-version data useful.

- `jsm diff <pkg>@<a> <pkg>@<b>`: file-level diff (added, removed, modified) using stored manifests; content diff on request; `--deps`, `--scripts`, `--exports`, `--size` views. No network required if both versions are stored; otherwise fetch the missing one.
- `jsm why <pkg> [--size]`: all dependency paths, with size contribution.
- `jsm dupes`: duplicate versions within a project and across the store, with suggested dedupe actions.
- `jsm unused`: static analysis of `import`, `require`, dynamic imports with configurable ignore patterns and known tool-config references.
- `jsm outdated`: current, wanted, latest, with age and release-note links when available.

**Exit criteria**
- Diff results verified against `diff -r` on extracted tarballs; `unused` precision measured on a labeled corpus.

## 4.8 Package Executor (`jsm x`)

**Objective:** instant `npx` replacement.

- `jsm x <pkg>[@range] [args...]`: resolve, fetch to store, create a cached ephemeral environment keyed by resolved lockfile, and execute the bin.
- Cache ephemeral environments for reuse (second run starts in milliseconds).
- Respect script policy and sandboxing (when available).
- Support `--package`, multiple packages, and bin name disambiguation.
- Provide TTL for cached range resolution (for example 1 hour for `latest`) with `--refresh`.

**Exit criteria**
- Second invocation of a cached tool starts under the Appendix D budget.

## 4.9 Release Cooldown and Resolution Modes

**Objective:** safer and test-friendly resolution.

- `--min-release-age <dur>` (and config): exclude versions published more recently than the duration; allow per-package exceptions; produce explanations when exclusion causes failure.
- `--resolution lowest` and `lowest-direct`: pick the oldest satisfying versions (for library authors).
- Time-based resolution (`--before <date>`) for reproducing historical states.
- Interaction rules documented for lockfile, `update`, and `prefer-store`.

**Exit criteria**
- Deterministic tests with fixture publish times verify exclusion and fallback behavior.

## 4.10 Compatibility Corpus and CI

**Objective:** evidence of ecosystem compatibility.

- Corpus A: top 1,000 npm packages by downloads; install each in isolation and verify `require`/`import` of the main entry (or declared `exports`).
- Corpus B: 100+ open-source repositories (frameworks, monorepos, tools) with their own test scripts run after install.
- Compare resulting dependency trees and runtime behavior against npm and pnpm; classify and track diffs.
- Run on Linux (glibc, musl), macOS, and Windows; nightly and on release candidates.
- Maintain a published compatibility dashboard and a known-issues list.

**Exit criteria**
- Corpus A at least 95% pass at Phase 4 gate (98% by v1.0); regressions block release.

## 4.11 Registry Authentication, Proxy, and Enterprise Networking

**Objective:** work in corporate environments.

- Scoped registries and per-registry auth: bearer tokens, basic auth, `_authToken`, `_auth`, credential helpers.
- Proxy support (HTTP, HTTPS, SOCKS where feasible), `NO_PROXY`, custom CA bundles, client certificates.
- OIDC-based token acquisition hooks for CI registries (extensible plugin interface).
- Secure credential storage guidance; never write tokens to logs, lockfile, or store.
- Registry mirror and fallback list with health-based failover.

**Exit criteria**
- Integration tests with an authenticated fake registry, a proxy, and a custom-CA TLS endpoint.

**Phase 4 Gate:** corpus thresholds met; script policy and build cache operational; peer and optional dependency suites pass.

---

# PHASE 5: Production Readiness (v1.0)

**Goal:** everything required to recommend `jsm` for production use.

## 5.1 Workspaces and Monorepos

**Objective:** first-class multi-package repositories.

- Discover workspaces from `workspaces` globs in `package.json` or `jsm-workspace.toml`; support negation patterns.
- `workspace:` protocol (`workspace:*`, `workspace:^`, `workspace:~`, exact) with version rewriting on publish/deploy.
- Single shared lockfile with per-importer sections.
- Filtering: `--filter <expr>` by name, path, glob, git-changed since ref (`...[origin/main]`), dependents (`pkg...`), and dependencies (`...pkg`).
- Run scripts across workspaces: `jsm -r run <script>` with topological ordering, parallelism, `--stream`, and failure policy (`--bail`/`--no-bail`).
- Workspace-aware commands: `add --workspace`, `workspaces list`, `why`, `outdated`.
- Local package linking uses symlinks with no copying.

**Exit criteria**
- A 100-package fixture monorepo installs, filters, and runs scripts with correct ordering; changed-since filtering verified against git history fixtures.

## 5.2 Catalogs, Overrides, and Resolutions

**Objective:** centralized version governance.

- `catalogs` (named and default) in workspace config; `catalog:` protocol in manifests.
- `overrides` / `resolutions` with selector syntax (by package, by parent, by version range) and `$dependency` references.
- Validate overrides (unused overrides warn); display in `why` outputs.
- `jsm update --catalog <name>`.

**Exit criteria**
- Override precedence rules documented and tested, including nested and scoped selectors.

## 5.3 Patching

**Objective:** durable local modifications.

- `jsm patch <pkg>@<ver>`: extract to an editable temp directory (copy, never touching the store).
- `jsm patch-commit <dir>`: compute a diff and save in `patches/` and register in `package.json` (`jsm.patchedDependencies`) with a content hash.
- Apply patches at link time to a per-project copy (never hardlinked), include patch hash in lockfile and materialization key.
- Fail clearly when a patch no longer applies after an update; provide `jsm patch-remove`.

**Exit criteria**
- Patched packages do not alter store content; patch failure produces a clear diagnostic with hunk info.

## 5.4 Lockfile Importers and Migration

**Objective:** painless adoption.

- Importers: `package-lock.json` (v1, v2, v3), `npm-shrinkwrap.json`, `yarn.lock` (v1 and Berry), `pnpm-lock.yaml` (multiple versions), `bun.lock`.
- `jsm migrate [--from <tool>]`: convert lockfile preserving resolved versions where possible, verify integrity, and report non-translatable entries.
- Import settings from `.npmrc`, `.yarnrc.yml`, `pnpm-workspace.yaml`, and `bunfig.toml` where applicable.
- Offer a dry-run and diff report; do not delete the old lockfile unless `--remove-old`.
- Optional exporters (`jsm lock export --format npm`) for incremental team adoption.

**Exit criteria**
- Migration of the Corpus B repositories yields installs whose resolved versions match the source lockfile in at least 99% of entries.

## 5.5 Provenance and Signature Verification

**Objective:** trust the supply chain.

- Verify registry signatures using published registry keys (with key rotation handling and caching).
- Verify npm provenance attestations (Sigstore bundles: certificate chain, transparency log inclusion, builder identity).
- Policy: `security.provenance = off | warn | require`, with per-package exceptions and "must not regress" rule (a package that previously had provenance and no longer does triggers a warning or error).
- Record verification results in the lockfile (compact flags) and surface in `info` and `audit`.
- Operate offline using cached keys and bundles.

**Exit criteria**
- Tests with valid, tampered, expired, and revoked attestations behave per policy.

## 5.6 Audit and Advisories

**Objective:** known-vulnerability visibility.

- Local advisory database (OSV and GitHub Advisory data) synchronized incrementally; usable offline.
- `jsm audit [--prod] [--severity <level>] [--fix]`: report by package, severity, and path; `--fix` proposes or applies compatible updates and overrides.
- Exit non-zero when advisories at or above a threshold exist (CI mode).
- Support allowlisting advisories with justification and expiry.
- Provide SARIF and JSON output for code-scanning integrations.

**Exit criteria**
- Detection results match a reference scanner on a fixture lockfile set; offline mode functions with a synced database.

## 5.7 Sandboxed Script Execution

**Objective:** approved scripts still run with least privilege.

- Linux: Landlock (filesystem), seccomp-bpf (syscalls), network namespace or seccomp network denial, optional user namespaces; graceful degradation with explicit warnings by kernel capability.
- macOS: generated `sandbox-exec` profiles scoped to the package build directory.
- Windows: Job Objects (process limits, kill-on-close), restricted tokens/AppContainer where available.
- Default policy: no network, write access only to the package build directory and a private temp dir, read access to the package, its dependencies, and system toolchain paths.
- Per-package allowlist entries can grant additional capabilities (network for prebuilt download, specific paths).
- Sandbox policy is part of the build key so cached results remain sound.
- `scripts.sandbox = true` by default; `--no-sandbox` requires explicit opt-out and logs a warning.

**Exit criteria**
- Escape-attempt test suite (network access, writes outside allowed paths, reading `~/.ssh`) fails closed on all supported platforms; common native packages still build in the sandbox.

## 5.8 Prebuilt Binary Resolution

**Objective:** avoid compiling when binaries exist.

- Resolution order: local build cache, remote build cache (when enabled), platform-specific optional packages, known prebuild conventions (`prebuildify`, `node-pre-gyp`, `prebuild-install`, N-API `binary` fields), then compile.
- Download prebuilt binaries through the controlled fetch pipeline (integrity verified, stored in CAS, linked) rather than through arbitrary script network access.
- Maintain a curated metadata map for popular packages to shortcut detection.
- Report in the install summary whether each native package was cached, prebuilt, or compiled.

**Exit criteria**
- For the top native packages (Corpus A subset), at least 90% are satisfied without compilation on supported platforms.

## 5.9 Script Change Review

**Objective:** prevent silent script escalation.

- Record the approved script content hash for each approval.
- On update, if scripts for an approved package change, revoke approval and display a diff of the scripts and referenced files.
- Detect newly introduced lifecycle scripts in packages previously script-free and flag them prominently.
- Provide `jsm approve-scripts --review` interactive flow.

**Exit criteria**
- Update scenarios with changed or added scripts block execution until re-approved.

## 5.10 Update Review Report

**Objective:** informed upgrades.

- `jsm update --review` (and `--interactive`): per package show version delta, size delta, new and removed dependencies, install script changes, `exports`/`bin` changes, provenance status, advisories fixed or introduced, and age.
- Present results in a table and as JSON; allow selective acceptance.
- Use the store diff engine (4.7) for local comparison where both versions are available.

**Exit criteria**
- Report output verified on fixture upgrades covering dependency, script, and export changes.

## 5.11 Store Export and Import

**Objective:** portable stores for CI and air-gapped environments.

- `jsm store export <lockfile|--packages> <out>`: create a bundle (tar plus zstd) with the minimal set of manifests and blobs, plus a bundle manifest with integrity and optional signature.
- `jsm store import <bundle>`: verify and merge idempotently.
- `jsm install --offline --from-bundle <bundle>` convenience flow.
- Support incremental bundles (delta against a base bundle).

**Exit criteria**
- Air-gapped CI scenario: export on a connected machine, import on a disconnected one, and install successfully.

## 5.12 Production Slimming

**Objective:** smaller deployments.

- `jsm install --prod`: skip `devDependencies`.
- `--slim`: remove unneeded files (tests, docs, sourcemaps, `.d.ts` optional, non-matching platform binaries) using `files`, `exports`, and conservative built-in rules, with an allow/deny config.
- `jsm deploy <dir> [--filter <pkg>]`: produce a self-contained production tree for a workspace package (resolving `workspace:` deps into copies).
- Provide a size report before and after.

**Exit criteria**
- Slimmed outputs of the compatibility corpus still pass their runtime smoke tests.

## 5.13 Release Engineering

**Objective:** trustworthy distribution.

- Reproducible release builds; signed binaries (cosign/Sigstore) and published checksums and SBOM (CycloneDX).
- Distribution: GitHub releases, install script, Homebrew, Scoop/winget, apt/rpm repos, npm wrapper package (`npm i -g @jsm/cli` downloading the right binary), Docker images.
- Self-update (`jsm self-update`) with signature verification and channel selection (stable, beta).
- Semantic versioning for CLI, JSON schemas, lockfile, and store formats; documented deprecation policy (at least two minor releases).
- Store and lockfile format migrations tested across versions (forward and backward compatibility policy).
- Security policy (`SECURITY.md`), vulnerability disclosure process, and dependency update automation.

**Exit criteria**
- A release candidate is produced from a tagged commit by CI alone; migration tests across all supported previous versions pass.

## 5.14 Documentation

**Objective:** complete user and contributor docs.

- Getting started, command reference (generated from clap), configuration reference, lockfile and store format docs.
- Migration guides (npm, Yarn, pnpm, Bun), CI recipes (GitHub Actions, GitLab CI, CircleCI, Docker layers and caching), monorepo guide, troubleshooting, security guide.
- Architecture documentation and ADR index.
- Documentation tests: all code examples and command snippets validated in CI.

**Exit criteria**
- Every command and config key is documented; link and example checks pass.

**Phase 5 Gate (v1.0):** compatibility Corpus A at least 98%; all benchmark targets met; security review and fuzzing complete; signed releases published.

---

# PHASE 6: Ecosystem and Scale

**Goal:** team-scale and infrastructure features beyond a single machine.

## 6.1 Store-as-Registry (`jsm serve`)

**Objective:** share a store over a network.

- Serve an npm-registry-compatible read API backed by the local store with upstream fallthrough and caching.
- Authentication (tokens), TLS, rate limits, and access logging.
- Optional LAN discovery (mDNS) and peer-to-peer fetch from teammates' stores with integrity verification.
- Metrics endpoint (Prometheus) and health checks.
- Configurable upstream and cache policy (read-through, pull-through).

**Exit criteria**
- Two clients install through a third machine's `jsm serve` with a single upstream download per package.

## 6.2 Remote Build Cache

**Objective:** share compiled artifacts across a team and CI.

- Backends: S3-compatible, GCS, Azure Blob, HTTP(S), and `jsm serve` peers.
- Entries addressed by build key; signed with team keys and verified on download.
- Read-only vs. read-write modes (typically CI writes, developers read).
- Upload and download parallelism with the adaptive concurrency controller.
- Poisoning defenses: signature verification, key rotation, namespace isolation, TTL, and audit logs.
- Provide `jsm cache remote login|status|push|pull|verify`.

**Exit criteria**
- A CI-built artifact is consumed by a developer machine without compilation; tampered entries are rejected.

## 6.3 Sparse Registry Index

**Objective:** near-instant, offline-capable resolution.

- Define an incremental, compressed metadata index format (per-package files, sharded paths, delta updates).
- Client sync with ETag/range updates; local index cached in the store.
- Optional server component (or `jsm serve` mode) generating the index from an upstream registry.
- Fall back to standard metadata API when the index is unavailable.
- Resolution against the local index requires no per-package network calls.

**Exit criteria**
- Resolution of the large fixture with a warm index completes with zero network requests.

## 6.4 Merkle Verification and Store Sync

**Objective:** fast integrity and efficient synchronization.

- Maintain a Merkle tree over package manifests and blobs; store root hashes.
- `jsm store verify --fast` uses subtree comparison to detect divergence.
- Efficient sync between stores or bundles by exchanging subtree hashes.
- Tamper-evidence: signed root hashes for exported bundles and served stores.

**Exit criteria**
- Full-store verification time on a 20 GB store drops at least 10x versus full re-hash in the unchanged case.

## 6.5 Optional Daemon (`jsmd`)

**Objective:** zero-wait installs through background work.

- Long-running per-user daemon communicating over a local socket (Unix domain socket / named pipe) with versioned protocol.
- Keeps registry metadata hot, watches project manifests and lockfiles, and prefetches missing packages.
- Git integration: hooks (`post-checkout`, `post-merge`) trigger prefetch and optional pre-materialization.
- Coordinates store GC and verification in idle time.
- Strictly optional: every command MUST work identically without the daemon; auto-start only when configured.
- Resource limits (CPU, memory, network), idle shutdown, and observability (`jsm daemon status|logs`).
- Security: socket permissions restricted to the user; no remote control surface.

**Exit criteria**
- After `git checkout` of a branch with changed dependencies, a subsequent `jsm install` completes within the materialization-hit budget.

## 6.6 Capability Analysis

**Objective:** detect behavioral changes in packages.

- Static analysis (parse JS/TS with an embedded parser) identifying use of network, `child_process`, `fs` writes, `eval`/`Function`, `process.env` reads, native addon loading, and obfuscation indicators.
- Compute a capability profile per stored version, cached in the store.
- `jsm update` and `diff` flag newly introduced capabilities; policy can require approval (`security.capability_policy`).
- Provide `jsm capabilities <pkg>@<ver>`.
- Keep false-positive management via allowlists and heuristics tuning.

**Exit criteria**
- Detection tested against a corpus of known malicious package samples (in a safe dataset) and benign popular packages with measured precision and recall.

## 6.7 Typosquat and Anomaly Warnings

**Objective:** guard new dependency additions.

- On `add` of a new package, compare the name to popular packages (edit distance, confusable characters, scope impersonation) and warn.
- Flag suspicious metadata (very new package, no repository, sudden maintainer change, install scripts on a package with a popular-sounding name).
- Configurable strictness; offline popularity list bundled and updated.

**Exit criteria**
- Known typosquat examples are flagged in tests; false-positive rate on top-10,000 popular names is within a defined threshold.

## 6.8 Zero-Install Script Running

**Objective:** run single-file scripts with inline dependencies.

- `jsm run script.ts|js|mjs` reads an inline dependency header (comment block or `// jsm:` directive) and resolves dependencies into a cached ephemeral environment from the store.
- Lock inline dependencies optionally to a sidecar file for reproducibility.
- Detect and use an installed runtime (Node, Bun, Deno) per configuration; run TypeScript through a configured loader.
- Cache environments keyed by resolved set; second run is near-instant.

**Exit criteria**
- A script with three dependencies runs on first invocation without any project files and re-runs under the zero-install warm budget.

## 6.9 Advanced Deploy and Container Optimizations

**Objective:** best-in-class image builds.

- Layer-friendly output: deterministic ordering, separate dependency and app layers, `jsm install --docker` mode.
- Support BuildKit cache mounts using the store path; documented recipes.
- Generate minimal runtime trees with SBOM output (`jsm sbom --format cyclonedx|spdx`).

**Exit criteria**
- Reference Dockerfiles show measurable build time and image size improvements versus npm/pnpm baselines.

**Phase 6 Gate:** serve, remote cache, sparse index, and daemon operate with documented security models and pass integration suites.

---

# PHASE 7: Advanced Extensions

**Goal:** differentiating, higher-risk enhancements built on the stable core.

## 7.1 Chunk-Level Store with Compression

**Objective:** minimize disk usage across many versions.

- Content-defined chunking (FastCDC) of large files; chunk store with zstd compression (dictionaries per file type where beneficial).
- Transparent reassembly on materialization; small files remain whole-file blobs.
- Report logical vs. physical size in `store status`.
- Online migration from the whole-file store with resumable conversion and rollback.
- Maintain materialization performance within budget via parallel decompression and caching.

**Exit criteria**
- Store containing 40 versions of a large package (for example `typescript`) uses at most 35% of the whole-file store size; materialization regressions within the defined tolerance.

## 7.2 Interactive TUI (`jsm ui`)

**Objective:** visual exploration and management.

- Built with `ratatui`: views for project dependencies, store browser, updates, advisories, and history.
- Interactive update selection showing size delta, new dependencies, capabilities, and changelog links.
- Store view with "used by N projects" and in-place remove, pin, and verify actions (with confirmation).
- Keyboard navigation, search, and accessible color themes; non-TTY fallback message.

**Exit criteria**
- Usability testing with a defined task set; automated TUI snapshot tests.

## 7.3 Node Runtime Management

**Objective:** one tool for packages and runtimes.

- `jsm node install|use|list|remove|which <version>`; runtimes stored in the shared store (content-addressed, verified against official checksums and signatures).
- Honor `.node-version`, `.nvmrc`, `engines.node`, and `jsm.toml` runtime pinning; auto-switch via shims or per-command resolution.
- Include Node ABI detection feeding build keys.
- Optional support for other runtimes (Bun, Deno) via a pluggable runtime interface.

**Exit criteria**
- Version switching is instant for installed runtimes; checksum and signature failures are rejected.

## 7.4 Virtual Linker (Overlay Filesystem)

**Objective:** zero files written for project installs.

- Linux/macOS: FUSE (`macFUSE`, `FUSE-T`) overlay serving `node_modules` from the store; Windows: ProjectedFS or WinFsp.
- Lazy file materialization with caching; consistent semantics for `stat`, `readdir`, symlinks, and file watching.
- Clear compatibility matrix and automatic fallback to the isolated linker when the driver is unavailable.
- Opt-in only; never the default.
- Security review for privilege and sandbox interaction.

**Exit criteria**
- Dev server, bundler, and TypeScript language server function correctly on the virtual layout in the compatibility corpus; fallback tested.

## 7.5 Editor and Tooling Integrations

**Objective:** make `jsm` visible where developers work.

- JSON Schemas for `jsm.toml`, `jsm.lock`, and manifest extensions registered with schema stores.
- A language-server-friendly diagnostics output and a VS Code extension (dependency view, store view, update review, approve-scripts prompt).
- GitHub Action(s) for setup, cache restore/save of the store and build cache, and audit reporting.
- Renovate/Dependabot compatibility notes and, where feasible, native support.

**Exit criteria**
- Extension and Action published with documented versioning and tests.

## 7.6 Plugin and Extension API

**Objective:** controlled extensibility.

- Define stable hooks (resolution hooks, fetch hooks for custom protocols, post-install hooks) via WebAssembly (WASI) plugins with capability-limited access, or out-of-process JSON-RPC plugins.
- Plugin manifest, permissions model, and signature verification.
- Plugins MUST NOT bypass script policy, integrity checks, or sandbox.
- Provide a reference plugin and documentation.

**Exit criteria**
- A third-party plugin can add a custom registry protocol without modifying core; permission violations are blocked.

**Phase 7 Gate:** each extension ships behind a feature flag or opt-in setting, with its own acceptance, security review, and rollback plan.

---

# Appendix A: Data Formats

## A.1 Store Directory Layout

```
store/
  VERSION
  files/sha512/<xx>/<rest>
  packages/<name>/<version>/<integrity>.manifest
  metadata/<registry-host>/<name>.json
  metadata/<registry-host>/<name>.etag
  index/<db files>
  builds/<build-key>/...
  materialization/<cache-key>/...
  history/<project-id>/...
  tmp/
  locks/
  quarantine/
```

## A.2 Package Manifest (logical schema)

| Field | Type | Description |
|---|---|---|
| `name` | string | Package name |
| `version` | string | Exact version |
| `integrity` | string | SRI sha512 of tarball |
| `files[]` | array | `{ path, hash, mode, size }` |
| `symlinks[]` | array | `{ path, target }` (validated, in-package only) |
| `package_json` | object | Selected fields (bin, scripts, engines, os, cpu, libc, exports, deps) |
| `stored_at` | timestamp | First stored |
| `case_collisions[]` | array | Paths colliding on case-insensitive filesystems |

## A.3 Lockfile (logical schema)

```toml
lockfile_version = 1
generated_by = "jsm <version>"

[importers."<path>"]
dependencies = { <name> = "<spec>" }
dev_dependencies = {}
optional_dependencies = {}

[packages."<name>@<version>"]
resolution = "<tarball url | git commit | file path>"
integrity = "sha512-..."
dependencies = {}
optional_dependencies = {}
peer_dependencies = {}
peer_dependencies_meta = {}
engines = {}
os = []
cpu = []
libc = []
has_scripts = false
provenance = "verified | absent | unverified"
```

## A.4 Build Key Inputs

- Package integrity
- Script content hash
- Node ABI version
- Platform (`os`, `cpu`, `libc`)
- Toolchain fingerprint (compiler, node-gyp, Python, platform SDKs)
- Allowlisted environment variable values
- Build keys of native dependencies
- Sandbox policy hash

---

# Appendix B: CLI Command Index

| Group | Commands |
|---|---|
| Project | `init`, `install`, `add`, `remove`, `update`, `outdated`, `list`, `why`, `dupes`, `unused`, `link`, `unlink`, `patch`, `patch-commit`, `patch-remove`, `migrate`, `audit`, `approve-scripts`, `doctor` |
| Execution | `run`, `exec`, `x`, `node` |
| Store | `store path`, `status`, `list`, `versions`, `info`, `add`, `remove`, `usage`, `prune`, `gc`, `pin`, `unpin`, `pinned`, `verify`, `export`, `import` |
| Registry and versions | `versions`, `info`, `diff`, `switch`, `search`, `capabilities` |
| History | `history`, `undo`, `redo`, `snapshot save|restore|list|delete` |
| Workspace | `-r`, `--filter`, `workspaces list`, `deploy` |
| Services | `serve`, `daemon start|stop|status|logs`, `cache build|materialization|remote ...`, `ui` |
| Lockfile | `lock merge`, `lock verify`, `lock install-merge-driver`, `lock export` |
| Config and misc | `config`, `completions`, `help`, `self-update`, `sbom`, `build-log` |

---

# Appendix C: Exit Codes and Error Taxonomy

| Exit code | Error family | Description |
|---|---|---|
| 0 | n/a | Success |
| 1 | `JSM_E_GENERAL` | Unspecified failure |
| 2 | `JSM_E_USAGE` | Invalid arguments or configuration |
| 3 | `JSM_E_RESOLVE` | Dependency resolution failure |
| 4 | `JSM_E_NETWORK` | Network or registry failure |
| 5 | `JSM_E_INTEGRITY` / `JSM_E_SECURITY` | Integrity, signature, provenance, or policy violation |
| 6 | `JSM_E_LOCKFILE` | Lockfile out of date or invalid under frozen mode |
| 7 | `JSM_E_SCRIPT` | Script or build failure |
| 8 | `JSM_E_STORE` | Store corruption, lock, or capacity failure |
| 9 | `JSM_E_INTERRUPTED` | Cancelled by signal |
| 10 | `JSM_E_AUDIT` | Advisories at or above threshold |

---

# Appendix D: Non-Functional Requirements and Budgets

Budgets are initial targets and MUST be recalibrated against Phase 0 baseline measurements; once ratified they become CI gates.

## D.1 Performance

| Scenario | Target |
|---|---|
| Cold install (medium fixture) | At least 1.5x faster than the fastest competitor |
| Warm store install | At least 3x faster than the fastest competitor |
| No-op re-install (1,000 packages) | Under 50 ms |
| Materialization cache hit (1,000 packages) | Under 500 ms |
| `jsm x` cached start | Under 150 ms overhead |
| Zero-install script warm start | Under 200 ms overhead |
| Branch switch | At least 5x faster than the fastest competitor |
| Peak memory (2,000-package install) | Under 300 MB |
| Disk (10 related projects) | At most 40% of npm |
| Store verify fast mode (20 GB, unchanged) | Under 5 seconds |

## D.2 Reliability

- Zero known data-loss defects in the store or project trees at release.
- Crash-injection suite passes at 100%.
- Installs are idempotent: repeated runs on the same inputs produce byte-identical lockfiles and equivalent trees.
- Multi-process stress suite passes with no corruption across 1,000 randomized runs.

## D.3 Security

- No dependency script executes without explicit approval (default policy).
- All network-fetched content is integrity-verified before becoming visible to projects.
- Extraction rejects traversal, escape, and decompression-bomb inputs.
- Credentials never appear in logs, lockfiles, stores, or diagnostic bundles.
- Fuzz targets (semver, tar, gzip, lockfile, config, manifest parsers) run continuously; findings block release.
- External security review before v1.0 and before enabling sandbox and serve features by default.

## D.4 Portability

- Tier 1: Linux x64/arm64 (glibc, musl), macOS arm64/x64, Windows x64.
- Tier 2: Windows arm64, FreeBSD (best effort).
- Filesystem matrix: ext4, btrfs, XFS, tmpfs, APFS, NTFS, ReFS/Dev Drive; network filesystems supported in degraded mode with warnings.
- Static binaries where feasible; minimal runtime dependencies.

## D.5 Usability

- Zero-configuration defaults are secure and fast.
- Every error includes a code, a cause, and where possible a suggested remedy.
- First successful install within 2 minutes of installing `jsm` for a new user (measured in usability tests).
- Telemetry is off by default; any future telemetry MUST be opt-in, documented, and anonymized.

## D.6 Maintainability

- Public API of each crate documented; `cargo doc` warnings are errors.
- Test coverage target: at least 80% line coverage on core crates; critical paths (store, resolver, linker) at least 90%.
- ADR required for any change to on-disk formats, security policy, or CLI/JSON schema.

---

# Appendix E: Requirements Traceability Map

| Capability | Sub-phases |
|---|---|
| Shared multi-version store | 1.6, 1.7, 2.1, 2.2, 2.3, 7.1 |
| Version listing, retrieval, removal | 2.2, 3.7, 4.7 |
| Speed (pipeline, concurrency, I/O) | 1.8, 3.1, 3.2, 3.3, 3.4, 3.5, 6.3 |
| Build optimization | 4.5, 4.6, 5.8, 6.2 |
| Concurrency and safety | 2.5, 2.6, 2.3 |
| Resolution quality | 1.4, 1.9, 3.6, 3.9, 4.1, 4.2, 4.9 |
| Security | 1.14, 4.4, 5.5, 5.6, 5.7, 5.9, 6.6, 6.7 |
| DX | 1.1, 2.7, 2.8, 3.7, 4.7, 4.8, 6.8, 7.2, 7.5 |
| Monorepo and enterprise | 4.11, 5.1, 5.2, 5.3, 6.1 |
| Adoption and migration | 5.4, 5.14 |
| Production release | 4.10, 5.13, Appendix D |
