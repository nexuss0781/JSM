# Isolated linker

The linker materializes each lockfile package under `node_modules/.jsm` and
publishes only validated top-level links. Linking is staged and committed by
rename: a failed build or publish preflight leaves the previously valid tree in
place, and old staging/orphan data is removed only after a successful commit.

## File strategy and mutability

The linker probes reflink support per source/target volume and caches failed
capability probes for the process. It uses reflink when available and falls
back to a byte-for-byte copy. It **does not hardlink package files**. Although
hardlinks can be faster, package files under `node_modules` are mutable user
outputs; a hardlink would allow an edit to mutate the content-addressed store.
The copy fallback is therefore intentional and security-preserving. Existing
staged entries are hash-checked and reused where unchanged, while removed
manifest entries are pruned during staging.

## Paths and shebangs

Manifest paths, symlink targets, package names, bin names, and bin targets are
validated again at link time. Absolute paths, `..`, platform separators, and
symlinked ancestors are rejected. Package bytes are copied unchanged: **the
linker never rewrites a package shebang**. Unix bins are executable symlinks;
Windows uses `.cmd` and PowerShell shims that invoke Node.

## Bin conflicts

Bin selection is deterministic: a direct dependency beats an indirect one;
within the same class, the lexicographically smaller lockfile package key wins.
Each losing candidate produces a stable warning naming the bin, winner, and
loser. This warning is diagnostic only; the selected winner is the one exposed
through `node_modules/.bin`.
