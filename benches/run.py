#!/usr/bin/env python3
"""Phase 0/1 benchmark harness; registry fixtures are served from loopback."""

from __future__ import annotations

import argparse
import atexit
import base64
import gzip
import hashlib
import http.server
import io
import json
import os
import platform
import re
import shutil
import socketserver
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.parse
from datetime import datetime, timezone
from pathlib import Path
from statistics import median
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
CONFIG_PATH = ROOT / "benches" / "fixtures.json"
CONTAINER_IMAGES = {
    "npm": "docker.io/library/node:24.19.0-bookworm-slim",
    "pnpm": "docker.io/library/node:24.19.0-bookworm-slim",
    "yarn": "docker.io/library/node:24.19.0-bookworm-slim",
    "bun": "docker.io/oven/bun:1.2.22",
    "jsm-stub": "docker.io/library/python:3.12.3-slim-bookworm",
}
COREPACK_VERSIONS = {"pnpm": "9.15.9", "yarn": "1.22.22"}
_ACTIVE_CONTAINERS: set[tuple[str, str]] = set()


def scrub(text: str) -> str:
    text = re.sub(r"(?i)(https?://)[^/\s?#@]+@", r"\1[REDACTED]@", text)
    text = re.sub(
        r"(?i)\b(token|password|passwd|secret|api[_-]?key|access[_-]?token|authorization|auth)\b(\s*[:=]\s*)(?:\"[^\"]*\"|'[^']*'|[^&,\s}\]]+)",
        r'\1\2"[REDACTED]"',
        text,
    )
    return re.sub(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+", "Bearer [REDACTED]", text)


def make_tarball(name: str, package_json: dict[str, Any], files: dict[str, bytes]) -> bytes:
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0) as gzipped:
        with tarfile.open(fileobj=gzipped, mode="w") as archive:
            entries = {"package.json": json.dumps(package_json, sort_keys=True, separators=(",", ":")).encode(), **files}
            for relative, content in sorted(entries.items()):
                info = tarfile.TarInfo(f"package/{relative}")
                info.size = len(content)
                info.mode = 0o644
                info.uid = 0
                info.gid = 0
                info.uname = ""
                info.gname = ""
                info.mtime = 0
                archive.addfile(info, io.BytesIO(content))
    return output.getvalue()


def make_artifact(name: str, native: bool = False) -> dict[str, Any]:
    package_json: dict[str, Any] = {"name": name, "version": "1.0.0", "main": "index.js"}
    files = {"index.js": b"module.exports = 'phase0 fixture';\n"}
    if native:
        package_json["gypfile"] = True
        files["binding.gyp"] = b"{\"targets\": []}\n"
    tarball = make_tarball(name, package_json, files)
    digest = hashlib.sha512(tarball).digest()
    return {
        "name": name,
        "tarball": tarball,
        "integrity": "sha512-" + base64.b64encode(digest).decode("ascii"),
        "shasum": hashlib.sha1(tarball).hexdigest(),
    }


class RegistryState:
    def __init__(self, artifacts: dict[str, dict[str, Any]], delay_ms: float, bandwidth: int):
        self.artifacts = artifacts
        self.delay_ms = delay_ms
        self.bandwidth = bandwidth
        self.base_url = ""


class RegistryHandler(http.server.BaseHTTPRequestHandler):
    state: RegistryState

    def log_message(self, _format: str, *_args: Any) -> None:
        return

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        state = type(self).state
        if state.delay_ms:
            time.sleep(state.delay_ms / 1000.0)
        parsed = urllib.parse.urlparse(self.path)
        path = urllib.parse.unquote(parsed.path).lstrip("/")
        if path.startswith("tarballs/"):
            name = path[len("tarballs/") :].removesuffix(".tgz")
            artifact = state.artifacts.get(name)
            if artifact is None:
                self._send(404, b"not found", "text/plain")
                return
            self._send(200, artifact["tarball"], "application/octet-stream")
            return
        name = path
        artifact = state.artifacts.get(name)
        if artifact is None:
            self._send(404, b"not found", "text/plain")
            return
        metadata = {
            "name": name,
            "dist-tags": {"latest": "1.0.0"},
            "versions": {
                "1.0.0": {
                    "name": name,
                    "version": "1.0.0",
                    "dist": {
                        "tarball": f"{state.base_url}/tarballs/{name}.tgz",
                        "integrity": artifact["integrity"],
                        "shasum": artifact["shasum"],
                    },
                }
            },
        }
        self._send(200, json.dumps(metadata, separators=(",", ":")).encode(), "application/json")

    def _send(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        chunk_size = 16 * 1024
        for offset in range(0, len(body), chunk_size):
            chunk = body[offset : offset + chunk_size]
            if self.state.bandwidth:
                time.sleep(len(chunk) / self.state.bandwidth)
            try:
                self.wfile.write(chunk)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                break


def registry_server(state: RegistryState) -> tuple[socketserver.ThreadingTCPServer, threading.Thread, str]:
    class Server(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        daemon_threads = True

    handler = type("BoundRegistryHandler", (RegistryHandler,), {"state": state})
    server = Server(("127.0.0.1", 0), handler)
    host, port = server.server_address
    base_url = f"http://{host}:{port}"
    state.base_url = base_url
    thread = threading.Thread(target=server.serve_forever, name="phase0-fake-registry", daemon=True)
    thread.start()
    return server, thread, base_url


def generate_fixture(root: Path, kind: str, count: int, registry_url: str, seed: int) -> dict[str, Any]:
    root.mkdir(parents=True, exist_ok=True)
    (root / ".npmrc").write_text(f"registry={registry_url}/\nignore-scripts=true\n", encoding="utf-8")
    if kind == "monorepo":
        workspaces = [f"packages/workspace-{index:03d}" for index in range(count)]
        manifest = {"name": "jsm-bench-monorepo", "version": "1.0.0", "private": True, "workspaces": ["packages/*"]}
        (root / "package.json").write_text(json.dumps(manifest, sort_keys=True) + "\n", encoding="utf-8")
        for index, workspace in enumerate(workspaces):
            path = root / workspace
            path.mkdir(parents=True, exist_ok=True)
            (path / "package.json").write_text(json.dumps({"name": f"jsm-workspace-{index:03d}", "version": "1.0.0", "private": True}, sort_keys=True) + "\n", encoding="utf-8")
        return {"fixture": kind, "workspaces": count, "seed": seed}

    dependency_count = 1 if kind == "native" else count
    prefix = json.loads(CONFIG_PATH.read_text(encoding="utf-8"))["package_prefix"]
    dependencies = {"jsm-native-fixture": "1.0.0"} if kind == "native" else {
        f"{prefix}-{index:04d}": "1.0.0" for index in range(dependency_count)
    }
    manifest = {
        "name": f"jsm-bench-{kind}",
        "version": "1.0.0",
        "private": True,
        "dependencies": dependencies,
    }
    (root / "package.json").write_text(json.dumps(manifest, sort_keys=True) + "\n", encoding="utf-8")
    if kind == "native":
        package = root / "native-fixture"
        package.mkdir()
        (package / "package.json").write_text(json.dumps({"name": "jsm-native-fixture", "version": "1.0.0", "gypfile": True}, sort_keys=True) + "\n", encoding="utf-8")
        (package / "binding.gyp").write_text('{"targets": []}\n', encoding="utf-8")
    return {"fixture": kind, "dependency_count": dependency_count, "seed": seed}


def tool_specs() -> dict[str, dict[str, Any]]:
    jsm_binary = os.environ.get("JSM_BINARY")
    if jsm_binary:
        jsm_binary = str(Path(jsm_binary).expanduser().resolve())
    else:
        jsm_binary = str(ROOT / "target" / "release" / "jsm")
    return {
        "npm": {"executable": "npm", "version": ["npm", "--version"]},
        "pnpm": {"executable": "pnpm", "version": ["pnpm", "--version"]},
        "yarn": {"executable": "yarn", "version": ["yarn", "--version"]},
        "bun": {"executable": "bun", "version": ["bun", "--version"]},
        "jsm": {"executable": jsm_binary, "version": [jsm_binary, "--version"]},
        "jsm-stub": {"executable": sys.executable, "version": [sys.executable, str(ROOT / "benches" / "jsm_stub.py"), "--version"]},
    }


def runtime_prefix(runtime: str) -> list[str]:
    return [runtime, "--cgroup-manager=cgroupfs"] if Path(runtime).name == "podman" else [runtime]


def resolve_container_runtime(requested: str) -> str | None:
    if requested == "none":
        return None
    candidates = [requested] if requested != "auto" else ["podman", "docker"]
    for runtime in candidates:
        if shutil.which(runtime) is None:
            continue
        probe = runtime_prefix(runtime) + (["info", "--format", "json"] if Path(runtime).name == "podman" else ["info"])
        try:
            result = subprocess.run(probe, text=True, capture_output=True, timeout=30, check=False)
        except (OSError, subprocess.TimeoutExpired):
            continue
        if result.returncode == 0:
            return runtime
    if requested != "auto":
        raise RuntimeError(f"requested container runtime {requested!r} is unavailable or not operational")
    return None


def container_command(
    runtime: str,
    tool: str,
    command: list[str],
    project: Path,
    cache_dir: Path,
    toolchain_cache: Path,
    environment: dict[str, str],
    allow_package_download: bool = False,
) -> list[str]:
    if platform.system() != "Linux" or not hasattr(os, "getuid"):
        raise RuntimeError("containerized benchmark mode requires Linux host networking for the loopback registry")
    for directory in (cache_dir / "home", cache_dir / "tmp"):
        directory.mkdir(parents=True, exist_ok=True)
    args = runtime_prefix(runtime) + ["run"]
    if Path(runtime).name == "podman":
        args.append("--cgroups=disabled")
    else:
        args.extend(["--user", f"{os.getuid()}:{os.getgid()}"])
    args.extend([
        "--rm",
        "--network=host",
        "--volume",
        f"{project.resolve()}:/project:rw",
        "--volume",
        f"{cache_dir.resolve()}:/cache:rw",
        "--volume",
        f"{toolchain_cache.resolve()}:/toolchain:rw",
        "--volume",
        f"{ROOT.resolve()}:/repo:ro",
        "--workdir",
        "/project",
    ])
    container_environment = {
        "HOME": "/cache/home",
        "TMPDIR": "/cache/tmp",
        "npm_config_cache": "/cache/npm",
        "YARN_CACHE_FOLDER": "/cache/yarn",
        "PNPM_HOME": "/cache/pnpm",
        "BUN_INSTALL_CACHE_DIR": "/cache/bun",
        "COREPACK_HOME": "/toolchain",
        "COREPACK_DEFAULT_TO_LATEST": "0",
        "COREPACK_ENABLE_NETWORK": "1" if allow_package_download else "0",
    }
    if "CI" in environment:
        container_environment["CI"] = environment["CI"]
    for name in ("npm_config_registry", "NPM_CONFIG_REGISTRY"):
        if name in environment:
            container_environment[name] = environment[name]
    if tool == "pnpm":
        container_environment["npm_config_store_dir"] = "/cache/pnpm-store"
    for name, value in container_environment.items():
        args.extend(["--env", f"{name}={value}"])
    args.extend([CONTAINER_IMAGES[tool], *command])
    return args


def install_command(
    tool: str,
    offline: bool = False,
    ci: bool = False,
    containerized: bool = False,
) -> list[str]:
    package_manager = {
        "pnpm": ["corepack", f"pnpm@{COREPACK_VERSIONS['pnpm']}"] if containerized else ["pnpm"],
        "yarn": ["corepack", f"yarn@{COREPACK_VERSIONS['yarn']}"] if containerized else ["yarn"],
    }
    stub_command = ["python3", "/repo/benches/jsm_stub.py", "install"] if containerized else [sys.executable, str(ROOT / "benches" / "jsm_stub.py"), "install"]
    if ci:
        commands = {
            "npm": ["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund", "--no-progress"],
            "pnpm": [*package_manager["pnpm"], "install", "--frozen-lockfile", "--ignore-scripts"],
            "yarn": [*package_manager["yarn"], "install", "--frozen-lockfile", "--ignore-scripts", "--non-interactive"],
            "bun": ["bun", "install", "--frozen-lockfile", "--ignore-scripts"],
            "jsm": [tool_specs()["jsm"]["executable"], "install", "--frozen-lockfile"],
            "jsm-stub": stub_command,
        }
    else:
        commands = {
            "npm": ["npm", "install", "--ignore-scripts", "--no-audit", "--no-fund", "--no-progress"],
            "pnpm": [*package_manager["pnpm"], "install", "--ignore-scripts", "--no-frozen-lockfile"],
            "yarn": [*package_manager["yarn"], "install", "--ignore-scripts", "--non-interactive"],
            "bun": ["bun", "install", "--ignore-scripts"],
            "jsm": [tool_specs()["jsm"]["executable"], "install"],
            "jsm-stub": stub_command,
        }
    command = list(commands[tool])
    if offline and tool != "jsm-stub":
        if tool == "jsm":
            command.insert(1, "--offline")
        else:
            command.append("--offline")
    return command


def scenario_environment(base: dict[str, str], ci_mode: bool) -> dict[str, str]:
    """Keep host CI variables from leaking into non-CI benchmark scenarios."""
    environment = base.copy()
    environment.pop("CI", None)
    if ci_mode:
        environment["CI"] = "1"
    return environment


def start_container_session(
    runtime: str,
    tool: str,
    root: Path,
    toolchain_cache: Path,
    environment: dict[str, str],
    timeout_seconds: int = 120,
) -> str:
    cache_dir = root / "cache"
    for directory in (cache_dir / "home", cache_dir / "tmp"):
        directory.mkdir(parents=True, exist_ok=True)
    args = runtime_prefix(runtime) + ["run"]
    if Path(runtime).name == "podman":
        args.append("--cgroups=disabled")
    else:
        args.extend(["--user", f"{os.getuid()}:{os.getgid()}"])
    args.extend([
        "--detach",
        "--network=host",
        "--volume",
        f"{root.resolve()}:/workspace-run:rw",
        "--volume",
        f"{toolchain_cache.resolve()}:/toolchain:rw",
        "--volume",
        f"{ROOT.resolve()}:/repo:ro",
        "--workdir",
        "/workspace-run/project",
    ])
    container_environment = {
        "HOME": "/workspace-run/cache/home",
        "TMPDIR": "/workspace-run/cache/tmp",
        "npm_config_cache": "/workspace-run/cache/npm",
        "YARN_CACHE_FOLDER": "/workspace-run/cache/yarn",
        "PNPM_HOME": "/workspace-run/cache/pnpm",
        "BUN_INSTALL_CACHE_DIR": "/workspace-run/cache/bun",
        "COREPACK_HOME": "/toolchain",
        "COREPACK_DEFAULT_TO_LATEST": "0",
        "COREPACK_ENABLE_NETWORK": "0",
    }
    if "CI" in environment:
        container_environment["CI"] = environment["CI"]
    for name in ("npm_config_registry", "NPM_CONFIG_REGISTRY"):
        if name in environment:
            container_environment[name] = environment[name]
    if tool == "pnpm":
        container_environment["npm_config_store_dir"] = "/workspace-run/cache/pnpm-store"
    for name, value in container_environment.items():
        args.extend(["--env", f"{name}={value}"])
    args.extend(["--entrypoint", "/bin/sh", CONTAINER_IMAGES[tool], "-c", "sleep infinity"])
    result = subprocess.run(
        args,
        cwd=ROOT,
        env=os.environ.copy(),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout_seconds,
        check=False,
    )
    if result.returncode != 0 or not result.stdout.strip():
        raise RuntimeError(f"failed to start {tool} container: {scrub((result.stderr or result.stdout)[-1200:])}")
    container_id = result.stdout.strip().splitlines()[-1]
    _ACTIVE_CONTAINERS.add((runtime, container_id))
    return container_id


def stop_container_session(runtime: str, container_id: str) -> None:
    _ACTIVE_CONTAINERS.discard((runtime, container_id))
    subprocess.run(runtime_prefix(runtime) + ["kill", "--signal", "KILL", container_id], text=True, capture_output=True, timeout=30, check=False)
    subprocess.run(runtime_prefix(runtime) + ["rm", "--force", container_id], text=True, capture_output=True, timeout=30, check=False)


def cleanup_active_containers() -> None:
    for runtime, container_id in list(_ACTIVE_CONTAINERS):
        try:
            stop_container_session(runtime, container_id)
        except (OSError, subprocess.TimeoutExpired):
            pass


atexit.register(cleanup_active_containers)


def container_exec_command(
    runtime: str,
    container_id: str,
    command: list[str],
    project: Path,
    root: Path,
    allow_package_download: bool = False,
    environment: dict[str, str] | None = None,
) -> list[str]:
    workdir = "/workspace-run" + ("/" + project.resolve().relative_to(root.resolve()).as_posix())
    args = runtime_prefix(runtime) + ["exec", "--workdir", workdir]
    if allow_package_download:
        args.extend(["--env", "COREPACK_ENABLE_NETWORK=1"])
    if environment is not None and "CI" in environment:
        args.extend(["--env", f"CI={environment['CI']}"])
    args.extend([container_id, *command])
    return args


def run_container_exec(
    runtime: str,
    container_id: str,
    command: list[str],
    project: Path,
    root: Path,
    timeout_seconds: int,
    allow_package_download: bool = False,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        container_exec_command(runtime, container_id, command, project, root, allow_package_download),
        cwd=ROOT,
        env=os.environ.copy(),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout_seconds,
        check=False,
    )


def container_tool_version(runtime: str, tool: str, toolchain_cache: Path) -> str:
    with tempfile.TemporaryDirectory(prefix=f"jsm-version-{tool}-") as temporary:
        root = Path(temporary)
        project = root / "project"
        cache_dir = root / "cache"
        project.mkdir()
        cache_dir.mkdir()
        environment = {"CI": "1"}
        container_id = start_container_session(runtime, tool, root, toolchain_cache, environment)
        try:
            if tool in COREPACK_VERSIONS:
                version = COREPACK_VERSIONS[tool]
                prepared = run_container_exec(runtime, container_id, ["corepack", "prepare", f"{tool}@{version}", "--activate"], project, root, 120, allow_package_download=True)
                if prepared.returncode != 0:
                    detail = scrub((prepared.stderr or prepared.stdout)[-1200:])
                    raise RuntimeError(f"failed to prepare pinned {tool}: {detail}")
                command = ["corepack", f"{tool}@{version}", "--version"]
            else:
                command = {
                    "npm": ["npm", "--version"],
                    "bun": ["bun", "--version"],
                    "jsm-stub": ["python3", "/repo/benches/jsm_stub.py", "--version"],
                }[tool]
            result = run_container_exec(runtime, container_id, command, project, root, 120)
            if result.returncode != 0:
                detail = scrub((result.stderr or result.stdout)[-1200:])
                raise RuntimeError(f"failed to read {tool} version in its pinned image: {detail}")
            return scrub((result.stdout or result.stderr).strip())
        finally:
            stop_container_session(runtime, container_id)


def container_image_digest(runtime: str, image: str) -> str:
    template = "{{.Digest}}" if Path(runtime).name == "podman" else "{{.Id}}"
    result = subprocess.run(
        runtime_prefix(runtime) + ["image", "inspect", "--format", template, image],
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )
    return result.stdout.strip() if result.returncode == 0 and result.stdout.strip() else "unavailable"


def run_one(command: list[str], cwd: Path, environment: dict[str, str], timeout_seconds: int) -> dict[str, Any]:
    start = time.perf_counter()
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout_seconds,
            check=False,
        )
        elapsed = time.perf_counter() - start
        diagnostic = "\n".join(
            part for part in (
                f"stdout:\n{completed.stdout[-900:]}" if completed.stdout else "",
                f"stderr:\n{completed.stderr[-900:]}" if completed.stderr else "",
            ) if part
        )
        return {
            "status": "passed" if completed.returncode == 0 else "failed",
            "return_code": completed.returncode,
            "elapsed_seconds": round(elapsed, 6),
            "diagnostic_tail": scrub(diagnostic[-1800:]),
        }
    except subprocess.TimeoutExpired as error:
        return {
            "status": "timeout",
            "return_code": None,
            "elapsed_seconds": round(time.perf_counter() - start, 6),
            "diagnostic_tail": scrub(str(error)[-1200:]),
        }


def run_tool_command(
    command: list[str],
    project: Path,
    environment: dict[str, str],
    timeout_seconds: int,
    container_runtime: str | None,
    container_id: str | None,
    root: Path,
) -> dict[str, Any]:
    if container_runtime is None:
        return run_one(command, project, environment, timeout_seconds)
    if container_id is None:
        raise RuntimeError("container session was not started")
    wrapped = container_exec_command(
        container_runtime, container_id, command, project, root, environment=environment
    )
    return run_one(wrapped, ROOT, os.environ.copy(), timeout_seconds)


def prepare_branch_switch(project: Path, iteration: int, package_prefix: str, count: int) -> None:
    manifest_path = project / "package.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    all_names = [f"{package_prefix}-{index:04d}" for index in range(count)]
    selected = all_names[::2] if iteration % 2 == 0 else all_names[1::2]
    manifest["dependencies"] = {name: "1.0.0" for name in selected}
    manifest_path.write_text(json.dumps(manifest, sort_keys=True) + "\n", encoding="utf-8")


def clear_directory(directory: Path) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    for child in directory.iterdir():
        if child.is_dir() and not child.is_symlink():
            shutil.rmtree(child)
        else:
            child.unlink()


def benchmark_tool(
    tool: str,
    fixture: str,
    scenario: str,
    source: Path,
    registry_url: str,
    repeat: int,
    timeout_seconds: int,
    package_prefix: str,
    dependency_count: int,
    container_runtime: str | None,
    toolchain_cache: Path,
    image_digest: str | None,
) -> dict[str, Any]:
    executable = tool_specs()[tool]["executable"]
    if container_runtime is None and tool != "jsm-stub" and shutil.which(executable) is None:
        return {"tool": tool, "fixture": fixture, "scenario": scenario, "status": "skipped", "reason": f"{executable} not installed", "samples": []}
    if tool == "jsm" and container_runtime is not None:
        raise RuntimeError("the Phase 1 JSM binary comparison is host-only; use --container-runtime none")
    samples: list[dict[str, Any]] = []
    project: Path | None = None
    cache_dir: Path | None = None
    container_id: str | None = None
    with tempfile.TemporaryDirectory(prefix=f"jsm-bench-{tool}-") as temporary:
        root = Path(temporary)
        if source.exists():
            project = root / "project"
            shutil.copytree(source, project)
        cache_dir = root / "cache"
        cache_dir.mkdir()
        environment = scenario_environment(os.environ, ci_mode=False)
        environment.update({
            "npm_config_registry": registry_url + "/",
            "NPM_CONFIG_REGISTRY": registry_url + "/",
            "npm_config_cache": str(cache_dir / "npm"),
            "YARN_CACHE_FOLDER": str(cache_dir / "yarn"),
            "PNPM_HOME": str(cache_dir / "pnpm"),
            "BUN_INSTALL_CACHE_DIR": str(cache_dir / "bun"),
        })
        if tool == "pnpm":
            environment["npm_config_store_dir"] = str(cache_dir / "pnpm-store")
        if tool == "jsm-stub":
            environment["PATH"] = os.environ.get("PATH", "")
        assert project is not None
        if container_runtime is not None:
            container_id = start_container_session(container_runtime, tool, root, toolchain_cache, environment)

        if scenario in {"warm-store", "warm-lockfile", "reinstall", "offline", "branch-switch"}:
            prewarm = run_tool_command(install_command(tool, containerized=container_runtime is not None), project, environment, timeout_seconds, container_runtime, container_id, root)
            if prewarm["status"] != "passed":
                if container_runtime is not None and container_id is not None:
                    stop_container_session(container_runtime, container_id)
                return {"tool": tool, "fixture": fixture, "scenario": scenario, "status": "failed", "reason": "prewarm failed", "samples": [prewarm]}
        elif scenario == "ci" and tool != "jsm-stub":
            prewarm = run_tool_command(install_command(tool, containerized=container_runtime is not None), project, environment, timeout_seconds, container_runtime, container_id, root)
            if prewarm["status"] != "passed":
                if container_runtime is not None and container_id is not None:
                    stop_container_session(container_runtime, container_id)
                return {"tool": tool, "fixture": fixture, "scenario": scenario, "status": "failed", "reason": "lockfile setup failed", "samples": [prewarm]}
        if scenario == "branch-switch" and fixture != "monorepo":
            prepare_branch_switch(project, 0, package_prefix, dependency_count)

        sample_environment = scenario_environment(environment, ci_mode=(scenario == "ci"))
        for index in range(repeat):
            if scenario == "cold":
                clear_directory(cache_dir)
                cold_project = root / f"cold-{index}"
                shutil.copytree(source, cold_project)
                target = cold_project
            else:
                target = project
            if scenario == "ci":
                shutil.rmtree(target / "node_modules", ignore_errors=True)
                clear_directory(cache_dir)
            command = install_command(tool, offline=(scenario == "offline"), ci=(scenario == "ci"), containerized=container_runtime is not None)
            if scenario == "branch-switch" and fixture != "monorepo" and index > 0:
                prepare_branch_switch(target, index, package_prefix, dependency_count)
            result = run_tool_command(command, target, sample_environment, timeout_seconds, container_runtime, container_id, root)
            result.update({"tool": tool, "fixture": fixture, "scenario": scenario, "sample": index + 1, "command": command})
            samples.append(result)
        if container_runtime is not None and container_id is not None:
            stop_container_session(container_runtime, container_id)
            container_id = None
    status = "passed" if all(sample["status"] == "passed" for sample in samples) else "failed"
    return {
        "tool": tool,
        "fixture": fixture,
        "scenario": scenario,
        "status": status,
        "samples": samples,
        "median_seconds": round(median([sample["elapsed_seconds"] for sample in samples]), 6) if samples else None,
        "execution_isolation": (
            f"isolated container using {CONTAINER_IMAGES[tool]}@{image_digest}; separate temporary project/cache; loopback registry via host network"
            if container_runtime is not None
            else "separate temporary project and per-tool cache; host process (not containerized)"
        ),
    }


def render_markdown(report: dict[str, Any], previous: dict[str, float]) -> str:
    lines = [
        "# JSM Phase 1 Benchmark Comparison" if report.get("benchmark_scope") == "phase1" else "# JSM Phase 0 Benchmark Baseline",
        "",
        f"Generated: `{report['generated_at']}`\\",
        f"Fixture: `{report['fixture']}` (revision {report['fixture_revision']}, seed {report['seed']})\\",
        f"Git revision: `{report['git_revision']}` (working tree clean: `{report['working_tree_clean']}`)\\",
        f"Network shape: latency `{report['network']['latency_ms']} ms/request`, bandwidth `{report['network']['bandwidth_bytes_per_second']} B/s`\\",
        f"Container runtime: `{report['container']['runtime']}` (`{report['container']['version']}`)\\",
        f"Isolation: {report['container']['isolation_summary']}",
        "",
        (
            "> This run measures the actual configured JSM executable against npm and pnpm on a deterministic loopback fixture. It is host-mode evidence only; compare only reports with matching fixture, tool versions, flags, OS, and network profile."
            if report.get("benchmark_scope") == "phase1"
            else "> This is a Phase 0 harness baseline, not a JSM performance claim. `jsm-stub` is deliberately not an installer; local registry fixtures and tool versions are captured. Compare results only when fixture, tools, flags, OS, and network profile match."
        ),
        "",
        "| Tool | Version | Fixture | Scenario | Status | Median (s) | Prior median (s) |",
        "|---|---|---|---|---|---:|---:|",
    ]
    for result in report["results"]:
        version = report["tool_versions"].get(result["tool"], "unavailable")
        key = f"{result['tool']}|{result['fixture']}|{result['scenario']}"
        old = previous.get(key)
        old_text = f"{old:.6f}" if old is not None else "—"
        median_value = result.get("median_seconds")
        median_text = f"{median_value:.6f}" if median_value is not None else "—"
        lines.append(f"| {result['tool']} | {version} | {result['fixture']} | {result['scenario']} | {result['status']} | {median_text} | {old_text} |")
    lines.extend(["", "## Machine and method", "", "```json", json.dumps(report["environment"], indent=2, sort_keys=True), "```", ""])
    return "\n".join(lines)


def load_prior_medians(history_path: Path) -> dict[str, float]:
    previous: dict[str, float] = {}
    if history_path.exists():
        for line in history_path.read_text(encoding="utf-8").splitlines():
            try:
                prior_report = json.loads(line)
            except json.JSONDecodeError:
                continue
            for result in prior_report.get("results", []):
                if result.get("status") != "passed" or result.get("median_seconds") is None:
                    continue
                previous[f"{result['tool']}|{result['fixture']}|{result['scenario']}"] = result[
                    "median_seconds"
                ]
    return previous


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--smoke", action="store_true", help="Run npm and the jsm stub on the small local-registry fixture")
    parser.add_argument("--phase1", action="store_true", help="Compare the real JSM executable with npm and pnpm on the small local-registry fixture (host mode)")
    parser.add_argument("--jsm-binary", type=Path, help="JSM executable for --phase1 (defaults to target/release/jsm)")
    parser.add_argument("--fixture", choices=["small", "medium", "large", "monorepo", "native"], default="small")
    parser.add_argument("--scenario", action="append", choices=["cold", "warm-store", "warm-lockfile", "reinstall", "offline", "branch-switch", "ci", "monorepo", "native"])
    parser.add_argument("--tools", help="Comma-separated subset of npm,pnpm,yarn,bun,jsm,jsm-stub")
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--latency-ms", type=float, default=0.0, help="Fixed delay per local fake-registry request")
    parser.add_argument("--bandwidth-bytes-per-second", type=int, default=0, help="Throttle each local fake-registry response; zero is unshaped")
    parser.add_argument("--timeout-seconds", type=int, default=180)
    parser.add_argument("--container-runtime", choices=["auto", "none", "podman", "docker"], default="auto", help="Run each tool in a pinned Linux container; auto-detect, or explicitly use host mode")
    parser.add_argument("--output-dir", type=Path, help="Report directory (Phase 1 defaults to docs/benchmarks/phase1-baseline)")
    args = parser.parse_args()
    if args.repeat < 1 or args.latency_ms < 0 or args.bandwidth_bytes_per_second < 0:
        parser.error("repeat must be positive and shaping values cannot be negative")
    if args.phase1 and args.smoke:
        parser.error("--phase1 and --smoke are separate benchmark modes")
    if args.phase1 and args.container_runtime not in {"auto", "none"}:
        parser.error("--phase1 is host-only; select --container-runtime none")
    if args.phase1 and args.tools and set(args.tools.split(",")) != {"npm", "pnpm", "jsm"}:
        parser.error("--phase1 requires all three comparison tools: npm,pnpm,jsm")
    if args.jsm_binary:
        os.environ["JSM_BINARY"] = str(args.jsm_binary.expanduser().resolve())
    if args.phase1:
        configured_binary = Path(tool_specs()["jsm"]["executable"])
        if not configured_binary.is_file() or not os.access(configured_binary, os.X_OK):
            parser.error(f"JSM executable is missing or not executable: {configured_binary}; build it or pass --jsm-binary")
        missing_competitors = [tool for tool in ("npm", "pnpm") if shutil.which(tool) is None]
        if missing_competitors:
            parser.error("Phase 1 comparison requires both npm and pnpm; missing: " + ", ".join(missing_competitors))
        container_runtime = None
    else:
        try:
            container_runtime = resolve_container_runtime(args.container_runtime)
        except RuntimeError as error:
            parser.error(str(error))
    if container_runtime is None and args.container_runtime == "auto":
        if not args.phase1:
            print("No container runtime is available; benchmark is using host mode.", file=sys.stderr)

    config = json.loads(CONFIG_PATH.read_text(encoding="utf-8"))
    if args.phase1:
        fixture, scenarios, tools = args.fixture, args.scenario or ["cold", "ci"], ["npm", "pnpm", "jsm"]
    elif args.smoke:
        fixture, scenarios, tools = "small", ["cold", "reinstall", "ci"], ["npm", "jsm-stub"]
    else:
        fixture = args.fixture
        scenarios = args.scenario or ["cold", "warm-store", "warm-lockfile", "reinstall", "offline", "branch-switch", "ci"]
        if fixture == "monorepo":
            scenarios = args.scenario or ["monorepo"]
        elif fixture == "native":
            scenarios = args.scenario or ["native"]
        tools = args.tools.split(",") if args.tools else ["npm", "pnpm", "yarn", "bun", "jsm-stub"]
    unknown_tools = set(tools) - set(tool_specs())
    if unknown_tools:
        parser.error(f"unknown tools: {sorted(unknown_tools)}")
    if "jsm" in tools and container_runtime is not None:
        parser.error("the real JSM comparison is host-only; use --container-runtime none")
    count = config["dependency_sizes"].get(fixture, config["monorepo_workspaces"] if fixture == "monorepo" else 1)
    prefix = config["package_prefix"]
    artifacts = {}
    if fixture == "monorepo":
        artifacts = {}
    elif fixture == "native":
        artifacts["jsm-native-fixture"] = make_artifact("jsm-native-fixture", native=True)
    else:
        artifacts = {f"{prefix}-{index:04d}": make_artifact(f"{prefix}-{index:04d}") for index in range(count)}

    registry_state = RegistryState(artifacts, args.latency_ms, args.bandwidth_bytes_per_second)
    server, thread, registry_url = registry_server(registry_state)
    try:
        with tempfile.TemporaryDirectory(prefix="jsm-bench-fixture-") as directory, tempfile.TemporaryDirectory(prefix="jsm-bench-toolchain-") as toolchain_directory:
            source = Path(directory) / fixture
            toolchain_cache = Path(toolchain_directory)
            fixture_info = generate_fixture(source, fixture, count, registry_url, config["seed"])
            if fixture == "native":
                fixture_info["note"] = "native metadata fixture; lifecycle/build scripts disabled"
            versions: dict[str, str] = {}
            container_images: dict[str, dict[str, str]] = {}
            for tool, spec in tool_specs().items():
                if tool not in tools:
                    continue
                if container_runtime is not None:
                    try:
                        versions[tool] = container_tool_version(container_runtime, tool, toolchain_cache)
                        container_images[tool] = {
                            "image": CONTAINER_IMAGES[tool],
                            "digest": container_image_digest(container_runtime, CONTAINER_IMAGES[tool]),
                        }
                    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                        parser.error(f"container setup failed for {tool}: {scrub(str(error))}")
                else:
                    if tool != "jsm-stub" and shutil.which(spec["executable"]) is None:
                        versions[tool] = "unavailable"
                        continue
                    version = subprocess.run(spec["version"], text=True, capture_output=True, timeout=20, check=False)
                    versions[tool] = scrub((version.stdout or version.stderr).strip()) if version.returncode == 0 else "version-unavailable"
            results = []
            dependency_count = int(fixture_info.get("dependency_count", count))
            for tool in tools:
                for scenario in scenarios:
                    image_digest = container_images.get(tool, {}).get("digest") if container_runtime is not None else None
                    results.append(benchmark_tool(tool, fixture, scenario, source, registry_url, args.repeat, args.timeout_seconds, prefix, dependency_count, container_runtime, toolchain_cache, image_digest))
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)

    try:
        git_revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True, capture_output=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        git_revision = "unknown"
    try:
        working_tree = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, text=True, capture_output=True, check=True).stdout
        working_tree_clean = not working_tree.strip()
    except (OSError, subprocess.CalledProcessError):
        working_tree_clean = False
    try:
        fixture_sha = hashlib.sha256(CONFIG_PATH.read_bytes()).hexdigest()
    except OSError:
        fixture_sha = "unknown"
    if container_runtime is None:
        container_info = {
            "runtime": "none (host mode)",
            "version": "unavailable",
            "images": {},
            "isolation_summary": "separate temporary project directories and per-tool caches; processes run on the host, not in containers.",
        }
    else:
        runtime_version = subprocess.run(runtime_prefix(container_runtime) + ["--version"], text=True, capture_output=True, timeout=20, check=False)
        container_info = {
            "runtime": container_runtime,
            "version": scrub((runtime_version.stdout or runtime_version.stderr).strip()),
            "images": container_images,
            "isolation_summary": "each tool/scenario run uses one fresh container across its samples, with a separate project and cache; container startup and teardown are outside sample timing; the local registry is reached over Linux host networking.",
        }
    report = {
        "schema": "jsm.phase1.benchmark.v1" if args.phase1 else "jsm.phase0.benchmark.v1",
        "benchmark_scope": "phase1" if args.phase1 else "phase0",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "git_revision": git_revision,
        "working_tree_clean": working_tree_clean,
        "fixture_revision": config["fixture_revision"],
        "fixture_sha256": fixture_sha,
        "fixture": fixture_info["fixture"],
        "fixture_details": fixture_info,
        "seed": config["seed"],
        "tool_versions": versions,
        "container": container_info,
        "network": {"transport": "local HTTP fake registry", "latency_ms": args.latency_ms, "bandwidth_bytes_per_second": args.bandwidth_bytes_per_second},
        "environment": {
            "platform": platform.platform(),
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "python": platform.python_version(),
            "cpu_count": os.cpu_count(),
            "node_version": subprocess.run(["node", "--version"], text=True, capture_output=True, check=False).stdout.strip() if shutil.which("node") else "unavailable",
            "npm_version": versions.get("npm", "unavailable"),
        },
        "results": results,
    }
    if args.output_dir is None:
        args.output_dir = ROOT / "docs" / "benchmarks" / "phase1-baseline" if args.phase1 else ROOT / "benches" / "results"
    args.output_dir.mkdir(parents=True, exist_ok=True)
    json_path = args.output_dir / "report.json"
    markdown_path = args.output_dir / "report.md"
    history_path = args.output_dir / "history.jsonl"
    previous = load_prior_medians(history_path)
    markdown = render_markdown(report, previous)
    json_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    markdown_path.write_text(markdown, encoding="utf-8")
    with history_path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps({"generated_at": report["generated_at"], "git_revision": git_revision, "results": results}, sort_keys=True) + "\n")
    print(f"Benchmark report: {json_path}")
    print(f"Markdown report: {markdown_path}")
    failed = (
        [result for result in results if result["status"] != "passed"]
        if args.phase1
        else [result for result in results if result["status"] == "failed"]
    )
    if failed:
        print(f"{len(failed)} benchmark(s) failed; see {markdown_path}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
