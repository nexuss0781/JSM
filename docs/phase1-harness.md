# Phase 1 acceptance and benchmark harness

The Phase 1 gate runner is `scripts/phase1_gate.py`. It reads the authoritative Phase 1 checklist from `TODO.md`, runs repository quality checks, checks the CLI command surface, identifies still-scaffolded implementation crates, and writes a JSON plus Markdown status report. It deliberately fails the phase gate while implementation requirements or evidence are missing; passing the Phase 0 fake-registry tests is not treated as proof of a working JSM package manager.

Run the harness unit tests with:

```sh
python3 scripts/phase1_gate.py --self-test
python3 -m unittest benches.test_run -v
```

Run the full local verification and produce `docs/phase-1-status.json` and `docs/phase-1-status.md` with:

```sh
python3 scripts/phase1_gate.py --verify
```

Exit code `2` means the local checks completed but the Phase 1 gate is still unmet. The report lists open checklist items, absent CLI commands, scaffold crates, and missing acceptance/benchmark evidence. It is intentionally not a CI job that marks an incomplete product as green.

## Real-registry acceptance set

Create a timestamped, replayable snapshot of 100 npm search results:

```sh
python3 scripts/phase1_gate.py --snapshot-top100 benches/phase1-top100.json
```

The snapshot uses npm Registry Search ([API specification](https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md), endpoint `/-/v1/search`) with `text=keywords:javascript` and popularity-only score weights (`quality=0`, `popularity=1`, `maintenance=0`). It records the source URL, timestamp, package names, and versions. npm search is query-scoped; this is a stable-to-replay acceptance sample, **not** a claim that these are the 100 packages with the greatest global download counts. The list is saved so later runs use the same packages rather than silently changing with registry rankings.

After the Phase 1 CLI supports the specified commands, run all 100 packages against the public npm registry:

```sh
cargo build --release -p jsm-cli --locked
python3 scripts/phase1_gate.py --run-top100 benches/phase1-top100.json --jsm target/release/jsm
```

For a quick harness/CLI diagnostic, `--limit 1` runs only the first package; that is not gate evidence. Each package gets an isolated project and exercises `init -y`, `add`, frozen `install`, and `remove`, checking manifest and installed-package state. Results are written to `benches/results/phase1-top100/report.json` and `report.md`. The runner does not invoke dependency lifecycle scripts.

## Comparative benchmark

The old `benches/run.py --smoke` path remains a Phase 0 harness check and continues to use `jsm-stub`; it must not be presented as a JSM performance result. The real-binary comparison is a separate mode:

```sh
cargo build --release -p jsm-cli --locked
python3 benches/run.py --phase1 --jsm-binary target/release/jsm \
  --container-runtime none --repeat 3
```

This runs npm, pnpm, and the supplied JSM binary on identical deterministic small fixtures served by the local fake registry, records versions, environment, commands, and samples, and writes to `docs/benchmarks/phase1-baseline/` by default. It is explicitly host-mode evidence; compare only results with matching operating system, tool versions, fixture revision, flags, and network shaping. Use `--fixture`, `--scenario`, `--latency-ms`, and `--bandwidth-bytes-per-second` to define a comparable workload. No Phase 1 product benchmark should be published until the real JSM binary successfully completes the scenarios.

## SemVer range fuzzing

The Phase 1 SemVer exit criterion requires a full 24-hour libFuzzer run with the tracked seed corpus. Install/use nightly Rust with `cargo-fuzz`, then run:

```sh
python3 scripts/run_semver_fuzz_24h.py --duration-seconds 86400 \
  --heartbeat-seconds 60 \
  --output-dir target/phase1-semver-fuzz-24h-<run-id>
```

The runner copies `fuzz/corpus/semver_range` into an isolated run directory, preserves the log, writes `result.json`, stores crash/timeout artifacts, and emits heartbeats while fuzzing. The run is accepted only when the result is completed with the full requested duration and no crash or timeout artifacts; preserve any newly minimized failure input in the tracked corpus before starting a replacement run.

After the runner finishes, persist a gate-validated summary (the summarizer checks the libFuzzer final duration line and requires an empty artifact directory):

```sh
python3 scripts/finalize_semver_fuzz.py \
  --run-dir target/phase1-semver-fuzz-24h-<run-id> \
  --output docs/phase1-semver-fuzz-24h.json
```

## Memory ceiling acceptance

Measure the optimized JSM CLI against a synthetic npm-compatible loopback registry with 2,000 direct packages:

```sh
cargo build --release -p jsm-cli --locked
python3 scripts/phase1_memory_2000.py \
  --binary target/release/jsm \
  --packages 2000 \
  --memory-ceiling-mib 512 \
  --output-dir benches/results/phase1-memory-2000
```

The harness serves deterministic two-file tarballs locally, verifies all 2,000 packages are present after install, and records JSM's peak resident set size using Linux `RUSAGE_CHILDREN`. The Phase 1 gate rejects missing/partial runs and runs above the documented 512 MiB process-RSS ceiling. Registry-server memory is separate from the measured JSM process; package extraction is sequential, so only one package archive is in-flight at a time. The fetcher never buffers an archive or file body: its fixed I/O working set is capped at 128 KiB (32 KiB input plus tar-parser overhead and a 64 KiB blob-copy buffer), while the only package-sized allocation is the manifest bounded by 20,000 entries and 4,096 path bytes per entry. Hard limits also cap compressed input at 256 MiB, total expanded bytes at 512 MiB, any one entry at 128 MiB, and compression ratio at 200:1.

Mutating CLI commands use one progress abstraction: an ASCII spinner on a TTY with per-entry file/byte detail, stable per-package status lines on redirected stderr, and no progress noise in `--quiet` or `--json` mode. Command summaries remain separate from this progress channel.

`python3 scripts/phase1_gate.py --verify` checks the SemVer summary, 100% differential report, real-registry top-100 replay, npm/pnpm/JSM benchmark, 2,000-package memory report, and workspace validation suite.
