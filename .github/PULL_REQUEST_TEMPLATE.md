## Phase traceability

- Phase/sub-phase IDs:
- Related issue or ADR:

## Change summary

Describe the behavior and any compatibility or design impact.

## Verification

- [ ] Tests added or updated
- [ ] `just lint` passes
- [ ] `just test` passes
- [ ] Documentation updated
- [ ] Platform evidence included where applicable

## Safety and release invariants

- [ ] No credentials/tokens are exposed in logs or diagnostics
- [ ] No dependency lifecycle script is executed without explicit approval
- [ ] No unverified network content becomes project-visible
- [ ] Failure/cancellation preserves the last valid state
- [ ] Persisted/public/security decisions have a reviewed ADR

## Checklist

- [ ] I did not mark a backlog item complete without merged implementation, tests, docs, and required platform/security evidence.
