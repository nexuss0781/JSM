# Phase 2 — Store Management and Safety

Phase 2 adds a persistent reference registry and operational controls around the shared content-addressed store. The selected backend is SQLite through `rusqlite` with bundled SQLite; see the accepted [ADR 0005](adr/0005-sqlite-reference-registry.md). The registry is a rebuildable index, not the source of package bytes: package manifests and SHA-512-addressed blobs remain the store's durable content records.

## Store operations

```sh
jsm store path
jsm store status --json
jsm store list --sort size --filter '@scope/*'
jsm store info package@1.2.3
jsm store usage package@1.2.3
jsm store add package@^1.2.0
jsm store add --from-lockfile
jsm store pin package@1.2.3
jsm store verify --full
jsm store prune --dry-run
jsm store gc --older-than 30d --max-size 2GiB --dry-run
```

`store status`, `list`, `versions`, `info`, and `usage` report logical/physical sizes, local references, last-use metadata, and legacy-index uncertainty. `store add` prefetches a resolved graph without linking it into a project. `store versions` lists locally stored versions; top-level `jsm versions <package>` compares remote versions with the local store.

Removal, prune, and GC protect referenced and pinned versions by default. `--dry-run` reports the candidate versions and predicted logical/physical bytes; interactive operations ask before mutation, and `--yes` is for deliberately non-interactive use. `store remove --force` explicitly accepts breaking project references. To release a project record, use `jsm store forget-project <project-path-or-id>`; it also supports `--dry-run` and `--yes`.

Missing project paths and changed lockfiles are marked stale, **not automatically forgotten**. A missing path can represent a moved project; keeping its references prevents prune/GC from deleting bytes before the project is reinstalled at its new path. Inspect rows with `store usage`; explicitly forget a deleted or intentionally abandoned project to release its references.

## Locking and install recovery

The lock hierarchy is:

1. A shared store-maintenance lease protects ordinary install/prefetch reads and writes from exclusive prune/GC/repair operations.
2. A project lease serializes installs that mutate the same project tree and its lockfile.
3. A package-identity lease (name, version, integrity) provides single-flight extraction/download for identical artifacts.
4. SQLite transactions atomically update the project/package reference graph.

Leases are cross-process advisory file locks with bounded waits and owner diagnostics. Install performs age-gated orphan cleanup under an exclusive startup lease, then holds the shared maintenance lease and project lease while resolving, fetching, linking, and committing metadata. Destructive maintenance holds an exclusive maintenance lease, so it cannot sweep an in-flight install.

Before project-visible mutation, install writes a durable journal and records prior project-tree/lockfile/reference state. On an ordinary error it rolls back; after a process crash, the next invocation recovers the journal before validating the lockfile. Recovery is idempotent and cleans transaction backups only after successful completion. Debug test builds support named process-crash injection points through `JSM_TEST_CRASH_AT`; release builds ignore this variable.

## Verification and repair

`jsm store verify` checks package manifests and blob metadata; `--full` hashes each unique blob. `--fix` quarantines invalid/corrupt store records under the exclusive maintenance lease and records corruption so an install can re-fetch the package. Verification output identifies the package and finding type; repair does not run package lifecycle scripts.

## JSON and progress contracts

Phase 2 JSON envelopes are defined in [phase2-cli-output.schema.json](schemas/phase2-cli-output.schema.json). Phase 1 envelopes remain in [phase1-cli-output.schema.json](schemas/phase1-cli-output.schema.json). Each response has a stable `schema` field using `jsm.v1.<command>`. With `--json --progress=ndjson`, each progress line is a standalone JSON object using `jsm.v1.progress`; the final command result remains its ordinary `jsm.v1.<command>` object. `--progress=ndjson` requires `--json` so human text never contaminates the stream.

## Diagnostics and shell support

`jsm doctor` reports a status, explanation, and remedy for store integrity/references, link capabilities, volume placement, long paths, case sensitivity, advisory locks, proxy/TLS configuration, registry reachability, clock, available space, platform-specific checks, and Node/jsm versions. `--fix` is limited to age-gated orphan cleanup and schema validation; it does not silently rewrite project dependencies. `--report` emits JSON with credentials redacted and the user's home path abbreviated.

Generate completion scripts with `jsm completion bash|zsh|fish|power-shell|elvish`. Local package/version and registered-project candidates are available through the hidden `jsm __complete packages|projects <prefix>` endpoint and are wired into the shell completion output. Topic help is available with `jsm help store`, `jsm help lockfile`, `jsm help config`, and `jsm help scripts`.

## Lockfile support

`jsm lock verify` validates the lockfile structure, dependency edges, package-instance keys, and importer data against the current manifest. `jsm lock merge <base> <ours> <theirs>` performs deterministic parser-based three-way merging; independent changes can be combined, while edits to the same field or delete/edit conflicts fail rather than choosing a side. Successful output must still verify against the current manifest. `jsm lock install-merge-driver` adds the repository-local `.gitattributes` rule and Git merge-driver config; the user can remove those repository-local settings later with normal Git configuration tools.

## Validation

The executable acceptance tests in `crates/jsm-cli/tests/phase2_cli.rs` cover recovery at all six injected transaction points (journal persistence, node_modules backup, lockfile backup, link completion, lockfile commit, and reference commit); concurrent install/GC; 50 concurrent processes sharing one package identity with one tarball request; store dry-run versus actual reclaimed-byte equality; referenced removal refusal; explicit stale-project release; doctor checks and registry reachability success/failure; report redaction; NDJSON; dynamic completion queries; all five completion generators; and topic help. Store and lockfile unit suites additionally exercise SQLite migration/reference invariants, bit flips, truncation, missing blobs, quarantine/re-fetch markers, and deterministic merge conflicts. The full workspace test suite and strict Clippy (`cargo clippy --workspace --all-targets -- -D warnings`) pass.

The Phase 1 24-hour SemVer fuzz campaign remains explicitly deferred and non-gating. Phase 2 does not restart or claim evidence from that campaign.
