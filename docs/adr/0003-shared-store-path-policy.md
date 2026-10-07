# ADR 0003: Shared store path and cross-volume fallback

- **Status:** Accepted
- **Date:** 2026-10-07
- **Related:** `PHASE.md` §1.6–1.8; `SPECS.md` content-addressed store and linker requirements

## Context

Phase 1 needs a platform-default path for its shared, multi-version content-addressed store and an explicit policy when a project is on another filesystem. A single shared store is preferred for cross-project deduplication, while placement must remain safe across filesystem boundaries.

## Decision

Use one per-user shared store by default:

- Linux and other non-macOS Unix: `$XDG_CACHE_HOME/jsm/store`, falling back to `$HOME/.cache/jsm/store`.
- macOS: `$HOME/Library/Caches/jsm/store`.
- Windows: `%LOCALAPPDATA%/jsm/store`, falling back to the user home directory.

Resolve overrides by precedence: `--store-dir`, `JSM_STORE_DIR`, project configuration, workspace configuration, user configuration, then the platform default. Relative override paths are resolved from the project root.

When no explicit override is supplied, `Store::open_for_project` compares the project and shared-default volume IDs using stdlib platform metadata (`st_dev` on Unix and the Windows volume serial number where available). On a different volume it selects the corresponding volume mount root's per-user cache path (for example, `<volume-root>/.cache/jsm/store` on Unix). If that location is not writable, it safely falls back to the project's hidden `.jsm-store` directory. Same-volume projects continue using the shared default. An explicit override always wins and is never replaced by per-volume selection.

Blob writes remain atomic: streams are written to `tmp/`, verified, and renamed into the content-addressed path only after successful completion. Reader or writer failures remove the partial temporary file and never expose a final blob.

Store format v2 keys each blob by the direct SHA-512 digest of its file bytes. Version 1 accidentally keyed blobs by a second SHA-512 over the first digest. Opening a v1 store upgrades its version marker; old package manifests fail v2 blob validation and are re-fetched on demand before linking. Legacy blobs may remain as unreachable cache entries, but they are never exposed through a project tree.

When materializing package files, the linker attempts reflink, then hardlink, then a byte-for-byte copy of a verified blob. Hardlinks are restricted to non-executable files whose source is a verified, read-only CAS blob. Files copied from an existing project-visible staged tree, executable files (whose mode may change), and future mutable/script/patch outputs are always reflinked or copied, never hardlinked. Unsupported or cross-volume reflinks/hardlinks fall back to copy.

## Alternatives considered

1. Store under every project. Rejected because it loses shared deduplication and makes package reuse project-scoped.
2. Always maintain a separate store per filesystem/volume. Rejected as the default because it unnecessarily loses sharing; implemented only when the shared default is on another volume.
3. Require same-volume hard links. Rejected because installs would fail for common home/project volume layouts and unsupported filesystems; hardlink is only an optimization and copy remains the safe fallback.

## Consequences

- The shared store is reusable across projects, with a user-visible override for custom placement.
- New CAS entries use ordinary SHA-512 content keys; v1 entries are revalidated and fetched again only when needed during the version transition.
- Cross-volume projects use a per-user cache rooted on their own volume; if it is unavailable, the project-local hidden store preserves correctness.
- Platform defaults follow user cache/application-data conventions; changing a path remains possible through the documented overrides.
- Package output materialization uses reflink-hardlink-copy ordering only where the source integrity and readonly preconditions make a hardlink safe; mutable and executable outputs remain copy/reflink-only.

## Evidence and review

The path selection is implemented by `jsm-store::Store::open_for_project`, with deterministic unit coverage for same-volume selection, different-volume selection, and explicit override precedence. The store also tests cleanup after a reader fails mid-stream. `jsm-linker` verifies CAS hash and readonly state, tests the safe non-executable hardlink path, and tests copy-only mutable, executable, and fallback paths.
