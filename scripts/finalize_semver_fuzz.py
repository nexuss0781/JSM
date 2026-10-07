#!/usr/bin/env python3
"""Validate and summarize a completed Phase 1 SemVer fuzz run."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import re
import tempfile
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
DONE_RE = re.compile(r"\bDone\s+(\d+)\s+runs\s+in\s+([0-9]+(?:\.[0-9]+)?)\s+second\(s\)")


def _relative_or_absolute(path: Path) -> str:
    try:
        return str(path.resolve().relative_to(ROOT))
    except ValueError:
        return str(path.resolve())


def summarize_run(run_dir: Path) -> dict[str, Any]:
    run_dir = run_dir.resolve()
    result_path = run_dir / "result.json"
    if not result_path.is_file():
        raise FileNotFoundError(f"missing fuzz runner result: {result_path}")
    result = json.loads(result_path.read_text(encoding="utf-8"))
    if result.get("schema") != "jsm.phase1.semver-fuzz.v1":
        raise ValueError("wrong or missing fuzz runner result schema")

    log_path = Path(result.get("log", run_dir / "fuzz.log"))
    artifact_dir = Path(result.get("artifact_directory", run_dir / "artifacts"))
    if not log_path.is_file():
        raise FileNotFoundError(f"missing fuzz log: {log_path}")
    if not artifact_dir.is_dir():
        raise FileNotFoundError(f"missing fuzz artifact directory: {artifact_dir}")

    fuzz_runs = 0
    fuzz_seconds = 0.0
    with log_path.open("r", encoding="utf-8", errors="replace") as log:
        for line in log:
            match = DONE_RE.search(line)
            if match:
                fuzz_runs = int(match.group(1))
                fuzz_seconds = float(match.group(2))

    artifacts = sorted(path.name for path in artifact_dir.iterdir())
    crash_count = sum(name.startswith("crash-") for name in artifacts)
    timeout_count = sum(name.startswith("timeout-") for name in artifacts)
    requested = result.get("requested_fuzz_seconds", 0)
    command = result.get("command", [])
    requested_in_command = f"-max_total_time={requested}" in command
    successful = (
        result.get("status") == "completed"
        and result.get("exit_code") == 0
        and result.get("received_signal") is None
        and isinstance(requested, (int, float))
        and requested >= 86_400
        and requested_in_command
        and fuzz_seconds >= requested
        and bool(fuzz_runs)
        and not artifacts
    )

    return {
        "schema": "jsm.phase1.semver-fuzz-evidence.v1",
        "status": "completed" if successful else "failed",
        "started_at": result.get("started_at"),
        "finished_at": result.get("finished_at"),
        "requested_fuzz_seconds": requested,
        "duration_seconds": fuzz_seconds,
        "runner_duration_seconds": result.get("duration_seconds"),
        "fuzz_executions": fuzz_runs,
        "exit_code": result.get("exit_code"),
        "received_signal": result.get("received_signal"),
        "artifact_count": len(artifacts),
        "crash_artifact_count": crash_count,
        "timeout_artifact_count": timeout_count,
        "artifacts": artifacts,
        "run_directory": _relative_or_absolute(run_dir),
        "result_file": _relative_or_absolute(result_path),
        "log_file": _relative_or_absolute(log_path),
        "input_corpus": result.get("input_corpus"),
        "runner_command": command,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z"),
    }


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(value, indent=2, sort_keys=True) + "\n"
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "docs" / "phase1-semver-fuzz-24h.json",
    )
    args = parser.parse_args()
    try:
        evidence = summarize_run(args.run_dir)
        write_json(args.output, evidence)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"Unable to summarize SemVer fuzz run: {error}")
        return 1
    print(
        f"SemVer fuzz evidence: {evidence['status']}; "
        f"{evidence['fuzz_executions']} runs in {evidence['duration_seconds']}s; "
        f"{evidence['artifact_count']} artifacts; report: {args.output}"
    )
    return 0 if evidence["status"] == "completed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
