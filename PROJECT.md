# jsm: A Fast JavaScript Package Manager with a Shared, Multi-Version Store

> **Status:** Draft v0.1 (specification)
> **Working name:** `jsm` (binary), `jsmd` (optional daemon)
> **Implementation language:** Rust
> **Target platforms:** Linux (x64, arm64, glibc and musl), macOS (x64, arm64), Windows (x64, arm64)

---

## Table of Contents

1. [Vision and Positioning](#1-vision-and-positioning)
2. [Goals and Non-Goals](#2-goals-and-non-goals)
3. [Core Pillars](#3-core-pillars)
4. [Target Users](#4-target-users)
5. [System Architecture](#5-system-architecture)
6. [The Shared Store](#6-the-shared-store)
7. [Install Pipeline and Concurrency](#7-install-pipeline-and-concurrency)
8. [Dependency Resolution](#8-dependency-resolution)
9. [Linking and `node_modules` Layout](#9-linking-and-node_modules-layout)
10. [Build System and Install Scripts](#10-build-system-and-install-scripts)
11. [Security Model](#11-security-model)
12. [CLI Specification](#12-cli-specification)
13. [Configuration](#13-configuration)
14. [Lockfile Specification](#14-lockfile-specification)
15. [Feature Catalog and Priorities](#15-feature-catalog-and-priorities)
16. [Performance Targets and Benchmarking](#16-performance-targets-and-benchmarking)
17. [Reliability, Compatibility and Platform Support](#17-reliability-compatibility-and-platform-support)
18. [Testing Strategy](#18-testing-strategy)
19. [Developer Experience](#19-developer-experience)
20. [Roadmap](#20-roadmap)
21. [Risks and Mitigations](#21-risks-and-mitigations)
22. [Success Metrics](#22-success-metrics)
23. [Open Questions](#23-open-questions)
24. [Appendix A: Recommended Crates](#appendix-a-recommended-crates)
25. [Appendix B: Glossary](#appendix-b-glossary)

---

## 1. Vision and Positioning

`jsm` is a JavaScript package manager written in Rust, built around two ideas:

1. **Speed.** The fastest installs of any package manager, in cold, warm, and offline conditions.
2. **A shared workspace store.** Packages are never installed into project source trees as independent copies. Every package, in every version, is stored exactly once in a machine-wide content-addressed store. Projects consume packages by linking to that store.

Two versions of the same package (for example `react@18.2.0` and `react@19.0.0`) coexist in the store. Switching a project from one to the other is a relink operation, not a download.

### One-line pitch

> Install once, reuse everywhere, switch versions instantly.

### Positioning against existing tools

| Tool | Strength | Gap `jsm` targets |
|---|---|---|
| npm | Ubiquity, compatibility | Slow, duplicates files per project |
| Yarn (Berry) | Plug'n'Play, workspaces | PnP compatibility friction |
| pnpm | Global store, strict layout | No version history, no build cache, no sandboxing, Node-based |
| Bun | Very fast installs | Tied to one runtime, limited store management |
| `jsm` | Store-first design in Rust | Multi-version store management, time travel, shared build cache, sandboxed scripts, adaptive pipeline |

---

## 2. Goals and Non-Goals

### 2.1 Goals

- **G1.** Faster than npm, Yarn, pnpm, and Bun on cold install, warm-store install, and warm-lockfile install benchmarks.
- **G2.** A single machine-wide store; each unique file stored once; each package version stored once.
- **G3.** First-class multi-version coexistence with a complete CLI to list, fetch, inspect, pin, and remove specific versions.
- **G4.** Compatibility with the npm registry, `package.json`, `.npmrc`, and the broad Node.js ecosystem.
- **G5.** Safe by default: no implicit script execution, verified integrity, auditable changes.
- **G6.** Crash-safe, concurrency-safe operation across multiple processes.
- **G7.** Excellent developer experience: clear errors, machine-readable output, shell completions, helpful diagnostics.

### 2.2 Non-Goals (v1)

- Replacing the Node.js runtime or implementing a JavaScript runtime.
- Hosting a public registry.
- Bundling, transpiling, or test running.
- Supporting non-npm ecosystems (Deno `jsr:` can be considered later).
- A custom module resolution hook that requires modified Node (Plug'n'Play style) in v1.

---

## 3. Core Pillars

### Pillar 1: Speed

Speed is a product requirement, measured and enforced in CI.

- Written in Rust with an async runtime and a pipelined install engine.
- Streaming download, decompress, hash, and write with no temporary tarballs.
- Adaptive concurrency instead of fixed limits.
- Skip-write extraction: files already in the store are never rewritten.
- Whole-tree materialization cache for repeat installs.
- Lockfile fast path: a valid lockfile bypasses resolution entirely.

### Pillar 2: Shared Workspace Store

- One content-addressed store per machine (configurable per user, per team, or per CI cache).
- Package payloads are stored once and linked into projects (reflink, then hardlink, then copy).
- Exact `name@version@integrity` already in the store is **never downloaded again**.
- A different version is fetched (or reused if present), and **older and newer versions are retained** until explicitly removed or garbage collected.
- Projects record exactly which store entries they use, enabling safe removal and cleanup.

---

## 4. Target Users

| Persona | Needs |
|---|---|
| Application developer | Fast installs, reliable switching between branches and versions |
| Monorepo maintainer | Workspaces, catalogs, filtered commands, deterministic lockfile |
| Library author | Test across dependency versions, lowest-version resolution, quick version flips |
| CI/CD engineer | Cacheable store, offline installs, reproducible builds, small Docker images |
| Security-conscious team | Script allowlists, provenance checks, change review, release cooldown |
| Student or hobbyist | Disk savings, zero-config scripts, simple CLI |

---

## 5. System Architecture

### 5.1 High-Level Components

```
                         +----------------------+
                         |        CLI (jsm)      |
                         +----------+-----------+
                                    |
            +-----------------------+------------------------+
            |                       |                        |
   +--------v--------+    +---------v---------+    +---------v---------+
   |  Project Model   |    |   Install Engine   |    |  Store Manager     |
   | package.json,    |    | pipeline scheduler |    | CAS, index, GC,    |
   | workspaces,      |    | fetch/extract/link |    | verify, locks      |
   | lockfile         |    +----+---------+-----+    +---------+---------+
   +--------+--------+         |         |                    |
            |            +-----v---+ +---v--------+   +-------v--------+
            |            | Resolver | | Build/Script|  |  Linker         |
            +----------> | (PubGrub)| | Orchestrator|  | reflink/hardlink|
                         +-----+---+ +---+---------+   +----------------+
                               |         |
                       +-------v---+ +---v-----------+
                       | Registry   | | Sandbox       |
                       | Client +   | | + Build Cache |
                       | Index Cache| +---------------+
                       +-----------+
```

### 5.2 Cargo Workspace Layout

```
jsm/
  Cargo.toml                 # workspace
  crates/
    jsm-cli/                 # clap commands, output formatting, TUI
    jsm-core/                # shared types: PackageId, Integrity, Range, Error
    jsm-registry/            # HTTP client, packument + sparse index, auth
    jsm-resolver/            # PubGrub-based resolution, overrides, peers
    jsm-store/               # CAS, package manifests, locks, GC, verify
    jsm-fetch/               # streaming download/extract pipeline
    jsm-linker/              # isolated/hoisted linkers, materialization cache
    jsm-build/               # script runner, sandbox, build cache
    jsm-lockfile/            # parse/write/merge, importers (npm, yarn, pnpm, bun)
    jsm-workspace/           # monorepo discovery, filtering, catalogs
    jsm-security/            # audit, provenance, capability analysis
    jsm-daemon/              # optional jsmd
    jsm-testkit/             # fake registry, fixtures, benchmark harness
  benches/
  docs/
```

### 5.3 Key Architectural Principles

1. **The store is append-only for content.** Package contents are never mutated after being written. Removal happens only through explicit GC or removal commands.
2. **Projects are views.** A project's `node_modules` is a derived artifact from the lockfile plus the store.
3. **Everything is idempotent.** Re-running any step converges to the same state.
4. **No global lock.** Concurrency control is fine-grained and cross-process.
5. **Streaming over buffering.** Bounded channels and backpressure keep memory flat.

---

## 6. The Shared Store

### 6.1 Location

| Platform | Default path |
|---|---|
| Linux | `$XDG_DATA_HOME/jsm/store` or `~/.local/share/jsm/store` |
| macOS | `~/Library/Application Support/jsm/store` |
| Windows | `%LOCALAPPDATA%\jsm\store` (Dev Drive recommended) |

Override with `JSM_STORE_DIR`, `.jsmrc`, or `--store-dir`. Per-volume stores are created automatically when the project is on a different volume than the default store, so links remain possible.

### 6.2 On-Disk Layout

```
store/
  VERSION                         # store format version
  files/
    sha512/ab/cdef0123...         # file blobs, content-addressed (mode in index)
  packages/
    react/
      18.2.0/
        <integrity>.manifest      # file path -> {hash, mode, size}
        package.json.cache        # parsed metadata for fast access
      19.0.0/
        <integrity>.manifest
  metadata/
    registry.npmjs.org/
      react.json                  # abbreviated packument
      react.etag
  index/
    store.db                      # embedded DB (references, usage, timestamps)
  builds/
    <build-key>/                  # compiled native artifacts
  tmp/                            # write-then-rename staging
  locks/                          # advisory lock files
```

### 6.3 Package Identity

A stored package is identified by:

```
name @ version @ integrity (sha512)
```

For packages with native builds, an additional **build key** identifies compiled variants:

```
build_key = hash(package integrity, node ABI, platform, arch, libc,
                 toolchain fingerprint, allowlisted env vars, script hash)
```

### 6.4 Multi-Version Semantics

- Installing `react@18.2.0` when present: **no network, no extraction**.
- Installing `react@19.0.0` while `18.2.0` exists: fetch `19.0.0`; `18.2.0` is **retained**.
- Upgrade and downgrade only change the project lockfile and links; the store is never reduced as a side effect.
- Identical files across versions are stored once through content addressing.

### 6.5 Reference Registry

The store tracks which projects reference which package versions:

| Field | Purpose |
|---|---|
| `project_path` | Absolute path of the project |
| `lockfile_hash` | Last known lockfile hash |
| `packages[]` | `name@version@integrity` entries used |
| `last_install_at` | Timestamp |
| `last_used_at` (per package) | LRU eviction input |

References are registered on install and verified lazily; stale projects (deleted directories) are detected by `store prune`.

### 6.6 Store Operations

| Operation | Behavior |
|---|---|
| `store add` | Pre-fetch a version into the store |
| `store remove` | Remove a specific version; refuses when referenced unless `--force` |
| `store prune` | Remove versions with no live references |
| `store gc` | Policy-based cleanup by age and size budget (LRU), skipping pinned and referenced entries |
| `store verify` | Re-hash content and validate manifests; report and optionally repair |
| `store export` / `import` | Portable bundles for CI and air-gapped environments |
| `store pin` / `unpin` | Protect entries from GC |

### 6.7 Advanced Store Features

- **Chunk-level dedupe and compression (post-v1).** Content-defined chunking (FastCDC) with zstd compression, materialized on demand.
- **Merkle verification.** A Merkle tree over the store enables fast integrity checks and efficient sync.
- **Store as registry (`jsm serve`).** Serves cached packages over a registry-compatible HTTP API for LAN and CI use.

---

## 7. Install Pipeline and Concurrency

### 7.1 Streaming DAG Pipeline

Install is **not** phased (resolve, then fetch, then extract, then link). It is a streaming pipeline:

```
[Resolve] --decided version--> [Fetch] --bytes--> [Gunzip] --entries--> [Hash+Dedupe]
                                                                              |
                                                          new files --> [Write to CAS]
                                                          known files -> (skip)
                                                                              |
                                                                    [Package Manifest]
                                                                              |
                                                                          [Link]
                                                                              |
                                                                    [Build Scheduler]
```

- The moment a version is selected, its tarball download begins.
- Stages are connected by bounded channels; slow stages apply backpressure.
- A scheduler prioritizes the critical path (packages with the largest transitive subtrees first).

### 7.2 Fast Paths

| Scenario | Fast path |
|---|---|
| Valid lockfile, store populated | Skip resolution, skip network, link only |
| Valid lockfile, store partially populated | Fetch only missing packages |
| Same lockfile hash seen before | Materialization cache: clone the whole tree |
| `--offline` | Resolve and install from store and cached metadata only |

### 7.3 Adaptive Concurrency Controller

- AIMD (additive increase, multiplicative decrease) control loop per host.
- Inputs: latency, throughput, error and timeout rates, HTTP/2 stream saturation.
- **Hedged requests:** if a request exceeds a dynamic tail-latency threshold (for example p95), start a duplicate against the same host or a configured mirror and use the first to complete.
- User-visible overrides: `--network-concurrency`, `--child-concurrency`, `--io-concurrency`.

### 7.4 Skip-Write Extraction

- Each tar entry is hashed as it streams.
- A Bloom filter in front of the on-disk index answers "already stored?" quickly.
- Known blobs are not written. Upgrades touch only changed files.

### 7.5 Cross-Process Safety

- Content writes use temp file, fsync policy, then **atomic rename**.
- Per-package advisory locks, never a global lock.
- **Single-flight downloads:** if another process is downloading the same tarball, wait and reuse its result.
- Crash-safe: an interrupted install leaves the last good state intact. `jsm doctor --fix` repairs orphaned temp data.

### 7.6 Platform I/O Optimizations

| Platform | Optimization |
|---|---|
| Linux | `io_uring` batching (optional), `copy_file_range`, reflink via `FICLONE` |
| macOS | `clonefile`, `copyfile` with `COPYFILE_CLONE` |
| Windows | Block cloning on ReFS/Dev Drive, long-path support, minimized small-file writes, Defender exclusion guidance in `doctor` |

Link priority: **reflink, then hardlink, then copy**, selected per volume and cached.

---

## 8. Dependency Resolution

### 8.1 Resolver

- Based on the **PubGrub** algorithm for correctness and human-readable conflict explanations.
- Full npm semver range support (`^`, `~`, hyphen ranges, `x`-ranges, `||`, prerelease rules).
- Dist-tags (`latest`, `next`), aliases (`npm:`), and protocol specifiers (`workspace:`, `file:`, `link:`, `git+`, tarball URLs).

### 8.2 Resolution Modes

| Mode | Flag | Behavior |
|---|---|---|
| Highest (default) | `--resolution highest` | Newest satisfying version |
| Store-aware | `--prefer-store` | Prefer versions already in the store when they satisfy the range |
| Lowest | `--resolution lowest` | Oldest satisfying version (library authors) |
| Locked | `--frozen-lockfile` | Fail if lockfile would change |
| Offline | `--offline` | Cached metadata and store only |
| Cooldown | `--min-release-age 3d` | Ignore versions newer than the given age |

### 8.3 Registry Metadata

- Abbreviated metadata (`application/vnd.npm.install-v1+json`) with ETag revalidation.
- Optional **sparse, incremental index** (Cargo-style) for near-instant local resolution and offline support.
- Optional **server-side resolution** against a compatible registry or `jsm serve` endpoint.

### 8.4 Peer, Optional, and Platform Dependencies

- Peer dependencies resolved per dependent context; instances keyed by name, version, and peer set (virtual store entries).
- `optionalDependencies` failures are non-fatal and recorded.
- `os`, `cpu`, and `libc` fields filter packages before download, avoiding unnecessary fetches.
- `engines` enforcement configurable (`warn`, `error`, `off`).

### 8.5 Overrides and Patches

- `overrides` / `resolutions` (compatible syntax) and `catalogs` for monorepos.
- `jsm patch <pkg>` and `jsm patch-commit` produce persistent patches applied at link time, without mutating the shared store.

### 8.6 Explainable Failures

When resolution fails, output a derivation tree plus suggested fixes:

```
error: cannot resolve dependencies

  foo@3.0.0 requires react@^19
  your project requires react@^18.2.0

help: upgrade bar to ^4.2.0 (supports react ^18 and ^19)
help: or pin foo to ^2.9.0 (supports react ^18)
```

---

## 9. Linking and `node_modules` Layout

### 9.1 Linker Modes

| Mode | Description | Use case |
|---|---|---|
| `isolated` (default) | pnpm-style virtual store with symlinks; strict, no phantom dependencies | Correctness, disk savings |
| `hoisted` | npm-style flat layout | Tooling that assumes flattening |
| `virtual` (later) | FUSE/ProjectedFS overlay, zero files written | Maximum speed, optional dependency on macFUSE/WinFsp |

### 9.2 Isolated Layout

```
project/
  node_modules/
    .jsm/
      react@18.2.0/node_modules/react/            # hardlinks/reflinks into store
      react-dom@18.2.0_react@18.2.0/node_modules/
        react-dom/
        react -> ../../react@18.2.0/node_modules/react
    react -> .jsm/react@18.2.0/node_modules/react
    react-dom -> .jsm/react-dom@18.2.0_react@18.2.0/node_modules/react-dom
```

### 9.3 Materialization Cache

- Cache key: `hash(lockfile, linker mode, platform, patches, build keys)`.
- On hit, the entire `node_modules` is created by a single directory clone (or hardlink tree) from a cached layout.
- Enables millisecond warm installs, instant branch switching, and fast CI restores.

### 9.4 File Mutation Safety

- Hardlinks are only used for files that are not expected to be mutated.
- Packages with install scripts, patches, or declared mutability are **copied or reflinked**, never hardlinked.
- `jsm doctor` detects store files with unexpected modifications and offers repair.

### 9.5 Bin Links

- Executable shims created in `node_modules/.bin`, with Windows `.cmd` and PowerShell shims.
- Honors `bin` maps and `directories.bin`.

---

## 10. Build System and Install Scripts

### 10.1 Script Policy

- **Lifecycle scripts are disabled by default** (`preinstall`, `install`, `postinstall`, `prepare` for dependencies).
- Approval via `jsm approve-scripts [pkg]`; recorded in `package.json` (`jsm.allowScripts`) or a dedicated file.
- If an approved package's scripts **change** in a new version, approval is revoked and a diff is shown.

### 10.2 Sandboxed Execution

| Platform | Mechanism |
|---|---|
| Linux | Landlock, seccomp, user namespaces (where available) |
| macOS | `sandbox-exec` profiles |
| Windows | Job Objects, AppContainer (best effort) |

Default sandbox policy: no network, write access limited to the package build directory and temp, read access limited to the package, its dependencies, and system toolchains. Packages may declare extra needs through an allowlist entry.

### 10.3 Shared Deterministic Build Cache

- Native addons and script outputs are cached by **build key** (see section 6.3).
- Subsequent installs on the same machine, or any machine sharing the cache, reuse the artifact.
- **Remote cache backends:** S3, GCS, Azure Blob, HTTP, or `jsm serve` peers.
- Cache entries are signed and integrity-checked.

### 10.4 Prebuilt Binary Resolver

For packages that would compile native code, resolve in order:

1. Local build cache hit
2. Remote build cache hit
3. Known prebuilds (`prebuildify`, `node-pre-gyp`, N-API releases, `optionalDependencies` platform packages)
4. Compile from source (sandboxed, with detected toolchain)

### 10.5 Parallel Build Scheduler

- Builds run in topological order with a job pool sized by CPU cores and available memory.
- Only cache-missed packages are built.
- Live progress and per-package build logs (`jsm build-log <pkg>`).

### 10.6 Production Slimming

`jsm install --prod --slim` removes files that are not needed at runtime (tests, docs, sourcemaps, unused platform binaries). `jsm deploy <dir>` outputs a self-contained production tree for Docker images.

---

## 11. Security Model

| Control | Description |
|---|---|
| Integrity | sha512 verification on every download; store verify |
| Provenance | Verify npm provenance and Sigstore attestations when available |
| Registry signatures | Verify registry signing keys |
| Script allowlist | Default-deny with explicit approval |
| Sandboxing | Restricted execution of approved scripts |
| Release cooldown | Skip freshly published versions (`--min-release-age`) |
| Capability analysis | Static scan flags new use of network, `child_process`, `eval`, filesystem, obfuscation between versions |
| Local advisory DB | OSV-based, usable offline (`jsm audit`) |
| Lockfile integrity | Hashes pinned; tarball host changes flagged |
| Typosquat guard | Warn on names close to popular packages when adding new dependencies |
| Telemetry | **None by default** |

### Update Review Report

`jsm update --review` shows, per updated package: version delta, size delta, new or removed dependencies, install script changes, `exports` changes, new capabilities, and advisories.

---

## 12. CLI Specification

Global flags: `--json`, `--quiet`, `--verbose`, `--offline`, `--store-dir`, `--registry`, `--no-color`, `--cwd`, `--config`.

### 12.1 Project Commands

| Command | Description |
|---|---|
| `jsm init` | Create `package.json` (interactive or `-y`) |
| `jsm install` (`i`) | Install from lockfile/manifest |
| `jsm add <pkg>[@range]` | Add dependency (`-D`, `-O`, `-P`, `--exact`, `--workspace`) |
| `jsm remove <pkg>` (`rm`) | Remove dependency |
| `jsm update [pkg]` (`up`) | Update within ranges; `--latest`, `--interactive`, `--review` |
| `jsm outdated` | Show available updates |
| `jsm why <pkg>` | Dependency path explanation; `--size` |
| `jsm list` (`ls`) | Dependency tree; `--depth`, `--prod` |
| `jsm dupes` | Duplicate version report |
| `jsm unused` | Detect unused dependencies via import scanning |
| `jsm link` / `unlink` | Link local packages |
| `jsm patch <pkg>` / `patch-commit` | Create and persist patches |
| `jsm migrate` | Import from npm, Yarn, pnpm, or Bun lockfiles |
| `jsm audit` | Vulnerability and capability audit |
| `jsm approve-scripts [pkg]` | Approve install scripts |
| `jsm doctor [--fix]` | Diagnose and repair |

### 12.2 Execution Commands

| Command | Description |
|---|---|
| `jsm run <script>` | Run a `package.json` script |
| `jsm exec <bin>` | Run a local binary |
| `jsm x <pkg> [args]` | Run a package binary without installing into the project (cached) |
| `jsm run file.ts` | Run a script with inline dependency declarations |
| `jsm node use <version>` | Manage Node.js runtimes |

### 12.3 Store Commands (Signature Feature)

| Command | Description |
|---|---|
| `jsm store path` | Print store location |
| `jsm store status` | Size, package count, dedupe ratio |
| `jsm store list [--sort size\|name\|used]` | All cached packages and versions with size |
| `jsm store versions <pkg>` | Versions of a package available **locally** |
| `jsm store info <pkg>@<ver>` | Metadata, files, size, references, build variants |
| `jsm store add <pkg>@<range>` | Pre-fetch into store |
| `jsm store remove <pkg>@<ver>` | Drop one version; refuses if referenced unless `--force` |
| `jsm store remove <pkg> --all` | Drop all versions of a package |
| `jsm store usage <pkg>@<ver>` | Which projects reference it |
| `jsm store prune` | Remove unreferenced versions |
| `jsm store gc --older-than 90d --max-size 20GB` | Policy-based cleanup |
| `jsm store pin` / `unpin <pkg>@<ver>` | Protect from GC |
| `jsm store verify [--fix]` | Integrity check and repair |
| `jsm store export <lockfile> <out>` | Portable bundle |
| `jsm store import <bundle>` | Load a bundle |

### 12.4 Registry and Version Commands

| Command | Description |
|---|---|
| `jsm versions <pkg>` | Versions available on the **registry** (marks local ones) |
| `jsm info <pkg>[@ver]` | Registry metadata |
| `jsm diff <pkg>@<a> <pkg>@<b>` | File, dependency, script, and exports diff using local store |
| `jsm switch <pkg> <version>` | Change a dependency version and relink (no network if stored) |
| `jsm search <term>` | Registry search |

### 12.5 History and Snapshots

| Command | Description |
|---|---|
| `jsm history` | List install and change history |
| `jsm undo` / `redo` | Revert or reapply last change |
| `jsm snapshot save <name>` | Save current lockfile and link plan |
| `jsm snapshot restore <name>` | Restore snapshot |
| `jsm snapshot list` / `delete` | Manage snapshots |

### 12.6 Workspace Commands

| Command | Description |
|---|---|
| `jsm -r <cmd>` / `--filter <expr>` | Run across workspaces with filters (name, path, git-changed, dependents) |
| `jsm workspaces list` | List workspace packages |
| `jsm deploy <dir>` | Produce a deployable package tree |

### 12.7 Services

| Command | Description |
|---|---|
| `jsm serve [--port]` | Serve store as a registry (LAN/CI) |
| `jsm daemon start\|stop\|status` | Optional background daemon |
| `jsm cache remote <cmd>` | Configure and inspect remote build cache |
| `jsm ui` | Interactive TUI |
| `jsm completions <shell>` | Shell completions |

### 12.8 Exit Codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General error |
| 2 | Usage error |
| 3 | Resolution failure |
| 4 | Network failure |
| 5 | Integrity or security failure |
| 6 | Lockfile out of date (`--frozen-lockfile`) |
| 7 | Script or build failure |

### 12.9 JSON Output Contract

Every command supports `--json`. Output is a single JSON document (or NDJSON stream for long-running commands) with a stable, versioned schema (`"schema": "jsm.v1.<command>"`).

---

## 13. Configuration

### 13.1 Sources and Precedence (highest to lowest)

1. CLI flags
2. Environment variables (`JSM_*`)
3. Project `.jsmrc` / `jsm.toml`
4. Workspace root config
5. User config (`~/.config/jsm/config.toml`)
6. Built-in defaults

`.npmrc` is read for registry, scope, and auth compatibility.

### 13.2 Example `jsm.toml`

```toml
[store]
dir = "~/.jsm/store"
link = "auto"              # auto | reflink | hardlink | copy
max_size = "30GB"

[install]
linker = "isolated"        # isolated | hoisted | virtual
resolution = "highest"
prefer_store = true
min_release_age = "0d"
frozen_lockfile = "auto"   # auto in CI
engine_strict = "warn"

[network]
concurrency = "auto"
hedging = true
timeout = "30s"
retries = 5
mirrors = []

[scripts]
policy = "deny"            # deny | allowlist | allow
sandbox = true

[build_cache]
local = true
remote = ""                # s3://bucket/prefix

[security]
provenance = "warn"        # off | warn | require
capability_scan = true
audit_on_install = false

[telemetry]
enabled = false
```

### 13.3 Key Environment Variables

`JSM_STORE_DIR`, `JSM_REGISTRY`, `JSM_OFFLINE`, `JSM_CI`, `JSM_LOG`, `JSM_NO_COLOR`, `JSM_CONCURRENCY`, `JSM_AUTH_TOKEN`.

---

## 14. Lockfile Specification

### 14.1 Filename and Goals

`jsm.lock`: human-readable, deterministic, merge-friendly.

Design requirements:

- Sorted keys and one entry per block so git diffs and merges stay small.
- Contains integrity (sha512), resolved URL, dependency edges, peer resolution info, and platform constraints.
- Records tool and lockfile format versions.
- Includes optional top-level `store-hint` information (never required for correctness).

### 14.2 Example

```toml
lockfile_version = 1
generated_by = "jsm 0.1.0"

[importers."."]
dependencies = { react = "^18.2.0" }
dev_dependencies = { vitest = "^2.0.0" }

[packages."react@18.2.0"]
resolution = "https://registry.npmjs.org/react/-/react-18.2.0.tgz"
integrity = "sha512-..."
dependencies = { loose-envify = "^1.1.0" }
engines = { node = ">=0.10.0" }

[packages."loose-envify@1.4.0"]
resolution = "https://registry.npmjs.org/loose-envify/-/loose-envify-1.4.0.tgz"
integrity = "sha512-..."
dependencies = { js-tokens = "^3.0.0 || ^4.0.0" }
```

### 14.3 Tooling

- `jsm lock merge`: a git merge driver that auto-resolves lockfile conflicts by re-deriving from both sides.
- `jsm lock verify`: validate integrity and consistency.
- Importers: `package-lock.json`, `yarn.lock` (v1 and Berry), `pnpm-lock.yaml`, `bun.lock`.

---

## 15. Feature Catalog and Priorities

**Legend:** TS = table stakes (required for production); DF = differentiator; Phase = roadmap milestone from section 20.

### 15.1 Store Features

| # | Feature | Class | Phase |
|---|---|---|---|
| S1 | Content-addressed store, shared across projects | TS | M1 |
| S2 | Multi-version coexistence | DF | M1 |
| S3 | Reference registry and safe removal | TS | M2 |
| S4 | Store CLI (list, versions, info, add, remove, usage) | DF | M2 |
| S5 | GC with size and age policy, pinning | TS | M2 |
| S6 | Store verify and repair | TS | M2 |
| S7 | Store export/import bundles | DF | M5 |
| S8 | Store-aware resolution (`--prefer-store`) | DF | M3 |
| S9 | Chunk-level dedupe and zstd compression | DF | M7 |
| S10 | `jsm serve` store-as-registry | DF | M6 |
| S11 | Merkle-tree verification | DF | M6 |

### 15.2 Performance and Concurrency

| # | Feature | Class | Phase |
|---|---|---|---|
| P1 | Streaming pipelined install | DF | M1 |
| P2 | Skip-write extraction with Bloom index | DF | M1 |
| P3 | Lockfile fast path | TS | M1 |
| P4 | Materialization cache (tree cloning) | DF | M3 |
| P5 | Adaptive concurrency and hedged requests | DF | M3 |
| P6 | Cross-process single-flight downloads | TS | M2 |
| P7 | Platform-specific I/O (io_uring, clonefile, block clone) | TS | M3 |
| P8 | Optional daemon with git-hook prefetch | DF | M6 |
| P9 | Sparse registry index | DF | M6 |

### 15.3 Resolution

| # | Feature | Class | Phase |
|---|---|---|---|
| R1 | PubGrub resolver, full npm semver | TS | M1 |
| R2 | Explainable conflicts with suggested fixes | TS | M3 |
| R3 | Peer, optional, platform-filtered dependencies | TS | M4 |
| R4 | Overrides, catalogs, patches | TS | M5 |
| R5 | Release cooldown, lowest-version mode | TS | M4 |
| R6 | Merge-friendly lockfile and merge driver | TS | M2 |
| R7 | Lockfile importers | TS | M5 |

### 15.4 Time Travel and Insight

| # | Feature | Class | Phase |
|---|---|---|---|
| T1 | `history`, `undo`, `redo` | DF | M3 |
| T2 | Snapshots | DF | M3 |
| T3 | `switch` (instant version flip) | DF | M3 |
| T4 | `diff` between stored versions | DF | M4 |
| T5 | `why --size`, `dupes`, `unused` | TS | M4 |
| T6 | Update review report | DF | M5 |

### 15.5 Build and Scripts

| # | Feature | Class | Phase |
|---|---|---|---|
| B1 | Scripts disabled by default, allowlist | TS | M4 |
| B2 | Shared deterministic build cache (local) | DF | M4 |
| B3 | Prebuilt binary resolver | DF | M5 |
| B4 | Sandboxed script execution | DF | M5 |
| B5 | Remote build cache | DF | M6 |
| B6 | Parallel build scheduler | TS | M4 |
| B7 | Production slimming, `deploy` | DF | M6 |

### 15.6 Security

| # | Feature | Class | Phase |
|---|---|---|---|
| X1 | Integrity verification everywhere | TS | M1 |
| X2 | Provenance and signature verification | TS | M5 |
| X3 | Local OSV advisory DB, `audit` | TS | M5 |
| X4 | Capability analysis between versions | DF | M6 |
| X5 | Script-change review and approval revocation | DF | M5 |
| X6 | Typosquat warnings | DF | M6 |

### 15.7 Developer Experience

| # | Feature | Class | Phase |
|---|---|---|---|
| D1 | Rich progress UI and error reporting | TS | M1 |
| D2 | `--json` on all commands, completions | TS | M2 |
| D3 | `doctor` diagnostics and repair | TS | M2 |
| D4 | Zero-install script running | DF | M6 |
| D5 | `jsm x` cached executor | DF | M4 |
| D6 | Node runtime management | DF | M7 |
| D7 | Interactive TUI | DF | M7 |
| D8 | Workspaces and filtering | TS | M5 |
| D9 | Linker modes (isolated, hoisted) | TS | M3 |
| D10 | Virtual (FUSE/ProjectedFS) linker | DF | M7 |

---

## 16. Performance Targets and Benchmarking

### 16.1 Scenarios

| Scenario | Description |
|---|---|
| Cold | Empty store, empty metadata cache, network on |
| Warm store | Populated store, no `node_modules` |
| Warm lockfile | Lockfile present, store populated |
| Re-install | `node_modules` exists, nothing changed |
| Offline | No network, populated store |
| Branch switch | Different lockfile, mostly overlapping packages |
| Monorepo | 50+ workspace packages |
| Native | Project with node-gyp dependencies |

### 16.2 Initial Targets (to be calibrated against baseline measurements)

| Scenario | Target vs. best competitor |
|---|---|
| Cold install | At least 1.5x faster |
| Warm store install | At least 3x faster |
| Re-install (no-op) | Under 50 ms for a 1,000-package project |
| Materialization cache hit | Under 500 ms for a 1,000-package project |
| Branch switch | At least 5x faster |
| Memory | Under 300 MB peak for 2,000-package install |
| Disk | At most 40% of npm for 10 similar projects |

### 16.3 Benchmark Harness

- Reproducible, containerized, pinned fixtures (small, medium, large, monorepo).
- Compare against npm, Yarn, pnpm, and Bun with versions recorded.
- Network conditions simulated (latency and bandwidth shaping).
- Runs on every release and nightly; **performance regressions beyond a threshold fail CI**.
- Results published publicly with methodology.

### 16.4 Profiling and Observability

- `jsm install --trace` writes a Chrome trace/flamegraph-compatible timeline of pipeline stages.
- `tracing` spans with structured logs; `JSM_LOG=debug`.

---

## 17. Reliability, Compatibility and Platform Support

### 17.1 Reliability

- **Transactional installs:** staging directory plus atomic swap; failure leaves previous state.
- **Crash recovery:** startup checks for incomplete operations and cleans or resumes.
- **Resumable downloads** (HTTP range) and retries with jittered exponential backoff.
- **Mirror fallback** and offline mode.
- **Corruption handling:** detect, quarantine, and re-fetch on integrity failure.

### 17.2 Environment Edge Cases

| Area | Handling |
|---|---|
| Case-insensitive filesystems | Collision detection and warnings |
| Windows symlinks | Junction fallback, developer-mode detection |
| Long paths | Long-path aware APIs, shortened virtual store names |
| Cross-device projects | Per-volume store or automatic copy fallback |
| Read-only filesystems | Clear error plus offline-only mode |
| Corporate networks | Proxy, custom CA, NTLM/Kerberos where feasible, `.npmrc` compatibility |
| Network filesystems | Detect and warn; avoid unsafe locking assumptions |
| Containers | Documented store mount patterns for Docker and CI |

### 17.3 Compatibility Surface

Supported manifest and protocol features: `dependencies`, `devDependencies`, `peerDependencies` (and `peerDependenciesMeta`), `optionalDependencies`, `bundledDependencies`, `overrides`/`resolutions`, `workspaces`, `bin`, `files`, `exports`, `engines`, `os`, `cpu`, `libc`, `npm:` aliases, `workspace:`, `file:`, `link:`, `git+` URLs, tarball URLs, and scoped registries with token and basic auth.

---

## 18. Testing Strategy

| Layer | Approach |
|---|---|
| Unit | Semver parsing, range intersection, manifest handling, lockfile round-trip |
| Property-based | Resolver invariants, lockfile determinism, store idempotency (`proptest`) |
| Fuzzing | Semver ranges, tar and gzip parsing, lockfile parsers (`cargo-fuzz`) |
| Integration | Local fake registry (`jsm-testkit`) for deterministic scenarios, including failures and slow responses |
| Concurrency | Multi-process stress tests against a shared store; crash injection (kill during write) |
| Compatibility corpus | Install the top 1,000+ npm packages and a set of real-world repos, `require()`/`import` them, and compare with npm's results |
| Cross-platform CI | Linux (glibc/musl), macOS, Windows; filesystem matrix (ext4, btrfs, XFS, APFS, NTFS, ReFS) |
| Security | Malicious tarball tests (path traversal, symlink escapes, zip bombs), sandbox escape tests |
| Performance | Benchmark harness with regression gates |
| Upgrade | Store and lockfile format migration tests |

---

## 19. Developer Experience

- **Errors:** structured, actionable, with context, cause, and a suggested fix (via `miette`).
- **Progress:** clean, low-noise progress bars with per-stage summary; non-interactive mode for CI.
- **Output:** concise by default, `--verbose` for detail, `--json` for tooling.
- **Discoverability:** `jsm --help` examples, `jsm help <topic>`, typo suggestions for commands.
- **Defaults:** secure and fast with zero configuration; configuration only when needed.
- **Docs:** getting started, migration guides (npm, Yarn, pnpm, Bun), store handbook, CI recipes, troubleshooting, architecture notes.
- **Editor integration:** JSON schema for `jsm.toml` and `jsm.lock`; a language-server-friendly diagnostics output (later).
- **Stability:** semver for the CLI and JSON schemas; deprecation policy of at least two minor releases.

### Example Session

```
$ jsm add react@18.2.0
  Resolved 4 packages in 38ms (3 from store)
  Downloaded 1 package (212 kB) in 120ms
  Linked 4 packages in 9ms
  + react 18.2.0

$ jsm switch react 19.0.0
  react 19.0.0 not in store, fetching... done (95ms)
  Relinked 4 packages in 7ms

$ jsm undo
  Reverted: react 19.0.0 -> 18.2.0 (no network needed)

$ jsm store versions react
  18.2.0   212 kB   used by 7 projects
  19.0.0   219 kB   used by 1 project

$ jsm store remove react@18.2.0
  error: react@18.2.0 is referenced by 7 projects
  help: run `jsm store usage react@18.2.0` to list them, or use --force
```

---

## 20. Roadmap

### M0: Foundations (weeks 1-3)
- Workspace scaffold, CI, lint, formatting, release pipeline
- Core types, error model, logging and tracing
- Fake registry testkit and benchmark skeleton

### M1: Walking Skeleton (weeks 4-9)
- Registry client with abbreviated metadata and ETag cache
- PubGrub resolver (semver, dist-tags)
- Streaming fetch to CAS pipeline with integrity checks and skip-write
- Package manifests in store; basic isolated linker
- `init`, `add`, `install`, `remove`, `jsm.lock`
- **First public benchmark vs. npm and pnpm**

### M2: Store Management and Safety (weeks 10-14)
- Reference registry, `store list/versions/info/add/remove/usage/prune/gc/verify/pin`
- Cross-process locking and single-flight downloads
- `--json` everywhere, completions, `doctor`
- Merge-friendly lockfile and merge driver

### M3: Speed and Time Travel (weeks 15-21)
- Materialization cache
- Adaptive concurrency, hedged requests, platform I/O optimizations
- Store-aware resolution
- `history`, `undo`, `snapshot`, `switch`
- Hoisted linker; improved conflict explanations

### M4: Real-World Compatibility (weeks 22-30)
- Peer, optional, and platform-filtered dependencies
- Script allowlist, build cache (local), parallel build scheduler
- `diff`, `why --size`, `dupes`, `unused`, `jsm x`
- Release cooldown, lowest-version resolution
- Compatibility corpus in CI

### M5: Production Readiness (weeks 31-40)
- Workspaces, filters, catalogs, overrides, patches
- Lockfile importers and `migrate`
- Provenance and signatures, OSV audit
- Sandboxed scripts, prebuilt binary resolver, script-change review
- Update review report
- Store export/import
- **v1.0 release candidate**

### M6: Ecosystem and Scale (post-1.0)
- `jsm serve`, remote build cache, sparse registry index, Merkle verification
- Daemon with git-hook prefetch
- Capability analysis, typosquat warnings
- Zero-install scripts, production slimming and `deploy`

### M7: Experience Extensions
- Chunk-level dedupe and compression
- Interactive TUI
- Node runtime management
- Virtual (FUSE/ProjectedFS) linker

---

## 21. Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Ecosystem compatibility edge cases | High | Corpus testing, hoisted fallback mode, rapid-fix release cadence |
| Hardlink mutation corrupting shared files | High | Reflink preference, copy for scripts and patches, `doctor` detection |
| Windows performance and symlink limits | High | Dev Drive guidance, junction fallback, dedicated Windows CI |
| Resolver correctness (peers, optionals) | High | Property tests, differential testing against npm and pnpm |
| Sandbox portability | Medium | Layered approach, graceful degradation with clear warnings |
| Store corruption or concurrent write bugs | High | Atomic writes, content addressing, crash-injection tests |
| Claims of "fastest" not holding | Medium | Public benchmark harness, regression gates, honest reporting |
| Scope creep | High | Strict milestone gates; differentiators beyond M5 are optional |
| Registry or ecosystem changes | Medium | Abstracted registry client, monitor npm API changes |
| Security reputation (supply chain tooling must itself be secure) | High | Fuzzing, audits, minimal dependencies, reproducible release builds |
| Maintainer capacity | Medium | Modular crates, contributor docs, clear ownership |

---

## 22. Success Metrics

| Category | Metric | Target |
|---|---|---|
| Performance | Benchmark wins across scenarios | Fastest in at least 6 of 8 scenarios |
| Efficiency | Disk usage for 10 related projects | At most 40% of npm |
| Compatibility | Top-1,000 package corpus passing | At least 98% at 1.0 |
| Reliability | Install failures from tool bugs | Under 0.1% of corpus runs |
| Adoption | Projects migrated via `jsm migrate` | Tracked via opt-in surveys and public repos |
| DX | Time from install to first successful install | Under 2 minutes |
| Security | Packages blocked or flagged by default policy | Reported per release |
| Stability | Crash-injection suite pass rate | 100% |

---

## 23. Open Questions

1. **Name and branding.** Is `jsm` final, or should the tool have a distinct brand? Check crates.io, npm, and binary name collisions.
2. **Store default location policy.** One store per user, or per volume with automatic selection?
3. **Embedded database choice** for the reference registry: SQLite vs. a Rust-native KV store (such as `redb` or `sled`).
4. **Default linker for compatibility.** Isolated by default, or hoisted by default with isolated opt-in?
5. **Sandbox strictness defaults.** How much breakage is acceptable for stronger isolation?
6. **Registry index strategy.** Build and host a sparse index, or rely on the official registry metadata API?
7. **Remote build cache trust model.** Signing keys, team-level trust, and poisoning defenses.
8. **Licensing.** MIT/Apache-2.0 dual license (Rust ecosystem norm) vs. alternatives.
9. **Governance.** Single maintainer, organization, or foundation model?
10. **Windows virtual linker.** Is ProjectedFS viable and worth the complexity?

---

## Appendix A: Recommended Crates

| Area | Crates |
|---|---|
| CLI | `clap`, `clap_complete`, `dialoguer`, `ratatui` |
| Async and HTTP | `tokio`, `reqwest` (with `rustls`), `hyper`, `tower` |
| Serialization | `serde`, `serde_json`, `toml`, `serde_yaml` |
| Semver | `node-semver` (or a custom npm-compatible implementation) |
| Resolution | `pubgrub` |
| Archives and compression | `tar`, `flate2` (`zlib-rs` backend), `zstd` |
| Hashing | `sha2`, `blake3`, `ssri` |
| File I/O | `reflink-copy`, `tempfile`, `fs4`, `walkdir`, `ignore`, `io-uring` (Linux, optional) |
| Parallelism | `rayon`, `crossbeam-channel`, `dashmap` |
| Storage index | `rusqlite` or `redb` |
| Bloom filters | `fastbloom` or `bloomfilter` |
| Chunking | `fastcdc` |
| Sandboxing | `landlock`, `seccompiler`, platform APIs |
| Observability | `tracing`, `tracing-subscriber`, `tracing-chrome` |
| UX and errors | `indicatif`, `miette`, `thiserror`, `anyhow`, `owo-colors` |
| Testing | `proptest`, `insta`, `wiremock`, `assert_cmd`, `cargo-fuzz`, `criterion` |
| Security | `sigstore`, `ed25519-dalek`, `osv` data tooling |
| Paths and platform | `dirs`, `which`, `windows-sys` |

---

## Appendix B: Glossary

| Term | Definition |
|---|---|
| **CAS** | Content-addressed storage: files stored by the hash of their content |
| **Store** | The machine-wide shared directory holding all package contents and metadata |
| **Packument** | The registry document describing all versions of a package |
| **Reflink** | Copy-on-write file clone sharing disk blocks until modified |
| **Hardlink** | Additional directory entry pointing to the same file data |
| **Materialization** | Creating a project's `node_modules` from store contents |
| **Virtual store** | Per-project directory of package instances keyed by name, version, and peer set |
| **Build key** | Hash identifying a specific compiled output for a package and environment |
| **Single-flight** | Ensuring only one in-flight fetch per resource across callers or processes |
| **Hedged request** | Duplicate request sent after a latency threshold to cut tail latency |
| **AIMD** | Additive-increase, multiplicative-decrease congestion control |
| **PubGrub** | Version-solving algorithm with conflict-driven learning and readable explanations |
| **Provenance** | Verifiable attestation of how and where a package was built |
