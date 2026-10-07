#!/usr/bin/env python3
"""Measure JSM peak RSS while installing a synthetic 2,000-package graph."""
from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import resource
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_PACKAGES = 2_000
DEFAULT_MEMORY_CEILING_MIB = 512


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def package_archive(name: str) -> bytes:
    manifest = json.dumps(
        {"name": name, "version": "1.0.0", "main": "index.js"},
        separators=(",", ":"),
    ).encode() + b"\n"
    module = b"module.exports = true;\n"
    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w:gz", format=tarfile.PAX_FORMAT) as archive:
        for path, content in (("package/package.json", manifest), ("package/index.js", module)):
            entry = tarfile.TarInfo(path)
            entry.size = len(content)
            entry.mode = 0o644
            entry.mtime = 0
            archive.addfile(entry, io.BytesIO(content))
    return payload.getvalue()


class RegistryState:
    def __init__(self, package_count: int) -> None:
        self.lock = threading.Lock()
        self.requests = 0
        self.packuments = 0
        self.tarballs = 0
        self.records: dict[str, tuple[bytes, bytes]] = {}
        for index in range(package_count):
            name = f"phase1-memory-{index:04d}"
            tarball = package_archive(name)
            integrity = base64.b64encode(hashlib.sha512(tarball).digest()).decode("ascii")
            record = {
                "name": name,
                "dist-tags": {"latest": "1.0.0"},
                "versions": {
                    "1.0.0": {
                        "name": name,
                        "version": "1.0.0",
                        "dependencies": {},
                        "dist": {
                            "tarball": f"{{base}}/tarballs/{name}.tgz",
                            "integrity": f"sha512-{integrity}",
                        },
                    }
                },
            }
            self.records[name] = (json.dumps(record, separators=(",", ":")).encode(), tarball)

    def count(self, kind: str) -> None:
        with self.lock:
            self.requests += 1
            if kind == "packument":
                self.packuments += 1
            elif kind == "tarball":
                self.tarballs += 1

    def snapshot(self) -> dict[str, int]:
        with self.lock:
            return {
                "requests": self.requests,
                "packuments": self.packuments,
                "tarballs": self.tarballs,
            }


def make_handler(state: RegistryState) -> type[http.server.BaseHTTPRequestHandler]:
    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            path = self.path.split("?", 1)[0]
            if path.startswith("/tarballs/"):
                name = path.removeprefix("/tarballs/").removesuffix(".tgz")
                record = state.records.get(name)
                if record is None:
                    self.send_error(404)
                    return
                state.count("tarball")
                body, content_type = record[1], "application/octet-stream"
            else:
                name = path.lstrip("/")
                record = state.records.get(name)
                if record is None:
                    self.send_error(404)
                    return
                state.count("packument")
                body = record[0].replace(b"{base}", self.server.base_url.encode("ascii"))
                content_type = "application/json"
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, _format: str, *_args: object) -> None:
            return

    return Handler


def run_install(binary: Path, package_count: int, output_dir: Path, memory_ceiling_mib: int, timeout: int) -> dict[str, Any]:
    if not binary.is_file():
        raise FileNotFoundError(f"JSM binary does not exist: {binary}")
    if sys.platform != "linux":
        raise RuntimeError("the memory acceptance harness currently requires Linux RUSAGE_CHILDREN")

    output_dir.mkdir(parents=True, exist_ok=True)
    log_path = output_dir / "install.log"
    state = RegistryState(package_count)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), make_handler(state))
    server.daemon_threads = True
    server.base_url = f"http://127.0.0.1:{server.server_address[1]}"
    server_thread = threading.Thread(target=server.serve_forever, name="phase1-memory-registry", daemon=True)
    server_thread.start()

    try:
        with tempfile.TemporaryDirectory(prefix="jsm-memory-2000-") as temporary:
            temp = Path(temporary)
            project = temp / "project"
            project.mkdir()
            (project / "package.json").write_text(
                json.dumps(
                    {
                        "name": "phase1-memory-project",
                        "version": "1.0.0",
                        "dependencies": {f"phase1-memory-{index:04d}": "1.0.0" for index in range(package_count)},
                    },
                    indent=2,
                ) + "\n",
                encoding="utf-8",
            )
            store = temp / "store"
            xdg_cache = temp / "cache"
            command = [
                str(binary.resolve()), "--cwd", str(project),
                "--registry", server.base_url,
                "--store-dir", str(store),
                "install", "--no-lockfile",
            ]
            env = os.environ.copy()
            env.update({"JSM_LOG": "error", "XDG_CACHE_HOME": str(xdg_cache), "LC_ALL": "C"})
            started = time.monotonic()
            with log_path.open("wb") as log:
                process = subprocess.run(
                    command,
                    cwd=ROOT,
                    env=env,
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    timeout=timeout,
                    check=False,
                )
            elapsed = time.monotonic() - started
            peak_rss_kib = int(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)
            expected = [f"phase1-memory-{index:04d}" for index in range(package_count)]
            installed_count = sum((project / "node_modules" / name / "package.json").is_file() for name in expected)
            registry_counts = state.snapshot()
            passed = (
                process.returncode == 0
                and installed_count == package_count
                and peak_rss_kib is not None
                and peak_rss_kib <= memory_ceiling_mib * 1024
                and registry_counts["packuments"] >= package_count
                and registry_counts["tarballs"] == package_count
            )
            report = {
                "schema": "jsm.phase1.memory-stress.v1",
                "status": "passed" if passed else "failed",
                "generated_at": utc_now(),
                "binary": str(binary.resolve()),
                "package_count": package_count,
                "installed_count": installed_count,
                "peak_rss_kib": peak_rss_kib,
                "memory_ceiling_mib": memory_ceiling_mib,
                "elapsed_seconds": round(elapsed, 3),
                "exit_code": process.returncode,
                "registry": registry_counts,
                "source": "synthetic loopback npm-compatible registry; two-file tarballs; sequential package extraction",
                "log": str(log_path.resolve()),
                "measurement": "resource.getrusage(RUSAGE_CHILDREN).ru_maxrss (Linux KiB)",
            }
    finally:
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)

    report_path = output_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target" / "release" / "jsm")
    parser.add_argument("--packages", type=int, default=DEFAULT_PACKAGES)
    parser.add_argument("--memory-ceiling-mib", type=int, default=DEFAULT_MEMORY_CEILING_MIB)
    parser.add_argument("--timeout-seconds", type=int, default=600)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "benches" / "results" / "phase1-memory-2000")
    args = parser.parse_args()
    if args.packages <= 0 or args.memory_ceiling_mib <= 0 or args.timeout_seconds <= 0:
        parser.error("package count, memory ceiling, and timeout must be positive")
    try:
        report = run_install(args.binary, args.packages, args.output_dir, args.memory_ceiling_mib, args.timeout_seconds)
    except (OSError, subprocess.TimeoutExpired) as error:
        print(f"Memory stress harness failed: {error}")
        return 1
    print(
        f"Phase 1 memory stress: {report['status']}; {report['installed_count']}/{report['package_count']} packages; "
        f"peak RSS={report['peak_rss_kib']} KiB; report={args.output_dir / 'report.json'}"
    )
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
