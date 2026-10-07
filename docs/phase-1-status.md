# Phase 1 gate status

**Gate result: PASS** — 52 of 53 checklist items are complete; 1 open follow-up(s) are explicitly non-gating.

Generated `2026-10-07T10:34:01Z` from revision `c8f40bf30f62ef92b5a5b1a0c743c6e542a13316` in `verify` mode.

## Verification results

| Check | Result | Time |
|---|---|---:|
| Phase 1 gate harness tests | passed | 0.197s |
| Benchmark mode tests | passed | 0.150s |
| Python syntax | passed | 0.081s |
| Workspace dependency policy | passed | 0.144s |
| Rust formatting | passed | 0.907s |
| Workspace build | passed | 1.297s |
| Workspace tests | passed | 12.208s |
| Phase 1 CLI fake-registry end-to-end tests | passed | 9.270s |
| Clippy warnings as errors | passed | 0.774s |
| Phase 0 fake-registry testkit suite (not product CLI acceptance) | passed | 0.417s |
| CLI help surface | passed | 3.331s |

## In-scope gaps

None.

## Explicit non-gating follow-ups

- **Deferred, not passed:** Complete the specified uninterrupted 24-hour SemVer fuzz campaign. **[DEFERRED: NON-GATING]** Explicitly deferred for Phase 1 closure on 2026-10-07; the run was stopped, was not completed, and has no evidence report.
  The 24-hour run was not completed and no fuzz evidence report is claimed.

## Baseline CLI commands

| Command | Present in help |
|---|---|
| `init` | yes |
| `add` | yes |
| `install` | yes |
| `remove` | yes |
| `run` | yes |
| `exec` | yes |
| `list` | yes |
| `why` | yes |

## Acceptance evidence

| Evidence | Result | Report | Details |
|---|---|---|---|
| Real-registry top-100 | PASS | `benches/results/phase1-top100/report.json` | 100 package runs passed |
| npm/pnpm/JSM benchmark | PASS | `docs/benchmarks/phase1-baseline/report.json` | JSM, npm, and pnpm benchmark scenarios passed |
| SemVer differential | PASS | `docs/semver-differential.json` | 90,800 SemVer cases agree 100% with npm |
| 24-hour SemVer fuzz | DEFERRED | `docs/phase1-semver-fuzz-24h.json` | 24-hour run not completed; explicitly deferred as non-gating; no valid evidence report is claimed |
| 2,000-package memory | PASS | `benches/results/phase1-memory-2000/report.json` | 2,000 packages installed within 512 MiB (peak 27.8 MiB) |

## Phase 1 checklist

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
- [x] Meet the reference corpus agreement criterion before resolver integration (90,800/90,800 cases).
- [ ] Complete the specified uninterrupted 24-hour SemVer fuzz campaign. **[DEFERRED: NON-GATING]** Explicitly deferred for Phase 1 closure on 2026-10-07; the run was stopped, was not completed, and has no evidence report.
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

- [x] Integrate Resolvo with highest-version default, exact/range/dist-tag resolution, dependency-cycle handling, and lazy parallel metadata retrieval.
- [x] Ensure identical metadata and inputs produce identical graphs independent of response timing; report structured conflict details.
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
- [x] Test executable invocation and conflict behavior on all supported CI operating systems.
### 1.13 Baseline commands

- [x] Implement `init`, `add`, `install`, `remove`, `run`, `exec`, basic `list`, and basic `why` with documented arguments and aliases.
- [x] Implement versioned `--json` output for each baseline command and schema/snapshot tests.
- [x] Add end-to-end tests for success, missing package, network/integrity failures, frozen-lockfile errors, and script execution behavior.
### 1.14 Integrity and safety baseline

- [x] Verify SHA-512 tarball integrity and blob hashes before content is committed or linked; make any SHA-1 exception explicit, opt-in, and warned.
- [x] Reject traversal and symlink escapes during both extraction and linking.
- [x] Ensure dependency lifecycle scripts cannot execute in the Phase 1 binary; test the absence of an execution path.
### Phase 1 gate

- [x] All baseline commands pass fake-registry end-to-end tests and all in-scope sub-phase criteria are met; follow-ups explicitly marked non-gating are recorded separately.
- [x] Exercise the real-registry top-100 package acceptance set and publish a benchmark against npm and pnpm.
- [x] Confirm no implicit dependency script execution and no project-visible unverified bytes.
