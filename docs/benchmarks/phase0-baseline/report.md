# JSM Phase 0 Benchmark Baseline

Generated: `2026-10-06T18:05:57.308144+00:00`\
Fixture: `small` (revision 1, seed 20261007)\
Git revision: `13a81c5c42c7d852d97dfc675faf0ae3eaa53939` (working tree clean: `True`)\
Network shape: latency `0.0 ms/request`, bandwidth `0 B/s`\
Container runtime: `podman` (`podman version 4.9.3`)\
Isolation: each tool/scenario run uses one fresh container across its samples, with a separate project and cache; container startup and teardown are outside sample timing; the local registry is reached over Linux host networking.

> This is a Phase 0 harness baseline, not a JSM performance claim. `jsm-stub` is deliberately not an installer; local registry fixtures and tool versions are captured. Compare results only when fixture, tools, flags, OS, and network profile match.

| Tool | Version | Fixture | Scenario | Status | Median (s) | Prior median (s) |
|---|---|---|---|---|---:|---:|
| npm | 11.17.0 | small | cold | passed | 0.718682 | — |
| npm | 11.17.0 | small | warm-store | passed | 0.472073 | — |
| npm | 11.17.0 | small | warm-lockfile | passed | 0.443829 | — |
| npm | 11.17.0 | small | reinstall | passed | 0.431036 | — |
| npm | 11.17.0 | small | offline | passed | 0.451648 | — |
| npm | 11.17.0 | small | branch-switch | passed | 0.512242 | — |
| npm | 11.17.0 | small | ci | passed | 0.667206 | — |
| pnpm | 9.15.9 | small | cold | passed | 0.901920 | — |
| pnpm | 9.15.9 | small | warm-store | passed | 0.692059 | — |
| pnpm | 9.15.9 | small | warm-lockfile | passed | 0.675562 | — |
| pnpm | 9.15.9 | small | reinstall | passed | 0.656196 | — |
| pnpm | 9.15.9 | small | offline | passed | 0.635373 | — |
| pnpm | 9.15.9 | small | branch-switch | passed | 0.697347 | — |
| pnpm | 9.15.9 | small | ci | passed | 0.844886 | — |
| yarn | 1.22.22 | small | cold | passed | 0.594561 | — |
| yarn | 1.22.22 | small | warm-store | passed | 0.364104 | — |
| yarn | 1.22.22 | small | warm-lockfile | passed | 0.358784 | — |
| yarn | 1.22.22 | small | reinstall | passed | 0.384156 | — |
| yarn | 1.22.22 | small | offline | passed | 0.393538 | — |
| yarn | 1.22.22 | small | branch-switch | passed | 0.463053 | — |
| yarn | 1.22.22 | small | ci | passed | 0.660228 | — |
| bun | 1.2.22 | small | cold | passed | 1.149343 | — |
| bun | 1.2.22 | small | warm-store | passed | 0.094838 | — |
| bun | 1.2.22 | small | warm-lockfile | passed | 0.094195 | — |
| bun | 1.2.22 | small | reinstall | passed | 0.110733 | — |
| bun | 1.2.22 | small | offline | passed | 0.112684 | — |
| bun | 1.2.22 | small | branch-switch | passed | 0.109916 | — |
| bun | 1.2.22 | small | ci | passed | 1.118385 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | cold | passed | 0.138824 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | warm-store | passed | 0.142466 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | warm-lockfile | passed | 0.137261 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | reinstall | passed | 0.139326 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | offline | passed | 0.126530 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | branch-switch | passed | 0.137878 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | ci | passed | 0.142300 | — |

## Machine and method

```json
{
  "cpu_count": 8,
  "machine": "x86_64",
  "node_version": "v24.19.0",
  "npm_version": "11.17.0",
  "platform": "Linux-6.18.38+-x86_64-with-glibc2.39",
  "python": "3.12.3",
  "release": "6.18.38+",
  "system": "Linux"
}
```
