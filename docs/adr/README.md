# Architecture Decision Records

Use an ADR for decisions that affect persisted formats, public CLI/JSON behavior, security policy, dependency boundaries, or compatibility. Keep decisions small, link the relevant `PHASE.md` / `SPECS.md` sub-phase, and state consequences and evidence. A draft ADR does not authorize a production format freeze; obtain project review before closing an open decision.

Copy `TEMPLATE.md` to the next zero-padded number and descriptive slug. Do not reuse a number.

## Accepted decisions

- [ADR 0002: Lockfile v2 peer-context package identity](0002-lockfile-v2-peer-context.md) — user-approved v2 key and v1 compatibility policy.
- [ADR 0003: Shared store path and cross-volume fallback](0003-shared-store-path-policy.md) — per-user default store, override precedence, and hard-link/copy policy.
