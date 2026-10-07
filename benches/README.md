# Phase 0 benchmark harness

Run the quick comparison with `python3 benches/run.py --smoke`. By default, the harness selects an operational Podman or Docker runtime and runs each competitor in a fresh container. Fixture metadata and tarballs come only from the loopback fake registry; pulling pinned container images and preparing the pinned Corepack package managers may use the network before timed samples. Use `--container-runtime none` only for a host-process diagnostic run; that mode is not equivalent evidence for the container requirement. Use `--fixture medium`, `--fixture large`, `--fixture monorepo`, or `--fixture native` for the pinned data shapes in `fixtures.json`; `--scenario` and `--tools` select a smaller run. The `ci` scenario prepares a lockfile from the loopback registry, then removes installed modules and caches before running npm `ci` or the package manager's frozen-lockfile mode. `--latency-ms` and `--bandwidth-bytes-per-second` shape each fake-registry response deterministically. Example:

```sh
python3 benches/run.py --fixture medium --scenario cold --scenario warm-lockfile \
  --tools npm,pnpm,yarn,bun,jsm-stub --repeat 3 \
  --latency-ms 25 --bandwidth-bytes-per-second 1048576
```

The harness generates packages with stable tar metadata, serves packuments and tarballs on loopback, disables lifecycle scripts, and records exact tool versions, pinned image tags and digests, container runtime, fixture hash, Git revision, machine details, command, timing, and network profile. Each tool/scenario run gets a fresh isolated container, reused across that run's repeated samples; container startup and teardown are outside the measured sample. Every run has a separate temporary project and package-manager cache. On Linux, host networking lets containers reach the loopback fake registry; all fixture package URLs point to that registry. Results are written as `report.json`, `report.md`, and append-only `history.jsonl` under `benches/results/` (or `--output-dir`). The Markdown report includes prior-run medians for trend comparison.

**Interpretation:** Phase 0 `warm-store` and `warm-lockfile` labels measure repeated package-manager runs/cache behavior, not a JSM shared-store implementation. `jsm-stub` only reads a manifest and records a marker; it is not an installer, and its measurements must never be presented as JSM performance. Baselines calibrate fixtures and methodology; they do not establish the product's performance targets.

Container images are pinned by tag and their resolved digest is captured in each report; npm uses the pinned Node image, pnpm/Yarn use pinned Corepack versions, Bun and the Python stub use pinned images. The benchmark smoke job explicitly requires Docker so CI cannot silently fall back to host mode. Docker containers run as the host UID/GID to keep bind-mounted caches writable and removable; rootless Podman uses its default UID mapping. A local report does not establish reproducibility across operating systems; compare only reports with matching fixture hash, tool and image versions, flags, runtime, OS, and network profile.

## Phase 1 real-binary comparison

The Phase 0 smoke remains unchanged and uses only `jsm-stub`. Once the Phase 1 CLI is implemented, build and benchmark the actual binary separately:

```sh
cargo build --release -p jsm-cli --locked
python3 benches/run.py --phase1 --jsm-binary target/release/jsm \
  --container-runtime none --repeat 3
```

This mode runs npm, pnpm, and the supplied JSM executable against the same deterministic small fixture and local fake registry. It writes a `jsm.phase1.benchmark.v1` report to `docs/benchmarks/phase1-baseline/` by default. Phase 1 mode is host-only (all tools run on the same machine with per-tool project and cache directories), so its results must not be compared directly with the containerized Phase 0 baseline or across mismatched operating systems and tool versions. A report is valid Phase 1 evidence only if all three tools complete the selected scenarios; a Phase 0 stub result is never substituted for JSM. The harness removes an inherited `CI` value for ordinary scenarios and sets `CI=1` only for the final `ci` sample, so cold and prewarm installs do not accidentally become frozen-lockfile installs.
