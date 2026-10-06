# JSM Phase 0 Benchmark Baseline

Generated: `2026-10-06T17:55:13.270275+00:00`\
Fixture: `small` (revision 1, seed 20261007)\
Git revision: `fbbd8c58e0badd0381faab3c3306ecc478669488` (working tree clean: `True`)\
Network shape: latency `0.0 ms/request`, bandwidth `0 B/s`\
Container runtime: `podman` (`podman version 4.9.3`)\
Isolation: each tool/scenario run uses one fresh container across its samples, with a separate project and cache; container startup and teardown are outside sample timing; the local registry is reached over Linux host networking.

> This is a Phase 0 harness baseline, not a JSM performance claim. `jsm-stub` is deliberately not an installer; local registry fixtures and tool versions are captured. Compare results only when fixture, tools, flags, OS, and network profile match.

| Tool | Version | Fixture | Scenario | Status | Median (s) | Prior median (s) |
|---|---|---|---|---|---:|---:|
| npm | 11.17.0 | small | cold | passed | 0.710466 | — |
| npm | 11.17.0 | small | warm-store | passed | 0.430913 | — |
| npm | 11.17.0 | small | warm-lockfile | passed | 0.427730 | — |
| npm | 11.17.0 | small | reinstall | passed | 0.444092 | — |
| npm | 11.17.0 | small | offline | passed | 0.439098 | — |
| npm | 11.17.0 | small | branch-switch | passed | 0.520909 | — |
| npm | 11.17.0 | small | ci | passed | 0.659960 | — |
| pnpm | 9.15.9 | small | cold | passed | 0.913068 | — |
| pnpm | 9.15.9 | small | warm-store | passed | 0.663830 | — |
| pnpm | 9.15.9 | small | warm-lockfile | passed | 0.671908 | — |
| pnpm | 9.15.9 | small | reinstall | passed | 0.677651 | — |
| pnpm | 9.15.9 | small | offline | passed | 0.677361 | — |
| pnpm | 9.15.9 | small | branch-switch | passed | 0.776976 | — |
| pnpm | 9.15.9 | small | ci | passed | 0.902148 | — |
| yarn | 1.22.22 | small | cold | passed | 0.661381 | — |
| yarn | 1.22.22 | small | warm-store | passed | 0.389153 | — |
| yarn | 1.22.22 | small | warm-lockfile | passed | 0.394645 | — |
| yarn | 1.22.22 | small | reinstall | passed | 0.399608 | — |
| yarn | 1.22.22 | small | offline | passed | 0.402618 | — |
| yarn | 1.22.22 | small | branch-switch | passed | 0.471481 | — |
| yarn | 1.22.22 | small | ci | passed | 0.644304 | — |
| bun | 1.2.22 | small | cold | passed | 1.133328 | — |
| bun | 1.2.22 | small | warm-store | passed | 0.089685 | — |
| bun | 1.2.22 | small | warm-lockfile | passed | 0.106221 | — |
| bun | 1.2.22 | small | reinstall | passed | 0.115464 | — |
| bun | 1.2.22 | small | offline | passed | 0.089957 | — |
| bun | 1.2.22 | small | branch-switch | passed | 0.091464 | — |
| bun | 1.2.22 | small | ci | passed | 1.141896 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | cold | passed | 0.144232 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | warm-store | passed | 0.128617 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | warm-lockfile | passed | 0.137358 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | reinstall | passed | 0.122061 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | offline | passed | 0.131399 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | branch-switch | passed | 0.135707 | — |
| jsm-stub | jsm-stub 0.1.0 (Phase 0; not an installer) | small | ci | passed | 0.140218 | — |

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
