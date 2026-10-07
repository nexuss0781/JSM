# JSM Phase 1 Benchmark Comparison

Generated: `2026-10-07T04:04:04.327630+00:00`\
Fixture: `small` (revision 1, seed 20261007)\
Git revision: `b8f798aaef40c7e5e4ff52844af7ad860489518b` (working tree clean: `True`)\
Measurement protocol: `phase1-per-sample-cache-isolated-v1`\
Network shape: latency `0.0 ms/request`, bandwidth `0 B/s`\
Container runtime: `none (host mode)` (`unavailable`)\
Isolation: separate temporary projects and per-tool caches; JSM CAS and registry caches are rooted under the cleared per-tool cache directory for every cold/CI sample; host processes, not containers.

> This run measures the actual configured JSM executable against npm and pnpm on a deterministic loopback fixture. It is host-mode evidence only; compare only reports with matching fixture, tool versions, flags, OS, and network profile.

| Tool | Version | Fixture | Scenario | Status | Median (s) | Prior median (s) |
|---|---|---|---|---|---:|---:|
| npm | 11.17.0 | small | cold | passed | 0.594176 | — |
| npm | 11.17.0 | small | ci | passed | 0.476722 | — |
| pnpm | 11.25.0 | small | cold | passed | 1.114917 | — |
| pnpm | 11.25.0 | small | ci | passed | 0.967131 | — |
| jsm | jsm 0.1.0 | small | cold | passed | 0.051632 | — |
| jsm | jsm 0.1.0 | small | ci | passed | 0.035784 | — |

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
