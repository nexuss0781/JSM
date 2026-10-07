# ADR 0005: SQLite-backed reference registry

- **Status:** Proposed
- **Date:** 2026-10-07
- **Related:** `PHASE.md` §2; `SPECS.md` §§2.1, 2.5; `TODO.md` §2.1

## Context

Phase 2.1 requires a persistent reference registry under `store/index/` that records project identities and paths, lockfile hashes, package references, install/use times, package sizes, and reference counts. Install commits must update references transactionally; Phase 2.5 then requires many independent `jsm` processes to operate on one store without corruption. The current layout already reserves `index/store.db`, but the registry and its database format have not been implemented, so there is no existing index data to migrate.

The unresolved choice in `PROJECT.md` §23 is SQLite versus a Rust-native key-value store such as `redb`. The reference data is naturally relational: projects use many package versions, usage commands need reverse lookups, and counts must remain consistent with the project/package relationships. The important workload constraint is safe access from separate processes, not maximum in-process read throughput.

## Decision

**Proposed: use SQLite through `rusqlite` for the reference registry, stored at `store/index/store.db`.** Use the `rusqlite` `bundled` feature so builds use the crate-managed SQLite implementation rather than relying on an unspecified system SQLite installation; keep the selected `rusqlite` version in `Cargo.lock`. The database is internal metadata, not a public interchange format, and does not replace the content-addressed package files or package manifests.

Represent project-to-package usage as a normalized relationship and make each reference-set update atomic with the install commit. Prefer deriving reference counts from those relationships rather than keeping an independently mutable count; any cached count must be updated in the same transaction and checked against ground truth. Version the schema with SQLite's `PRAGMA user_version` and test fresh creation plus every supported migration.

Keep database write transactions short: perform no network, archive, or package-file I/O while a metadata transaction is open. Configure a bounded busy wait/retry path with actionable diagnostics, and verify overlapping-process behavior under the Phase 2 stress tests. SQLite's single-writer model serializes only the brief metadata commits; it does not replace JSM's per-package and maintenance locks or require a global install lock. Journal mode is left to implementation evidence; do not assume WAL works on a network filesystem, and retain the existing warning/degraded-support policy for filesystems with unreliable locking.

This proposal resolves only the database-engine choice. It does not freeze the full SQL schema, locking hierarchy, transaction boundaries across the filesystem and database, or public CLI/JSON contracts; those remain subject to their Phase 2 design and acceptance tests.

## Alternatives considered

1. **`redb`:** attractive because it is written in Rust and provides ACID transactions with concurrent reads and a writer. However, in redb 4.3.0 the `experimental-multiprocess` feature enables `experimental-api-5`; the ordinary API documentation describes concurrent read transactions and a single writer on an open database handle. Using the experimental cross-process path for JSM's core shared-store invariant adds compatibility and operational risk. It would also leave relational joins and reference accounting to application-managed key/index conventions.
2. **SQLite through `rusqlite` (proposed):** SQLite documents process-aware file locking and supports multiple simultaneous readers, while allowing only one simultaneous writer. That matches the short, transactional metadata updates expected here and uses a mature cross-platform engine. The cost is compiled native SQLite code when bundled, a SQL schema/migration responsibility, and serialized writers that must be measured under load.
3. **Flat files or custom journaling:** rejected because atomic multi-record project/reference changes, reverse lookup, and recoverable concurrent updates would require JSM to build and maintain database machinery itself.

## Consequences

- Reference mutations and queries use relational constraints and transactions, while package payload integrity remains governed by the existing CAS manifests and hashing rules.
- Concurrent readers can proceed, but metadata writers serialize. Phase 2.1 randomized ground-truth tests and Phase 2.5 overlapping-process stress are required before claiming suitability; no throughput advantage is claimed by this ADR.
- Bundling SQLite gives a controlled engine version across supported platforms but adds native compilation work and some build time. Linux, macOS, Windows, and the repository's cross-target checks must continue to pass, and `cargo deny check` must remain clean.
- The index file requires explicit schema-versioned migrations and corruption handling. A future schema change must not silently discard references or package metadata.
- SQLite locking is not a guarantee for multi-host network filesystems. JSM's existing filesystem detection and warning requirements remain in force; WAL must not be treated as network-filesystem support.
- On acceptance, update `PROJECT.md` §23 to close the database-choice question and implement Phase 2.1 against this decision. Until then, this Proposed ADR does not authorize a production database format freeze or implementation.

## Evidence and review

The repository requirements are in [`SPECS.md` §2.1](../../SPECS.md), [`SPECS.md` §2.5](../../SPECS.md), and [`PHASE.md` §2](../../PHASE.md). SQLite's [transaction documentation](https://www.sqlite.org/lang_transaction.html) states that separate connections/processes may have simultaneous read transactions but only one simultaneous write transaction; its [locking documentation](https://www.sqlite.org/lockingv3.html) describes process-level locking. SQLite's [WAL documentation](https://www.sqlite.org/wal.html) notes that WAL does not work over a network filesystem. The versioned [redb 4.3.0 feature list](https://docs.rs/crate/redb/4.3.0/features) shows `experimental-multiprocess` depends on `experimental-api-5`, and its [Database API](https://docs.rs/redb/4.3.0/redb/struct.Database.html) documents its transaction concurrency. `rusqlite`'s [feature list](https://docs.rs/crate/rusqlite/0.40.2/features) documents the `bundled` feature chain.

Project review is required before accepting this choice and freezing the persisted format, as required by [`docs/adr/README.md`](README.md). This PR contains no database dependency, schema, or implementation.
