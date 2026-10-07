#!/usr/bin/env python3
"""Enforce the Phase 0 Cargo crate-boundary invariants."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXPECTED = {
    "jsm-cli",
    "jsm-core",
    "jsm-registry",
    "jsm-resolver",
    "jsm-store",
    "jsm-fetch",
    "jsm-linker",
    "jsm-build",
    "jsm-lockfile",
    "jsm-workspace",
    "jsm-security",
    "jsm-daemon",
    "jsm-testkit",
}
# New internal edges require a reviewed update to this explicit policy.
ALLOWED_EDGES = {
    "jsm-cli": {
        "jsm-core",
        "jsm-fetch",
        "jsm-linker",
        "jsm-lockfile",
        "jsm-registry",
        "jsm-resolver",
        "jsm-store",
        "jsm-testkit",  # testkit is a dev dependency only
    },
    "jsm-fetch": {"jsm-core", "jsm-security", "jsm-store", "jsm-testkit"},
    "jsm-linker": {"jsm-core", "jsm-lockfile", "jsm-store"},
    "jsm-lockfile": {"jsm-core"},
    "jsm-registry": {"jsm-core", "jsm-testkit"},  # testkit is a dev dependency only
    "jsm-resolver": {"jsm-core"},
    "jsm-security": {"jsm-core"},
    "jsm-store": {"jsm-core", "jsm-security"},
    "jsm-testkit": {"jsm-core"},
}


def fail(message: str) -> None:
    print(f"workspace policy error: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    metadata = subprocess.run(
        ["cargo", "metadata", "--format-version=1", "--no-deps"],
        cwd=ROOT,
        check=True,
        text=True,
        capture_output=True,
    )
    data = json.loads(metadata.stdout)
    packages = {package["name"]: package for package in data["packages"]}
    if set(packages) != EXPECTED:
        fail(f"expected crates {sorted(EXPECTED)}, found {sorted(packages)}")

    graph: dict[str, set[str]] = {name: set() for name in EXPECTED}
    for name, package in packages.items():
        for dependency in package["dependencies"]:
            if dependency["name"] in EXPECTED:
                graph[name].add(dependency["name"])
        allowed = ALLOWED_EDGES.get(name, set())
        unexpected = graph[name] - allowed
        if unexpected:
            fail(f"{name} has unreviewed JSM dependencies: {sorted(unexpected)}")
    if graph["jsm-core"]:
        fail("jsm-core must not depend on any JSM crate")

    visiting: set[str] = set()
    visited: set[str] = set()

    def visit(name: str) -> None:
        if name in visiting:
            fail(f"dependency cycle includes {name}")
        if name in visited:
            return
        visiting.add(name)
        for dependency in graph[name]:
            visit(dependency)
        visiting.remove(name)
        visited.add(name)

    for name in sorted(graph):
        visit(name)

    binary_owners = []
    for name, package in packages.items():
        if any("bin" in target["kind"] for target in package["targets"]):
            binary_owners.append(name)
    if binary_owners != ["jsm-cli"]:
        fail(f"only jsm-cli may define a binary target; found {sorted(binary_owners)}")

    print("workspace policy OK: 13 crates, no cycles, core isolated, jsm-cli is the only binary")


if __name__ == "__main__":
    main()
