# ADR 0002: Lockfile v2 peer-context package identity

- **Status:** Accepted
- **Date:** 2026-10-07
- **Related:** `PHASE.md` §1.10; `SPECS.md` lockfile and peer-dependency requirements

## Context

A `name@version` key cannot distinguish two instances of the same release when their peer environments differ. The lockfile must give dependency edges a deterministic identity that is independent of physical `node_modules` layout, while remaining verifiable and safe for downstream tools to treat as opaque.

## Decision

Use lockfile format version 2 and this exact package-instance key form:

```text
<name>@<version>#integrity=<canonical-SRI>#peer=<lowercase-hex-SHA256>
```

For example:

```text
@acme/widget@2.1.0#integrity=sha512-<SRI-digest>#peer=9f…
```

The `peer` digest is SHA-256 over a deterministic canonical serialization of the resolved peer-context graph. Bindings are ordered by UTF-8 byte order and identify the exact resolved peer instance. An absent optional peer is represented by an explicit sentinel; an absent required peer is a resolution error. The empty context has a fixed canonical digest. Cyclic peer graphs are serialized deterministically as graph components rather than by recursive key expansion. Importer resolutions and dependency edges point to full instance keys. Package records retain the canonical name, version, SRI, resolution URL, peer bindings, and digest.

Physical store/linker paths are not identity. Linkers must map the full logical key to a safe physical component and must not parse or directly use the SRI-bearing key as a path.

V1 remains readable. Peer-free v1 graphs may be migrated in memory when unambiguous. A v1 graph with peer metadata requiring context-sensitive identity must fail with an actionable re-resolve/migrate diagnostic rather than silently merging instances. Frozen installs must not rewrite the lockfile; when migration or re-resolution is required, they fail without modifying project state. Tools consuming v2 package keys must treat them as opaque.

## Alternatives considered

1. Keep `name@version` and store peer bindings only in package records. Rejected because one map key cannot represent two peer contexts for one release.
2. Put readable peer tuples in the key. Rejected because the key would be unbounded and awkward to escape and canonicalize.
3. Derive package identity from a physical layout/path. Rejected because that couples lock identity to linker layout and platform details.

## Consequences

- Lockfile format is v2 and v1 migration behavior is explicit.
- Same-release packages with distinct peer environments can coexist.
- A downstream reader must not assume package keys are just `name@version`.
- The SRI text may contain path-significant characters; filesystem layout must use an independent safe encoding.
- Peer graph canonicalization, including cycles and optional-missing sentinels, is part of the format contract and requires regression tests.

## Evidence and review

The project owner explicitly approved “hashed lockfile v2” on 2026-10-07. Initial deterministic key and v1 migration tests are in `crates/jsm-lockfile/src/lib.rs`; full peer graph resolution, cycle handling, and end-to-end linker validation remain subject to the Phase 1 verification run.
