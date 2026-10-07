# JSM Phase 1 Benchmark Comparison

Generated: `2026-10-07T03:57:32.752421+00:00`\
Fixture: `small` (revision 1, seed 20261007)\
Git revision: `2a2679b51b546d520c218ad8745b2156a5940151` (working tree clean: `False`)\
Network shape: latency `0.0 ms/request`, bandwidth `0 B/s`\
Container runtime: `none (host mode)` (`unavailable`)\
Isolation: separate temporary project directories and per-tool caches; processes run on the host, not in containers.

> This run measures the actual configured JSM executable against npm and pnpm on a deterministic loopback fixture. It is host-mode evidence only; compare only reports with matching fixture, tool versions, flags, OS, and network profile.

| Tool | Version | Fixture | Scenario | Status | Median (s) | Prior median (s) |
|---|---|---|---|---|---:|---:|
| npm | 11.17.0 | small | cold | passed | 0.919443 | 0.492249 |
| npm | 11.17.0 | small | ci | passed | 0.482658 | 0.421067 |
| pnpm | 11.25.0 | small | cold | passed | 1.045512 | 0.917438 |
| pnpm | 11.25.0 | small | ci | passed | 1.023272 | 0.874749 |
| jsm | jsm 0.1.0 | small | cold | passed | 0.956494 | 0.856935 |
| jsm | jsm 0.1.0 | small | ci | passed | 0.912991 | 0.768215 |

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
