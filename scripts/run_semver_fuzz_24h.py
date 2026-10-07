#!/usr/bin/env python3
"""Run the Phase 1 SemVer fuzz target with durable logs and regular heartbeats."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import selectors

ROOT = Path(__file__).resolve().parents[1]


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--duration-seconds", type=int, default=86_400)
    parser.add_argument("--heartbeat-seconds", type=int, default=60)
    parser.add_argument("--max-len", type=int, default=4096)
    parser.add_argument("--timeout-seconds", type=int, default=10)
    parser.add_argument("--rss-limit-mb", type=int, default=4096)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--corpus", type=Path, default=ROOT / "fuzz" / "corpus" / "semver_range")
    args = parser.parse_args()
    if min(args.duration_seconds, args.heartbeat_seconds, args.max_len,
           args.timeout_seconds, args.rss_limit_mb) <= 0:
        parser.error("duration, heartbeat, max-len, timeout, and RSS limit must be positive")

    run_stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = (args.output_dir or ROOT / "target" / f"phase1-semver-fuzz-{run_stamp}").resolve()
    if run_dir.exists():
        parser.error(f"output directory already exists: {run_dir}")
    run_dir.mkdir(parents=True)
    corpus_source = args.corpus.resolve()
    if not corpus_source.is_dir():
        parser.error(f"corpus directory does not exist: {corpus_source}")
    corpus_dir = run_dir / "corpus"
    artifact_dir = run_dir / "artifacts"
    shutil.copytree(corpus_source, corpus_dir)
    artifact_dir.mkdir()
    log_path = run_dir / "fuzz.log"
    result_path = run_dir / "result.json"

    command = [
        "cargo", "+nightly", "fuzz", "run", "semver_range", str(corpus_dir), "--",
        f"-max_total_time={args.duration_seconds}",
        f"-max_len={args.max_len}",
        f"-timeout={args.timeout_seconds}",
        f"-rss_limit_mb={args.rss_limit_mb}",
        f"-artifact_prefix={artifact_dir}{os.sep}",
        "-print_final_stats=1",
    ]
    started_at = utc_now()
    started = time.monotonic()
    print(f"SemVer fuzz started: {started_at}", flush=True)
    print(f"Run directory: {run_dir}", flush=True)
    print(f"Command: {json.dumps(command)}", flush=True)
    print(f"Input corpus: {corpus_source}", flush=True)
    print(f"Isolated corpus: {corpus_dir}", flush=True)

    process = subprocess.Popen(
        command,
        cwd=ROOT,
        env=os.environ.copy(),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        bufsize=0,
        start_new_session=True,
    )
    received_signal: int | None = None

    def forward_signal(signum: int, _frame: object) -> None:
        nonlocal received_signal
        received_signal = signum
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass

    old_handlers: dict[int, object] = {}
    for sig in (signal.SIGINT, signal.SIGTERM):
        old_handlers[sig] = signal.signal(sig, forward_signal)

    selector = selectors.DefaultSelector()
    assert process.stdout is not None
    selector.register(process.stdout, selectors.EVENT_READ)
    stdout_open = True
    last_output = started
    last_heartbeat = started
    return_code: int | None = None
    with log_path.open("ab", buffering=0) as log:
        while stdout_open or process.poll() is None:
            ready = selector.select(timeout=min(1.0, args.heartbeat_seconds))
            for key, _ in ready:
                chunk = os.read(key.fd, 64 * 1024)
                if not chunk:
                    selector.unregister(key.fileobj)
                    stdout_open = False
                    continue
                log.write(chunk)
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
                last_output = time.monotonic()
            now = time.monotonic()
            if now - last_heartbeat >= args.heartbeat_seconds and process.poll() is None:
                elapsed = int(now - started)
                quiet_for = int(now - last_output)
                heartbeat = f"[fuzz-heartbeat] elapsed={elapsed}s last_output={quiet_for}s ago process=running\n"
                log.write(heartbeat.encode())
                print(heartbeat, end="", flush=True)
                last_heartbeat = now
            return_code = process.poll()
        return_code = process.wait()

    for sig, handler in old_handlers.items():
        signal.signal(sig, handler)
    finished_at = utc_now()
    result = {
        "schema": "jsm.phase1.semver-fuzz.v1",
        "status": "completed" if return_code == 0 and received_signal is None else "failed",
        "started_at": started_at,
        "finished_at": finished_at,
        "duration_seconds": round(time.monotonic() - started, 3),
        "requested_fuzz_seconds": args.duration_seconds,
        "exit_code": return_code,
        "received_signal": received_signal,
        "command": command,
        "input_corpus": str(corpus_source),
        "isolated_corpus": str(corpus_dir),
        "artifact_directory": str(artifact_dir),
        "log": str(log_path),
    }
    result_path.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(f"Fuzz result: {result_path}", flush=True)
    print(f"Fuzz status: {result['status']} (exit {return_code})", flush=True)
    return return_code if return_code is not None else 1


if __name__ == "__main__":
    raise SystemExit(main())
