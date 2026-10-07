# Phase 1 validation sources

## Optional peer dependencies

- **Source:** npm CLI documentation, `package.json` reference, section `peerDependenciesMeta`: https://docs.npmjs.com/cli/configuring-npm/package-json/
- **Retrieved:** 2026-10-07.
- **Relevant documented behavior:** “Npm will not automatically install optional peer dependencies.”
- **Applied to:** Registry candidate construction excludes optional peer dependencies from automatic resolver edges; the CLI fake-registry regression `optional_peer_dependency_is_not_automatically_installed` proves the package is not fetched/linked merely because it is available.

This is an implementation reference, not a claim that JSM implements every npm peer-dependency behavior in later phases.
