# JSM Implementation Backlog

> **Status:** Phase 0 foundations and gate are complete on `main` (merged PR #2); Phases 1–7 remain open.
> **Companion:** [`PHASE.md`](PHASE.md) defines sequencing, phase gates, and cross-phase invariants.
> **Requirement source:** [`SPECS.md`](SPECS.md) is authoritative for detailed behavior and exit criteria; [`PROJECT.md`](PROJECT.md) is authoritative for product goals and scope.

## Working rules

- Keep the source sub-phase ID in each issue/PR/test reference. Split checklist items into reviewable tasks as needed; do not change the phase gate to accommodate an incomplete implementation.
- Check an item only after its implementation, tests, docs, and applicable security/platform evidence are merged. Link evidence from the related issue or PR.
- All unchecked items mean **not yet verified**, not necessarily unstarted. Phase 0 was merged and its gate passed in PR #2; later phases remain open.
- Resolve applicable open design questions in `PROJECT.md` through reviewed ADRs before freezing persisted data, external contracts, security defaults, or distribution behavior. See [Design decisions](#design-decisions-to-record).
- Phase 0–5 are the v1.0 delivery path. Phase 6–7 work is post-v1.0 and must remain opt-in as specified in `PHASE.md`.

## Phase 0 — Foundations

### 0.1 Repository and tooling

- [x] Create the Rust Cargo workspace and the specified crates: `jsm-cli`, `jsm-core`, `jsm-registry`, `jsm-resolver`, `jsm-store`, `jsm-fetch`, `jsm-linker`, `jsm-build`, `jsm-lockfile`, `jsm-workspace`, `jsm-security`, `jsm-daemon`, and `jsm-testkit`.
- [x] Pin the Rust toolchain and document the MSRV policy; enforce crate dependency boundaries, no cycles, and one CLI binary.
- [x] Add CI for Linux, macOS, and Windows with formatting, Clippy, tests, dependency/license/advisory checks, coverage, and documented required checks.
- [x] Add `justfile` or `xtask` for build, test, benchmark, lint, and release tasks; configure nextest and coverage tooling.
- [x] Add `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, issue/PR templates, and `docs/adr/` with an ADR template.

### 0.2 Core types and error model

- [x] Implement and validate shared types for package names, versions, ranges, integrity, package IDs, dist-tags, dependency specs, platforms, and Node ABI.
- [x] Define library errors and CLI diagnostics, stable error codes, process exit-code mapping, cause chains, and optional help text.
- [x] Add serialization round-trip tests for persisted types and snapshot tests for the error taxonomy.

### 0.3 Observability

- [x] Add structured `tracing` spans for resolve, fetch, extract, write, link, and build stages; support `JSM_LOG`, `--verbose`, and `--quiet`.
- [x] Implement Chrome-trace-compatible export and the metrics interface used by benchmarks and later concurrency control.
- [x] Redact credentials and tokens at the logging boundary; add tests that attempt to emit secrets through errors, spans, and HTTP diagnostics.

### 0.4 Test infrastructure

- [x] Build the in-process fake registry with controllable latency, bandwidth, status failures, truncation, and authentication.
- [x] Build a fixture-package generator for tarballs, manifests, dependencies, lifecycle scripts, integrity values, and malicious archive cases.
- [x] Add temporary project/store helpers, filesystem snapshots, and a CLI end-to-end harness.
- [x] Add deterministic clock/RNG injection, property-test scaffolding, and fuzz targets for parsers and archive handling.
- [x] Prove a fixture package can be installed end-to-end without public network access.

### 0.5 Benchmark skeleton

- [x] Define and pin small, medium, large, monorepo, and native-package fixtures plus cold, warm-store, warm-lockfile, reinstall, offline, branch-switch, and CI scenarios.
- [x] Implement isolated competitor runs for npm, pnpm, Yarn, Bun, and a stub `jsm`; capture exact versions and environment metadata.
- [x] Add repeatable network shaping and machine-readable JSON plus Markdown benchmark reports with history/trend comparison.
- [x] Run the harness end-to-end and save the initial baseline; record the method and any targets that need calibration before becoming CI gates.

**Phase 0 gate**

- [x] Clean checkout builds, tests, and passes required CI checks on Linux, macOS, and Windows.
- [x] Hermetic fake-registry install and benchmark report work; benchmark inputs and tool versions are reproducible.

> Evidence: [PR #2](https://github.com/nexuss0781/JSM/pull/2), merged at `16cefcac425954094e135fe89b4b4e9cc7542715`; [CI run 37509059359](https://github.com/nexuss0781/JSM/actions/runs/37509059359) passed all five required jobs.

## Phase 1 — Core Engine

### 1.1 CLI skeleton

- [x] Implement the global flags, command aliases, CI detection, non-interactive/frozen-lockfile behavior, progress rendering, and non-TTY output.
- [x] Add typo suggestions and graceful SIGINT/SIGTERM cancellation with defined exit codes and temp-data cleanup.
- [x] Snapshot-test help and verify cancellation never exposes partial project state.

### 1.2 Configuration system

- [x] Implement the documented precedence across CLI, `JSM_*`, project/workspace config, user config, and defaults.
- [x] Parse `.npmrc` registry/scope/auth/proxy/TLS settings and environment expansion with documented precedence.
- [x] Add `config get|set|list|delete` scopes, validation with file/line/key diagnostics, JSON Schema, and precedence/invalid-input tests.

### 1.3 Manifest handling

- [x] Parse and preserve supported dependency, script, bin, engine, platform, workspace, override, export, and file fields.
- [x] Implement minimal-diff edits that preserve formatting, key order, indentation, and trailing newline.
- [x] Add dependency-type and exact/prefix save flags; test round trips against a real-manifest corpus and malformed input.

### 1.4 Semver engine

- [x] Implement npm-compatible range grammar, comparator intersection/subset/simplification, prerelease rules, and build metadata handling.
- [x] Differential-test generated inputs against the reference npm `semver` implementation; add parser fuzzing and pathological-input tests.
- [ ] Meet the specification's corpus agreement and 24-hour fuzz-run exit criteria before resolver integration is considered complete.

### 1.5 Registry client

- [x] Implement pooled HTTPS/HTTP2-capable client, abbreviated packuments with full-metadata fallback, ETag/Last-Modified cache, and offline behavior.
- [x] Add scoped registry authentication, secret-safe credential handling, proxy/custom CA support, timeouts, retry/backoff, and `Retry-After` handling.
- [x] Sanitize all registry metadata before path use; abstract the registry behind a trait for later mirrors/indexes.
- [x] Test 304, flaky 5xx, timeout, truncation, auth failure, and cache/offline behavior using the fake registry.

### 1.6 Store: content-addressed storage

- [x] Implement platform-default store paths and overrides through flag, environment, and config.
- [x] Implement versioned store layout and sharded SHA-512 blob paths; resolve the open default/per-volume store policy in an ADR.
- [x] Implement verified, idempotent temp-write/fsync-policy/atomic-rename blob publication and read-only blob permissions.
- [x] Implement per-volume placement or safe link fallback and the specified Store API.
- [x] Test concurrent writers, existing-blob skip, invalid hash rejection, and killed-writer recovery.

### 1.7 Store: package manifests

- [x] Store manifests keyed by package name/version/tarball integrity with file path, hash, mode, size, symlink, and package metadata records.
- [x] Validate paths, symlink targets, duplicate/case-colliding paths, device entries, and non-standard archive roots before commit.
- [x] Make the package manifest the commit point and implement a compact/fast read path for large manifests.
- [x] Pass malicious archive tests for traversal, absolute paths, symlink escape, duplicate paths, and decompression bombs.

### 1.8 Fetch and extract pipeline

- [x] Stream response bytes through integrity verification, decompression, tar parsing, hashing, CAS writes, and manifest commit without retaining a full tarball.
- [x] Bound channels, buffers, per-package and whole-install memory; enforce entry-count, uncompressed-size, path-length, and compression-ratio limits.
- [x] Add safe resumable-download support where available and per-package progress events.
- [x] Test truncated downloads, hash mismatches, limits, cancellation, memory ceiling, and absence of partially committed packages.

### 1.9 Resolver baseline

- [x] Integrate PubGrub with highest-version default, exact/range/dist-tag resolution, dependency-cycle handling, and lazy parallel metadata retrieval.
- [x] Ensure identical metadata and inputs produce identical graphs independent of response timing; report structured conflict derivations.
- [x] Add property tests that every selected package satisfies all constraints and deterministic repeated-run tests.

### 1.10 Lockfile baseline

- [x] Implement deterministic, sorted `jsm.lock` serialization with importers, packages, integrity, URL, dependency edges, engine/platform fields, and format/tool versions.
- [x] Resolve package-instance identity and lockfile representation for peer-context instances before freezing the format; document in an ADR.
- [x] Detect manifest/lockfile staleness; implement newer-major rejection, `--frozen-lockfile`, and `--no-lockfile` semantics.
- [x] Test byte-identical output under shuffled inputs and stale cases for add, remove, and changed ranges.

### 1.11 Isolated linker baseline

- [x] Build the `.jsm` virtual-store layout with direct-dependency top-level links and internal dependency-edge links; write linker state with lockfile hash/settings.
- [x] Probe and use reflink, hardlink, then copy fallback; enforce copy/reflink for mutable/script/patch outputs and reject unsafe paths again at link time.
- [x] Incrementally update changed entries, preserve the previous valid tree on failure, and remove orphans only after successful commit.
- [x] Verify Node can load declared dependencies and undeclared dependencies are not accidentally resolvable.

### 1.12 Bin links

- [x] Support string/map `bin` and `directories.bin`; create Unix executables and Windows `.cmd`/PowerShell shims.
- [x] Define deterministic conflict resolution and warnings; document when shebangs are rewritten.
- [ ] Test executable invocation and conflict behavior on all supported CI operating systems.

### 1.13 Baseline commands

- [x] Implement `init`, `add`, `install`, `remove`, `run`, `exec`, basic `list`, and basic `why` with documented arguments and aliases.
- [x] Implement versioned `--json` output for each baseline command and schema/snapshot tests.
- [x] Add end-to-end tests for success, missing package, network/integrity failures, frozen-lockfile errors, and script execution behavior.

### 1.14 Integrity and safety baseline

- [x] Verify SHA-512 tarball integrity and blob hashes before content is committed or linked; make any SHA-1 exception explicit, opt-in, and warned.
- [x] Reject traversal and symlink escapes during both extraction and linking.
- [x] Ensure dependency lifecycle scripts cannot execute in the Phase 1 binary; test the absence of an execution path.

**Phase 1 gate**

- [ ] All baseline commands pass fake-registry end-to-end tests and meet the sub-phase exit criteria.
- [x] Exercise the real-registry top-100 package acceptance set and publish a benchmark against npm and pnpm.
- [x] Confirm no implicit dependency script execution and no project-visible unverified bytes.

## Phase 2 — Store Management and Safety

### 2.1 Reference registry

- [ ] Select the embedded DB through an ADR; implement transactional project/package references and install metadata.
- [ ] Track lockfile hashes, paths/project IDs, package references, size, stored/used timestamps, and reference counts.
- [ ] Detect stale, moved, or deleted projects and test randomized add/remove/move/delete sequences against ground truth.

### 2.2 Store CLI

- [ ] Implement `store path|status|list|versions|info|add|remove|usage` and remote `versions` with sorting/filtering and reference/build metadata.
- [ ] Add JSON output to every command; implement destructive `--dry-run`, confirmation prompts, and `--yes` behavior.
- [ ] Test referenced-version refusal, actionable usage output, and equality of dry-run prediction and actual removal effect.

### 2.3 Garbage collection and pinning

- [ ] Implement `store prune`, policy/LRU `store gc`, pin/unpin/list, and logical/physical reclaimed-byte reporting.
- [ ] Implement blob mark-and-sweep under a maintenance lease; protect installs with package/read leases and preserve all referenced/pinned content.
- [ ] Stress concurrent install and GC and verify no referenced manifest/blob or materialized tree is lost.

### 2.4 Verify and repair

- [ ] Implement metadata-only and full verification for blobs/manifests, missing/extra/mismatched files, and progress/parallelism.
- [ ] Implement quarantine and repair/re-fetch behavior; detect linked-file mutation and avoid unsafe hardlink assumptions.
- [ ] Inject bit flips, truncation, and deletion; prove detection, repair, and subsequent install recovery.

### 2.5 Cross-process concurrency

- [ ] Choose and document the lock primitive and lock hierarchy; implement fine-grained package and maintenance locks with timeout/fairness diagnostics.
- [ ] Implement per-identity single-flight tarball downloads and stale-owner detection/recovery.
- [ ] Detect or warn on filesystems with weaker locking guarantees; stress 50+ processes with overlapping dependency sets and verify one download per identical tarball.

### 2.6 Transactional installs and crash recovery

- [ ] Implement project staging and atomic swap where supported; provide journaled rollback on platforms without atomic directory replacement.
- [ ] Recover incomplete journals on next invocation and remove aged orphan temp data without touching live operations.
- [ ] Add crash injection at every transaction/pipeline boundary and verify the last good state survives and the next install converges.

### 2.7 Machine-readable output and shell support

- [ ] Define and document stable `jsm.v1.<command>` schemas and NDJSON progress/event streams.
- [ ] Add schemas for every command and automated compatibility/snapshot checks that block accidental breaking changes.
- [ ] Generate bash, zsh, fish, PowerShell, and elvish completions, including dynamic local package/project completions; add topic help pages.

### 2.8 Doctor

- [ ] Implement checks for store integrity, link capabilities, volumes, paths/case behavior, locks, proxy/CA, registry reachability, clock, disk, platform-specific environment, and installed Node/jsm versions.
- [ ] Return status, explanation, and remedy for every check; add safe `--fix` and redacted `--report`.
- [ ] Give each check positive and negative fault-injection tests; verify report redaction.

### 2.9 Merge-friendly lockfile

- [ ] Confirm merge-oriented lockfile layout and implement `lock verify` and a parser-based three-way `lock merge`.
- [ ] Add `lock install-merge-driver` setup for `.gitattributes` and Git configuration with safe, documented behavior.
- [ ] Test parallel additions, conflicting upgrades, malformed inputs, and merge results against manifests.

**Phase 2 gate**

- [ ] All store commands, JSON contracts, diagnostics, and lock merge tests pass.
- [ ] Multi-process stress and crash-injection criteria in `PHASE.md` pass; GC never removes live/in-flight data.

## Phase 3 — Performance and Time Travel

### 3.1 Pipelined install scheduler

- [ ] Start fetches as soon as resolution commits a package version; connect stages with bounded channels and backpressure.
- [ ] Add critical-path prioritization and isolated network, CPU/decompression/hash, and I/O scheduling.
- [ ] Begin linking as soon as safe dependencies are ready; verify deterministic output independent of schedule; compare against phased baseline.

### 3.2 Adaptive concurrency and hedging

- [ ] Implement per-host AIMD based on latency percentiles, throughput, errors, timeouts, and stream saturation.
- [ ] Add bounded hedging with loser cancellation, retry-after/rate-limit compliance, and automatic plus user-configurable concurrency.
- [ ] Simulate latency, packet loss, and 429/503 responses; show tail-latency improvement without request storms; expose controller metrics/traces.

### 3.3 Skip-write extraction and fast index

- [ ] Maintain a lazy in-memory/on-disk blob index with Bloom-filter negative checks.
- [ ] Skip writes for existing hashes during extraction and report bytes avoided.
- [ ] Verify with I/O counters that adjacent-version upgrades only write new or changed files.

### 3.4 Materialization cache

- [ ] Define cache key from lockfile, linker mode, platform, patch set, and build keys; document all invalidation inputs.
- [ ] Cache a layout/recipe and clone the tree using available filesystem primitives; add cheap validation and store-change invalidation.
- [ ] Add LRU size bounds and `cache materialization list|clear`; benchmark 1,000-package hit against the calibrated budget.

### 3.5 Platform-specific I/O optimization

- [ ] Probe/caches capabilities per store/project volume pair and keep portable fallback behavior.
- [ ] Implement gated Linux reflink/copy-range and optional io_uring paths, macOS clonefile/copyfile paths, and Windows block clone/hardlink/long-path paths.
- [ ] Add per-platform microbenchmarks and tests proving optimized paths do not change content or failure semantics.

### 3.6 Store-aware resolution

- [ ] Add `--prefer-store` and config behavior that selects the highest eligible local version without violating ranges, explicit pins, or lockfile.
- [ ] Define offline, update, and store-preference interactions; make offline use cached metadata/store only.
- [ ] Test identical store state yields deterministic choices and report packages satisfied locally.

### 3.7 History, undo, snapshots, and switch

- [ ] Persist content-addressed before/after lockfile history for each successful mutation and provide configurable retention/pinning.
- [ ] Implement history, undo/redo, named snapshots, and package version switch as transactions.
- [ ] Verify exact lockfile restoration and store-only undo/redo when content is present; test add/upgrade/switch/restore sequences.

### 3.8 Hoisted linker

- [ ] Resolve default linker decision by ADR before changing documented behavior; implement deterministic npm-compatible hoisting and conflict handling.
- [ ] Add `--linker=hoisted`, per-project configuration, exclusions, and public-hoist patterns while reusing safe store/link primitives.
- [ ] Run known flat-layout compatibility projects and ensure isolated mode remains the safe default unless the ADR says otherwise.

### 3.9 Conflict explanation and suggestions

- [ ] Render PubGrub derivations as readable dependency/range chains; expose structured conflict JSON and `--explain`.
- [ ] Generate candidate upgrade/downgrade/override/peer-range suggestions without presenting guesses as guaranteed fixes.
- [ ] Review 30 real conflict cases for accuracy, clarity, and actionable guidance.

**Phase 3 gate**

- [ ] Measured Phase 3 improvement and calibrated cold/warm/materialization budgets pass without determinism or state-safety regressions.
- [ ] Undo/redo/snapshot, linker, adaptive concurrency, and cache invalidation suites pass.

## Phase 4 — Real-World Compatibility

### 4.1 Peer dependencies

- [ ] Resolve peer dependencies in dependent context; key virtual instances by resolved peer set and deduplicate identical contexts.
- [ ] Implement optional peer metadata, configurable auto-install, strict-peer policy, bounded path names, and actionable warnings/errors.
- [ ] Compare behavior against curated React, ESLint, and Babel peer-resolution cases.

### 4.2 Optional and platform-filtered dependencies

- [ ] Filter OS/CPU/libc before download; record skipped optional packages and make optional build failures non-fatal.
- [ ] Implement clear failures for excluded required packages, configurable engine policy, and cross-platform lockfile selection.
- [ ] Test native optional packages and supported architecture selection on all platform CI targets.

### 4.3 Dependency protocols

- [ ] Implement `npm:` aliases, `file:`, `link:`, direct tarball URLs, and canonical specifier validation.
- [ ] Implement `git+https`, `git+ssh`, and `github:` with locked commit resolution and isolated, policy-controlled `prepare` execution.
- [ ] Record source integrity/commit in the lockfile/store and prove replay from lockfile is deterministic.

### 4.4 Script policy

- [ ] Implement default-deny dependency lifecycle scripts and configured root-script behavior/`--ignore-scripts`.
- [ ] Implement interactive/non-interactive `approve-scripts`; bind approvals to package/range and script-content hash; support documented storage choices.
- [ ] Warn with skipped scripts and remediation; test that deny mode cannot run dependency scripts and script changes invalidate prior approval.

### 4.5 Local build cache

- [ ] Implement build key over package integrity, script hash, runtime ABI, platform, toolchain, allowlisted env, dependency build keys, and sandbox policy.
- [ ] Capture approved build outputs separately from immutable store payloads and store them safely under the build key.
- [ ] Add `cache build list|info|clear|verify`, hit/miss reporting, and cross-project cache reuse tests.

### 4.6 Parallel build scheduler

- [ ] Schedule dependency builds topologically within CPU, memory, and configured concurrency limits.
- [ ] Save per-package logs; sanitize environment; provide standard npm compatibility variables, timeouts, and process-tree cancellation.
- [ ] Verify required failures roll back installs, optional failures are recorded, and cancellation leaves no orphan processes.

### 4.7 Insight and diff commands

- [ ] Implement store-backed file/dependency/script/exports/size diff for versions and optional content diff.
- [ ] Add dependency-path and size explanation, duplicate reports with safe suggestions, unused-dependency analysis with ignore/config references, and outdated views.
- [ ] Compare diff against extracted tarballs and measure `unused` precision on a labeled corpus.

### 4.8 Package executor (`jsm x`)

- [ ] Resolve and fetch packages into the shared store and create isolated, lockfile-keyed ephemeral environments.
- [ ] Add bin disambiguation, multiple `--package` support, TTL for floating ranges, and `--refresh`.
- [ ] Enforce script policy/sandbox availability and benchmark warm invocation against the specified budget.

### 4.9 Release cooldown and resolution modes

- [ ] Implement minimum release age, per-package exceptions, exclusion explanations, lowest/lowest-direct, and time-based `--before` resolution.
- [ ] Document precedence with locks, explicit updates, and store preference.
- [ ] Test deterministic behavior using fixture publish timestamps, including cases where cooldown leaves no eligible version.

### 4.10 Compatibility corpus and CI

- [ ] Build isolated top-1,000 package install/load corpus and 100+ real repository corpus with their test scripts.
- [ ] Compare dependency trees/runtime behavior with npm and pnpm; classify known differences and block unreviewed regressions.
- [ ] Run nightly/release-candidate tests across Linux glibc/musl, macOS, and Windows; publish dashboard and known issues.
- [ ] Reach 95% Corpus A at Phase 4 gate and define tracking to 98% for v1.0.

### 4.11 Registry authentication, proxy, and enterprise networking

- [ ] Implement scoped/per-registry bearer/basic and npm auth compatibility plus credential-helper integration where supported.
- [ ] Add HTTP(S) proxy, `NO_PROXY`, custom CA, client certificates, and registry mirror/fallback health policy.
- [ ] Define and secure any CI OIDC token extension point; never persist credentials in lockfiles, store, logs, or diagnostics.
- [ ] Test authenticated fake registry, proxy, custom-CA endpoint, and failover paths.

**Phase 4 gate**

- [ ] Compatibility, peer, optional/platform, script-deny, build-cache, and enterprise-networking acceptance suites pass.
- [ ] Corpus A meets the 95% gate; exceptions and regressions are documented.

## Phase 5 — Production Readiness (v1.0)

### 5.1 Workspaces and monorepos

- [ ] Discover workspace globs, exclusions, local packages, and supported `workspace:` ranges with publish-time rewriting.
- [ ] Implement one importer-aware lockfile, filters (including git-changed/dependency/dependent forms), and workspace-aware commands.
- [ ] Implement recursive script execution with topological ordering, parallelism, streaming, and bail policy; test 100-package fixture.

### 5.2 Catalogs, overrides, and resolutions

- [ ] Implement named/default catalogs and `catalog:` protocol.
- [ ] Implement package/parent/range overrides, resolutions, dependency references, unused-override warnings, and update-by-catalog.
- [ ] Document and test precedence, nested selectors, and scoped package behavior.

### 5.3 Patching

- [ ] Implement safe extraction to a working copy, patch diff/commit/remove, and content-hash registration.
- [ ] Apply patches only to per-project mutable materializations; include patch hashes in lockfile/cache keys.
- [ ] Test store immutability and clear hunk-level diagnostics when patches stop applying.

### 5.4 Lockfile importers and migration

- [ ] Implement importers for package-lock v1/v2/v3, shrinkwrap, Yarn Classic/Berry, pnpm, and Bun formats as specified.
- [ ] Implement `migrate --from`, dry-run/diff, integrity checks, precise untranslated-entry report, optional exporters, and opt-in old-lockfile removal.
- [ ] Import applicable ecosystem configuration without silently overriding user settings; test Corpus B resolution preservation target.

### 5.5 Provenance and signature verification

- [ ] Implement registry signature verification and key rotation/cache handling.
- [ ] Verify npm provenance/Sigstore bundles including certificate, transparency inclusion, and builder identity.
- [ ] Implement off/warn/require policy, exceptions, non-regression signal, compact lockfile status, and offline verification with cached material.
- [ ] Test valid, tampered, expired, revoked, absent, and offline cases.

### 5.6 Audit and advisories

- [ ] Build/sync local OSV and GitHub advisory data incrementally and support offline scans.
- [ ] Implement `audit` filters, dependency paths, threshold exit behavior, justified expiring exceptions, and SARIF/JSON output.
- [ ] Test results against reference scanners on fixtures and ensure `--fix` review/application behavior is explicit.

### 5.7 Sandboxed script execution

- [ ] Resolve and document per-OS sandbox capability/default policy before enabling execution; degrade with explicit warning only where the spec allows.
- [ ] Implement Linux, macOS, and Windows least-privilege sandbox adapters with no network by default and bounded read/write paths.
- [ ] Include sandbox policy in build keys and implement explicit `--no-sandbox` opt-out with warning.
- [ ] Run escape tests for network, paths, credentials, and process limits; test common native builds on supported platforms.

### 5.8 Prebuilt binary resolution

- [ ] Implement resolution order: local cache, trusted remote cache when enabled, platform packages, recognized prebuild conventions, then controlled compilation.
- [ ] Fetch prebuilds through verified CAS pipeline; never permit arbitrary lifecycle script downloads to bypass controls.
- [ ] Test top native packages/platforms and report cache/prebuilt/compiled result.

### 5.9 Script change review

- [ ] Persist approved script hashes and detect changes/new lifecycle scripts on package update.
- [ ] Revoke stale approvals and show reviewable script/file diffs before execution.
- [ ] Test unchanged, modified, newly added, and removed script cases.

### 5.10 Update review report

- [ ] Implement per-package version/size/dependency/script/exports/provenance/advisory/age change report for `update --review` and interactive selection.
- [ ] Support table and versioned JSON output and reuse local store diff where possible.
- [ ] Test reports with controlled upgrade fixtures covering every reported change category.

### 5.11 Store export and import

- [ ] Implement minimal lockfile/package store bundle with integrity manifest and optional signature; add incremental bundle support.
- [ ] Verify and merge imports idempotently and add offline install-from-bundle flow.
- [ ] Prove export/import installation succeeds in a disconnected CI fixture and tampering is rejected.

### 5.12 Production slimming

- [ ] Implement production-only installs, conservative opt-in slim rules/allow-deny config, and self-contained workspace deploy output.
- [ ] Report size before/after; preserve files required by runtime exports and package semantics.
- [ ] Run runtime smoke tests for slimmed compatibility corpus outputs.

### 5.13 Release engineering

- [ ] Implement reproducible release builds, signed artifacts/checksums/SBOM, CI-only tagged release candidate, and supported distribution channels.
- [ ] Add verified self-update with stable/beta channels; define CLI, JSON, lockfile, and store versioning/deprecation/migration policy.
- [ ] Add `SECURITY.md`, vulnerability disclosure process, and dependency-update automation.
- [ ] Pass migration compatibility tests across supported prior formats/versions.

### 5.14 Documentation

- [ ] Write getting-started, generated command/config references, store/lockfile, security, troubleshooting, architecture, and contributor documentation.
- [ ] Add npm/Yarn/pnpm/Bun migration, workspace, CI, Docker, and cache recipes.
- [ ] Validate command/config examples, links, schemas, and docs snippets in CI; ensure every command and config key is documented.

**Phase 5 / v1.0 gate**

- [ ] All Phase 5 exit criteria pass; Corpus A reaches 98%; ratified Appendix D budgets pass.
- [ ] Security review, required fuzzing, crash/concurrency/platform suites, release signing, SBOM, migration checks, and documentation tests pass.
- [ ] Produce a signed release candidate from a tagged commit by CI alone; publish compatibility and benchmark evidence.

## Phase 6 — Ecosystem and Scale (post-v1.0)

### 6.1 Store-as-registry (`jsm serve`)

- [ ] Implement npm-compatible read API with upstream fallback/cache, configurable policy, token auth, TLS, rate limits, and access logging.
- [ ] Add health/metrics endpoints and integrity-verified optional LAN/peer fetch.
- [ ] Test two clients sharing an upstream fetch and verify unauthorized clients cannot retrieve protected data.

### 6.2 Remote build cache

- [ ] Implement S3-compatible, GCS, Azure Blob, HTTPS, and `jsm serve` backends with read-only/read-write roles.
- [ ] Sign and verify build-key artifacts; implement key rotation, namespace isolation, TTL, audit logs, and poisoning defenses.
- [ ] Test CI-to-developer reuse, tampered artifacts, unauthorized access, and backend failure behavior.

### 6.3 Sparse registry index

- [ ] Specify and version a sharded incremental compressed index with delta/ETag synchronization.
- [ ] Implement client cache and optional generation/server mode; fall back to the standard registry metadata API.
- [ ] Prove warm-index large-fixture resolution uses zero per-package network requests and remains correct after updates.

### 6.4 Merkle verification and store sync

- [ ] Maintain Merkle roots/subtrees for package manifests and blobs and define signed root behavior for bundles/served stores.
- [ ] Implement fast verification and store/bundle synchronization by subtree comparison.
- [ ] Test tampering/divergence detection and meet the unchanged-store verification improvement target.

### 6.5 Optional daemon (`jsmd`)

- [ ] Implement versioned local socket/named-pipe protocol with user-only permissions and no remote control interface.
- [ ] Add opt-in metadata warming, project watch/prefetch, optional git hooks, idle maintenance, resource limits, and daemon diagnostics.
- [ ] Verify every CLI operation has equivalent daemon-free behavior and branch checkout prefetch meets its acceptance target.

### 6.6 Capability analysis

- [ ] Analyze and cache package capabilities for network, process, filesystem, environment, native loading, dynamic code, and obfuscation indicators.
- [ ] Surface changes in update/diff and implement configurable policy plus `capabilities` output.
- [ ] Measure precision/recall on benign and safe malicious-sample datasets; add false-positive controls.

### 6.7 Typosquat and anomaly warnings

- [ ] Implement popular-name edit/confusable/scope-impersonation checks and metadata anomaly hints for new dependencies.
- [ ] Bundle/update popularity data and make strictness configurable with offline behavior.
- [ ] Test known typosquat samples and quantify false positives on the popular-name corpus.

### 6.8 Zero-install script running

- [ ] Parse inline dependency declarations and resolve them into isolated cached ephemeral environments.
- [ ] Support optional reproducibility sidecar and configured Node/Bun/Deno runtime/TypeScript loader selection.
- [ ] Test first run with no project files and warm rerun against the specified budget.

### 6.9 Advanced deploy and container optimization

- [ ] Implement deterministic dependency/app layers, Docker-oriented install mode, and documented BuildKit store-cache mounts.
- [ ] Add CycloneDX/SPDX SBOM generation for deploy output.
- [ ] Benchmark reference Dockerfiles against npm/pnpm for build time and image size; publish methodology.

**Phase 6 gate**

- [ ] All Phase 6 sub-phase acceptance criteria pass; service threat models, auth/signature checks, and integration tests are reviewed.
- [ ] Serve, remote cache, sparse index, and daemon are optional and can be disabled without affecting core correctness.

## Phase 7 — Advanced Extensions (post-v1.0)

### 7.1 Chunk-level store with compression

- [ ] Implement content-defined chunking and zstd for eligible large files, retaining whole-file blobs for small files.
- [ ] Implement transparent reconstruction, integrity, resumable migration, rollback, and bounded cache behavior.
- [ ] Test disk savings on many package versions and keep materialization within the approved regression tolerance.

### 7.2 Interactive TUI (`jsm ui`)

- [ ] Implement dependency/store/update/advisory/history views, search, navigation, accessible themes, and confirmed destructive actions.
- [ ] Provide clear non-TTY fallback and snapshot tests; run usability task set.

### 7.3 Node runtime management

- [ ] Implement verified runtime install/use/list/remove/which and shared-store representation.
- [ ] Honor runtime pins and environment files; add shims/command selection and feed detected ABI into build keys.
- [ ] Test checksum/signature failures and instant switching among installed versions.

### 7.4 Virtual linker

- [ ] Resolve platform feasibility and permission/security model in an ADR before implementation.
- [ ] Implement opt-in FUSE/ProjectedFS-compatible overlay with lazy materialization and tested file/watch/symlink semantics.
- [ ] Maintain isolated-linker fallback and validate representative dev server, bundler, and language-server projects.

### 7.5 Editor and tooling integrations

- [ ] Publish versioned schemas for config, lockfile, and manifest extensions and expose language-server-friendly diagnostics.
- [ ] Build, test, and document editor extension and CI setup/cache/audit action.
- [ ] Document Renovate/Dependabot behavior and test integrations where supported.

### 7.6 Plugin and extension API

- [ ] Choose and specify WASI or out-of-process JSON-RPC contract, manifest, permissions, and versioning in an ADR.
- [ ] Implement capability enforcement, signature verification, reference plugin, and developer documentation.
- [ ] Test that a plugin can add an approved extension point but cannot bypass integrity, script policy, or sandbox controls.

**Phase 7 gate**

- [ ] Every extension is independently opt-in/feature-flagged and has acceptance, compatibility, security, performance, and rollback evidence.

## Design decisions to record

Track these open questions from `PROJECT.md` as ADRs at the point they block a decision. Do not let unresolved decisions silently become permanent behavior.

- [ ] **Before store/format implementation (0.1, 1.6, 1.10, 2.1):** finalize product/binary naming; store default and per-volume selection; reference DB; package/peer instance identity and lockfile representation.
- [ ] **Before changing linker defaults (3.8):** select compatibility default and document isolated/hoisted trade-offs.
- [ ] **Before script execution release (4.4, 5.7):** confirm policy defaults, approval storage, supported sandbox guarantees, and explicit degraded modes.
- [ ] **Before publishing v1.0 (5.13):** decide license, governance/ownership, distribution channels, signing identity, supported migration window, and release policy.
- [ ] **Before shared infrastructure (6.2–6.4):** settle remote-cache trust/key ownership, registry index hosting/versioning, and cross-store synchronization trust.
- [ ] **Before virtual linker release (7.4):** validate Windows ProjectedFS/alternative feasibility, platform support matrix, and fallback criteria.
- [ ] **Before plugin API release (7.6):** choose plugin execution model, compatibility/version policy, signature/trust roots, and permission model.
