#!/usr/bin/env python3
"""Audit and exercise the JSM Phase 1 acceptance gate.

The gate reads the Phase 1 checklist from TODO.md, verifies the baseline build
and CLI surface, and can snapshot and replay a 100-package npm-registry corpus.
It never treats the Phase 0 fixture installer or jsm-stub benchmark as product
acceptance evidence.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
TODO = ROOT / "TODO.md"
TOP100_SCHEMA = "jsm.phase1.top100.v1"
REPORT_SCHEMA = "jsm.phase1.gate.v1"
BASELINE_COMMANDS = ("init", "add", "install", "remove", "run", "exec", "list", "why")
SEARCH_ENDPOINT = "https://registry.npmjs.org/-/v1/search"
SEARCH_PARAMETERS = {
    "text": "keywords:javascript",
    "size": 100,
    "quality": 0,
    "popularity": 1,
    "maintenance": 0,
}


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def scrub(text: str) -> str:
    text = re.sub(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+", "Bearer [REDACTED]", text)
    text = re.sub(r"(?i)(https?://)[^/\s?#@]+@", r"\1[REDACTED]@", text)
    text = re.sub(
        r"(?i)\b(token|password|passwd|secret|api[_-]?key|access[_-]?token|authorization|auth)\b"
        r"(\s*[:=]\s*)(?:\"[^\"]*\"|'[^']*'|[^&,\s}\]]+)",
        r'\1\2"[REDACTED]"',
        text,
    )
    return text


def read_phase1_checklist(path: Path = TODO) -> list[dict[str, Any]]:
    text = path.read_text(encoding="utf-8")
    start_marker = "## Phase 1 — Core Engine"
    end_marker = "## Phase 2 — Store Management and Safety"
    start = text.find(start_marker)
    if start < 0:
        raise ValueError(f"missing section {start_marker!r} in {path}")
    end = text.find(end_marker, start + len(start_marker))
    if end < 0:
        raise ValueError(f"missing section {end_marker!r} in {path}")

    group = "Phase 1"
    checks: list[dict[str, Any]] = []
    base_line = text[:start].count("\n") + 1
    checkbox = re.compile(r"^\s*- \[([ xX])\] (.+?)\s*$")
    for line_number, line in enumerate(text[start:end].splitlines(), 1):
        if line.startswith("### "):
            group = line[4:].strip()
            continue
        if "**Phase 1 gate**" in line:
            group = "Phase 1 gate"
            continue
        match = checkbox.match(line)
        if match:
            checks.append({
                "group": group,
                "text": match.group(2),
                "done": match.group(1).lower() == "x",
                "source_line": base_line + line_number - 1,
            })
    if not checks:
        raise ValueError("no Phase 1 checklist items were found")
    return checks


def atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            stream.write(content)
            if not content.endswith("\n"):
                stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_name, path)
        os.chmod(path, 0o644)
    finally:
        try:
            os.unlink(temporary_name)
        except FileNotFoundError:
            pass


def run_check(name: str, command: list[str], timeout: int = 300) -> dict[str, Any]:
    started = time.perf_counter()
    try:
        result = subprocess.run(
            command,
            cwd=ROOT,
            env=os.environ.copy(),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
        output = "\n".join(part for part in (result.stdout, result.stderr) if part)
        return {
            "name": name,
            "status": "passed" if result.returncode == 0 else "failed",
            "return_code": result.returncode,
            "elapsed_seconds": round(time.perf_counter() - started, 3),
            "command": command,
            "diagnostic_tail": scrub(output[-2000:]),
        }
    except subprocess.TimeoutExpired as error:
        diagnostic = "\n".join(str(part) for part in (error.stdout, error.stderr) if part)
        return {
            "name": name,
            "status": "timeout",
            "return_code": None,
            "elapsed_seconds": round(time.perf_counter() - started, 3),
            "command": command,
            "diagnostic_tail": scrub(diagnostic[-2000:]) or f"Timed out after {timeout}s",
        }
    except OSError as error:
        return {
            "name": name,
            "status": "unavailable",
            "return_code": None,
            "elapsed_seconds": round(time.perf_counter() - started, 3),
            "command": command,
            "diagnostic_tail": scrub(str(error)),
        }


def scaffold_crates() -> list[str]:
    crates = (
        "jsm-registry", "jsm-resolver", "jsm-store", "jsm-fetch",
        "jsm-linker", "jsm-lockfile", "jsm-security",
    )
    found = []
    for crate in crates:
        source = ROOT / "crates" / crate / "src" / "lib.rs"
        try:
            if "Phase 0 scaffold" in source.read_text(encoding="utf-8"):
                found.append(crate)
        except OSError:
            found.append(crate)
    return found


def validate_top100_report(report: dict[str, Any]) -> tuple[bool, str]:
    results = report.get("results")
    if report.get("schema") != "jsm.phase1.top100-results.v1":
        return False, "wrong or missing schema"
    if report.get("gate_ready") is not True or report.get("requested_package_count") != 100:
        return False, "report does not mark all 100 packages as gate-ready"
    if report.get("executed_package_count") != 100 or report.get("passed") != 100 or report.get("failed") != 0:
        return False, "report does not contain 100 passing package runs"
    if not isinstance(results, list) or len(results) != 100 or any(
        not isinstance(item, dict) or item.get("status") != "passed" for item in results
    ):
        return False, "package result list is incomplete or contains failures"
    names = [item.get("package") for item in results]
    if any(not isinstance(name, str) or not name for name in names) or len(set(names)) != 100:
        return False, "package result names are missing or duplicated"
    expected_stages = {
        "init", "add", "manifest-after-add", "frozen-install", "installed-package",
        "remove", "manifest-after-remove", "installed-package-removed",
    }
    for item in results:
        commands = item.get("commands")
        if not isinstance(commands, list):
            return False, "a package has no command-stage evidence"
        stages = {entry.get("stage") for entry in commands if isinstance(entry, dict) and entry.get("status") == "passed"}
        if not expected_stages.issubset(stages):
            return False, "a package is missing a passing init/add/install/remove stage"
    if not re.fullmatch(r"[0-9a-f]{64}", str(report.get("source_snapshot_sha256", ""))):
        return False, "source snapshot hash is missing or malformed"
    return True, "100 package runs passed"


def validate_phase1_benchmark(report: dict[str, Any]) -> tuple[bool, str]:
    if report.get("schema") != "jsm.phase1.benchmark.v1" or report.get("benchmark_scope") != "phase1":
        return False, "wrong or missing Phase 1 benchmark schema"
    results = report.get("results")
    if not isinstance(results, list) or any(not isinstance(item, dict) for item in results):
        return False, "benchmark results are missing"
    for tool in ("jsm", "npm", "pnpm"):
        tool_runs = [item for item in results if item.get("tool") == tool]
        if not tool_runs or any(item.get("status") != "passed" for item in tool_runs):
            return False, f"{tool} has no successful result or has a failed scenario"
        if not {"cold", "ci"}.issubset({item.get("scenario") for item in tool_runs}):
            return False, f"{tool} is missing the cold or frozen-install benchmark scenario"
    return True, "JSM, npm, and pnpm benchmark scenarios passed"


def validate_semver_differential(report: dict[str, Any]) -> tuple[bool, str]:
    if report.get("schema") != "jsm.semver.differential.v1":
        return False, "wrong or missing SemVer differential schema"
    case_count = report.get("case_count")
    if not isinstance(case_count, int) or case_count <= 0:
        return False, "SemVer differential corpus is empty"
    if (
        report.get("agreement_cases") != case_count
        or report.get("mismatch_count") != 0
        or report.get("agreement_percent") != 100.0
    ):
        return False, "SemVer differential report does not show 100% agreement"
    return True, f"{case_count:,} SemVer cases agree 100% with npm"


def validate_semver_fuzz(report: dict[str, Any]) -> tuple[bool, str]:
    if report.get("schema") != "jsm.phase1.semver-fuzz-evidence.v1":
        return False, "wrong or missing SemVer fuzz evidence schema"
    requested = report.get("requested_fuzz_seconds")
    elapsed = report.get("duration_seconds")
    if not isinstance(requested, (int, float)) or requested < 86_400:
        return False, "SemVer fuzz run requested less than 24 hours"
    if not isinstance(elapsed, (int, float)) or elapsed < requested:
        return False, "SemVer fuzz run did not complete its requested duration"
    if report.get("status") != "completed" or report.get("exit_code") != 0:
        return False, "SemVer fuzz run did not complete successfully"
    if report.get("received_signal") is not None:
        return False, "SemVer fuzz run was interrupted"
    artifact_counts = (
        report.get("artifact_count"),
        report.get("crash_artifact_count"),
        report.get("timeout_artifact_count"),
    )
    if any(not isinstance(count, int) or count != 0 for count in artifact_counts):
        return False, "SemVer fuzz run has crash, timeout, or other failure artifacts"
    return True, "24-hour SemVer fuzz run completed without failure artifacts"


def validate_memory_stress(report: dict[str, Any]) -> tuple[bool, str]:
    if report.get("schema") != "jsm.phase1.memory-stress.v1":
        return False, "wrong or missing 2,000-package memory-stress schema"
    if report.get("status") != "passed" or report.get("exit_code") != 0:
        return False, "2,000-package install did not pass"
    package_count = report.get("package_count")
    installed_count = report.get("installed_count")
    if package_count != 2_000 or installed_count != package_count:
        return False, "memory-stress report does not show all 2,000 packages installed"
    ceiling = report.get("memory_ceiling_mib")
    peak = report.get("peak_rss_kib")
    if ceiling != 512 or not isinstance(peak, int) or peak <= 0 or peak > ceiling * 1024:
        return False, "peak RSS is missing or exceeds the required 512 MiB ceiling"
    registry = report.get("registry")
    if (
        not isinstance(registry, dict)
        or not isinstance(registry.get("packuments"), int)
        or registry["packuments"] < package_count
        or registry.get("tarballs") != package_count
    ):
        return False, "memory-stress registry evidence is incomplete"
    return True, f"2,000 packages installed within {ceiling} MiB (peak {peak / 1024:.1f} MiB)"


def evidence_state(path: Path, validator: Any) -> tuple[bool, str]:
    if not path.is_file():
        return False, "report is missing"
    try:
        report = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return False, f"report is unreadable: {error}"
    if not isinstance(report, dict):
        return False, "report root is not an object"
    return validator(report)


def discover_cli_surface(help_text: str) -> dict[str, bool]:
    return {
        command: bool(re.search(rf"(?m)^\s*{re.escape(command)}(?:\s|$)", help_text))
        for command in BASELINE_COMMANDS
    }


def git_value(*arguments: str) -> str:
    result = subprocess.run(
        ["git", *arguments], cwd=ROOT, text=True, capture_output=True, check=False
    )
    return result.stdout.strip() if result.returncode == 0 else "unknown"


def build_report(verify: bool = False, timeout: int = 300) -> dict[str, Any]:
    validations: list[dict[str, Any]] = []
    if verify:
        validations.extend([
            run_check("Phase 1 gate harness tests", [sys.executable, "scripts/phase1_gate.py", "--self-test"], timeout),
            run_check("Benchmark mode tests", [sys.executable, "-m", "unittest", "benches.test_run", "-v"], timeout),
            run_check("Python syntax", [sys.executable, "-m", "py_compile", "scripts/phase1_gate.py", "scripts/test_phase1_gate.py", "scripts/finalize_semver_fuzz.py", "scripts/phase1_memory_2000.py", "benches/run.py", "benches/test_run.py"], timeout),
            run_check("Workspace dependency policy", [sys.executable, "scripts/check_workspace.py"], timeout),
            run_check("Rust formatting", ["cargo", "fmt", "--all", "--", "--check"], timeout),
            run_check("Workspace build", ["cargo", "build", "--workspace", "--all-targets", "--locked"], timeout),
            run_check("Workspace tests", ["cargo", "test", "--workspace", "--all-features", "--locked"], timeout),
            run_check("Phase 1 CLI fake-registry end-to-end tests", ["cargo", "test", "-p", "jsm-cli", "--test", "phase1_cli", "--locked"], timeout),
            run_check("Clippy warnings as errors", ["cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--", "-D", "warnings"], timeout),
            run_check("Phase 0 fake-registry testkit suite (not product CLI acceptance)", ["cargo", "test", "-p", "jsm-testkit", "--test", "fake_registry_install", "--locked"], timeout),
        ])
    help_result = run_check("CLI help surface", ["cargo", "run", "-q", "-p", "jsm-cli", "--locked", "--", "--help"], timeout)
    validations.append(help_result)
    help_text = help_result.get("diagnostic_tail", "")
    if help_result["status"] == "passed":
        # Re-run with captured stdout intact; the bounded diagnostic tail is not a reliable parser input.
        completed = subprocess.run(
            ["cargo", "run", "-q", "-p", "jsm-cli", "--locked", "--", "--help"],
            cwd=ROOT, text=True, capture_output=True, timeout=timeout, check=False,
        )
        help_text = completed.stdout if completed.returncode == 0 else ""
    cli_commands = discover_cli_surface(help_text)
    checklist = read_phase1_checklist()
    open_checks = sum(not item["done"] for item in checklist)
    stubs = scaffold_crates()
    phase1_cli_e2e = ROOT / "crates" / "jsm-cli" / "tests" / "phase1_cli.rs"
    phase1_benchmark = ROOT / "docs" / "benchmarks" / "phase1-baseline" / "report.json"
    semver_differential = ROOT / "docs" / "semver-differential.json"
    semver_fuzz = ROOT / "docs" / "phase1-semver-fuzz-24h.json"
    memory_stress = ROOT / "benches" / "results" / "phase1-memory-2000" / "report.json"
    top100_evidence = ROOT / "benches" / "results" / "phase1-top100" / "report.json"
    top100_valid, top100_reason = evidence_state(top100_evidence, validate_top100_report)
    benchmark_valid, benchmark_reason = evidence_state(phase1_benchmark, validate_phase1_benchmark)
    semver_differential_valid, semver_differential_reason = evidence_state(
        semver_differential, validate_semver_differential
    )
    semver_fuzz_valid, semver_fuzz_reason = evidence_state(semver_fuzz, validate_semver_fuzz)
    memory_stress_valid, memory_stress_reason = evidence_state(memory_stress, validate_memory_stress)
    blockers: list[str] = []
    if open_checks:
        blockers.append(f"{open_checks} Phase 1 checklist item(s) remain unchecked in TODO.md.")
    missing_commands = [name for name, available in cli_commands.items() if not available]
    if missing_commands:
        blockers.append("Missing baseline CLI commands: " + ", ".join(missing_commands) + ".")
    if stubs:
        blockers.append("Phase 1 implementation crates still declare themselves scaffolds: " + ", ".join(stubs) + ".")
    if not phase1_cli_e2e.is_file():
        blockers.append("No Phase 1 CLI fixture-based end-to-end suite is present.")
    if not top100_valid:
        blockers.append(f"No passing real-registry top-100 acceptance report is present ({top100_reason}).")
    if not benchmark_valid:
        blockers.append(f"No passing Phase 1 benchmark report comparing real JSM with npm and pnpm is present ({benchmark_reason}).")
    if not semver_differential_valid:
        blockers.append(f"SemVer differential evidence is not acceptable ({semver_differential_reason}).")
    if not semver_fuzz_valid:
        blockers.append(f"SemVer 24-hour fuzz evidence is not acceptable ({semver_fuzz_reason}).")
    if not memory_stress_valid:
        blockers.append(f"2,000-package memory evidence is not acceptable ({memory_stress_reason}).")
    if not verify:
        blockers.append("Build, test, formatting, lint, and fake-registry checks were not run by this audit.")
    validations_passed = all(check["status"] == "passed" for check in validations)
    gate_ready = (
        verify and validations_passed and open_checks == 0 and not missing_commands
        and not stubs and phase1_cli_e2e.is_file() and top100_valid and benchmark_valid
        and semver_differential_valid and semver_fuzz_valid and memory_stress_valid
    )
    return {
        "schema": REPORT_SCHEMA,
        "generated_at": utc_now(),
        "repository": str(ROOT),
        "git_revision": git_value("rev-parse", "HEAD"),
        "working_tree_clean_before_report": not bool(git_value("status", "--porcelain")),
        "validation_mode": "verify" if verify else "audit",
        "validations": validations,
        "checklist": {
            "total": len(checklist),
            "complete": len(checklist) - open_checks,
            "open": open_checks,
            "items": checklist,
        },
        "cli_commands": cli_commands,
        "scaffold_crates": stubs,
        "phase1_cli_e2e_present": phase1_cli_e2e.is_file(),
        "evidence": {
            "top100_report": str(top100_evidence.relative_to(ROOT)),
            "top100_report_present": top100_evidence.is_file(),
            "top100_report_valid": top100_valid,
            "top100_report_status": top100_reason,
            "phase1_benchmark_report": str(phase1_benchmark.relative_to(ROOT)),
            "phase1_benchmark_present": phase1_benchmark.is_file(),
            "phase1_benchmark_valid": benchmark_valid,
            "phase1_benchmark_status": benchmark_reason,
            "semver_differential_report": str(semver_differential.relative_to(ROOT)),
            "semver_differential_present": semver_differential.is_file(),
            "semver_differential_valid": semver_differential_valid,
            "semver_differential_status": semver_differential_reason,
            "semver_fuzz_report": str(semver_fuzz.relative_to(ROOT)),
            "semver_fuzz_present": semver_fuzz.is_file(),
            "semver_fuzz_valid": semver_fuzz_valid,
            "semver_fuzz_status": semver_fuzz_reason,
            "memory_stress_report": str(memory_stress.relative_to(ROOT)),
            "memory_stress_present": memory_stress.is_file(),
            "memory_stress_valid": memory_stress_valid,
            "memory_stress_status": memory_stress_reason,
        },
        "blockers": blockers,
        "gate_ready": gate_ready,
    }


def render_report_markdown(report: dict[str, Any]) -> str:
    checks = report["checklist"]
    lines = [
        "# Phase 1 gate status",
        "",
        f"**Gate result: {'PASS' if report['gate_ready'] else 'NOT MET'}** — {checks['complete']} of {checks['total']} Phase 1 checklist items are marked complete.",
        "",
        f"Generated `{report['generated_at']}` from revision `{report['git_revision']}` in `{report['validation_mode']}` mode.",
        "",
        "## Verification results",
        "",
        "| Check | Result | Time |",
        "|---|---|---:|",
    ]
    if report["validations"]:
        for check in report["validations"]:
            lines.append(f"| {check['name']} | {check['status']} | {check['elapsed_seconds']:.3f}s |")
    else:
        lines.append("| Checks | Not run (audit mode) | — |")
    lines.extend(["", "## Remaining blockers", ""])
    if report["blockers"]:
        lines.extend(f"- {blocker}" for blocker in report["blockers"])
    else:
        lines.append("None.")
    lines.extend(["", "## Baseline CLI commands", "", "| Command | Present in help |", "|---|---|"])
    lines.extend(f"| `{name}` | {'yes' if present else 'no'} |" for name, present in report["cli_commands"].items())
    evidence = report["evidence"]
    evidence_rows = (
        ("Real-registry top-100", evidence["top100_report_valid"], evidence["top100_report"], evidence["top100_report_status"]),
        ("npm/pnpm/JSM benchmark", evidence["phase1_benchmark_valid"], evidence["phase1_benchmark_report"], evidence["phase1_benchmark_status"]),
        ("SemVer differential", evidence["semver_differential_valid"], evidence["semver_differential_report"], evidence["semver_differential_status"]),
        ("24-hour SemVer fuzz", evidence["semver_fuzz_valid"], evidence["semver_fuzz_report"], evidence["semver_fuzz_status"]),
        ("2,000-package memory", evidence["memory_stress_valid"], evidence["memory_stress_report"], evidence["memory_stress_status"]),
    )
    lines.extend(["", "## Acceptance evidence", "", "| Evidence | Result | Report | Details |", "|---|---|---|---|"])
    lines.extend(
        f"| {name} | {'PASS' if valid else 'BLOCKED'} | `{path}` | {details} |"
        for name, valid, path, details in evidence_rows
    )
    lines.extend(["", "## Phase 1 checklist", ""])
    last_group = None
    for item in report["checklist"]["items"]:
        if item["group"] != last_group:
            lines.extend([f"### {item['group']}", ""])
            last_group = item["group"]
        marker = "x" if item["done"] else " "
        lines.append(f"- [{marker}] {item['text']}")
    lines.append("")
    return "\n".join(lines)


def snapshot_top100(destination: Path, timeout: int = 30) -> dict[str, Any]:
    query = urllib.parse.urlencode(SEARCH_PARAMETERS)
    url = f"{SEARCH_ENDPOINT}?{query}"
    request = urllib.request.Request(
        url,
        headers={"Accept": "application/json", "User-Agent": "jsm-phase1-acceptance/1"},
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        payload = json.load(response)
    objects = payload.get("objects")
    if not isinstance(objects, list):
        raise ValueError("npm registry search response has no objects array")
    entries: list[dict[str, Any]] = []
    seen: set[str] = set()
    for result in objects:
        package = result.get("package", {})
        name = package.get("name")
        if not isinstance(name, str) or not name or name in seen:
            continue
        seen.add(name)
        entries.append({
            "name": name,
            "version": package.get("version"),
            "popularity_score": result.get("score", {}).get("detail", {}).get("popularity"),
            "search_score": result.get("searchScore"),
        })
        if len(entries) == 100:
            break
    if len(entries) != 100:
        raise ValueError(f"npm search returned only {len(entries)} distinct package names; expected 100")
    snapshot = {
        "schema": TOP100_SCHEMA,
        "generated_at": utc_now(),
        "source": url,
        "selection": {
            "query": SEARCH_PARAMETERS["text"],
            "size": SEARCH_PARAMETERS["size"],
            "score_weights": {"quality": 0, "popularity": 1, "maintenance": 0},
            "note": "A replayable popularity-only snapshot for the JavaScript-keyword search; this is not a claim of the 100 highest global download counts.",
        },
        "registry_total_matches": payload.get("total"),
        "packages": entries,
    }
    atomic_write(destination, json.dumps(snapshot, indent=2, sort_keys=True) + "\n")
    return snapshot


def validate_top100_snapshot(snapshot: dict[str, Any]) -> list[str]:
    if snapshot.get("schema") != TOP100_SCHEMA:
        raise ValueError(f"expected schema {TOP100_SCHEMA!r}")
    packages = snapshot.get("packages")
    if not isinstance(packages, list) or len(packages) != 100:
        raise ValueError("acceptance snapshot must contain exactly 100 packages")
    names: list[str] = []
    for index, package in enumerate(packages):
        name = package.get("name") if isinstance(package, dict) else None
        parts = name.split("/") if isinstance(name, str) else []
        scoped_shape = isinstance(name, str) and name.startswith("@") and len(parts) == 2 and len(parts[0]) > 1
        unscoped_shape = isinstance(name, str) and not name.startswith("@") and len(parts) == 1
        if (
            not isinstance(name, str)
            or not name
            or "\\" in name
            or not (scoped_shape or unscoped_shape)
            or any(part in {"", ".", ".."} for part in parts)
            or any(ord(character) < 33 or ord(character) > 126 for character in name)
        ):
            raise ValueError(f"invalid package name at index {index}")
        names.append(name)
    if len(set(names)) != 100:
        raise ValueError("acceptance snapshot package names must be unique")
    return names


def package_install_path(project: Path, package_name: str) -> Path:
    # Names originate from the validated snapshot and are still constrained before filesystem use.
    return project / "node_modules" / Path(*package_name.split("/"))


def manifest_depends_on(manifest: dict[str, Any], package_name: str) -> bool:
    for field in ("dependencies", "devDependencies", "optionalDependencies"):
        values = manifest.get(field)
        if isinstance(values, dict) and package_name in values:
            return True
    return False


def run_package_acceptance(binary: Path, package_name: str, registry: str, timeout: int) -> dict[str, Any]:
    commands: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="jsm-phase1-acceptance-") as temporary:
        project = Path(temporary)

        def invoke(command: list[str], stage: str) -> bool:
            started = time.perf_counter()
            try:
                result = subprocess.run(
                    [str(binary), "--cwd", str(project), "--registry", registry, *command],
                    cwd=project,
                    env=os.environ.copy(),
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=timeout,
                    check=False,
                )
                output = scrub("\n".join(part for part in (result.stdout, result.stderr) if part))
                commands.append({
                    "stage": stage,
                    "status": "passed" if result.returncode == 0 else "failed",
                    "return_code": result.returncode,
                    "elapsed_seconds": round(time.perf_counter() - started, 3),
                    "diagnostic_tail": output[-1200:],
                })
                return result.returncode == 0
            except subprocess.TimeoutExpired as error:
                diagnostic = "\n".join(str(part) for part in (error.stdout, error.stderr) if part)
                commands.append({
                    "stage": stage,
                    "status": "timeout",
                    "return_code": None,
                    "elapsed_seconds": round(time.perf_counter() - started, 3),
                    "diagnostic_tail": scrub(diagnostic[-1200:]) or f"Timed out after {timeout}s",
                })
                return False
            except OSError as error:
                commands.append({
                    "stage": stage,
                    "status": "unavailable",
                    "return_code": None,
                    "elapsed_seconds": round(time.perf_counter() - started, 3),
                    "diagnostic_tail": scrub(str(error)),
                })
                return False

        stages = [
            (["init", "-y"], "init"),
            (["add", package_name], "add"),
        ]
        for command, stage in stages:
            if not invoke(command, stage):
                break
        else:
            manifest_path = project / "package.json"
            try:
                manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                added = isinstance(manifest, dict) and manifest_depends_on(manifest, package_name)
            except (OSError, json.JSONDecodeError):
                added = False
            commands.append({"stage": "manifest-after-add", "status": "passed" if added else "failed"})
            if added and invoke(["install", "--frozen-lockfile"], "frozen-install"):
                installed_manifest = package_install_path(project, package_name) / "package.json"
                try:
                    installed = json.loads(installed_manifest.read_text(encoding="utf-8"))
                    present = installed.get("name") == package_name
                except (OSError, json.JSONDecodeError):
                    present = False
                commands.append({"stage": "installed-package", "status": "passed" if present else "failed"})
                if present and invoke(["remove", package_name], "remove"):
                    try:
                        after_remove = json.loads(manifest_path.read_text(encoding="utf-8"))
                        removed = isinstance(after_remove, dict) and not manifest_depends_on(after_remove, package_name)
                    except (OSError, json.JSONDecodeError):
                        removed = False
                    commands.append({"stage": "manifest-after-remove", "status": "passed" if removed else "failed"})
                    path_removed = not os.path.lexists(package_install_path(project, package_name))
                    commands.append({"stage": "installed-package-removed", "status": "passed" if path_removed else "failed"})
    return {
        "package": package_name,
        "status": "passed" if commands and all(item.get("status") == "passed" for item in commands) else "failed",
        "commands": commands,
    }


def run_top100(snapshot_path: Path, binary: Path, limit: int, timeout: int, output_dir: Path) -> dict[str, Any]:
    snapshot = json.loads(snapshot_path.read_text(encoding="utf-8"))
    names = validate_top100_snapshot(snapshot)
    if not binary.is_file():
        raise FileNotFoundError(f"JSM binary does not exist: {binary}")
    selected = names[:limit]
    results: list[dict[str, Any]] = []
    registry = "https://registry.npmjs.org/"
    for index, name in enumerate(selected, 1):
        print(f"[{index}/{len(selected)}] {name}", flush=True)
        results.append(run_package_acceptance(binary.resolve(), name, registry, timeout))
    report = {
        "schema": "jsm.phase1.top100-results.v1",
        "generated_at": utc_now(),
        "source_snapshot": str(snapshot_path),
        "source_snapshot_sha256": hashlib.sha256(snapshot_path.read_bytes()).hexdigest(),
        "registry": registry,
        "binary": str(binary.resolve()),
        "requested_package_count": 100,
        "executed_package_count": len(results),
        "results": results,
        "passed": sum(result["status"] == "passed" for result in results),
        "failed": sum(result["status"] != "passed" for result in results),
        "gate_ready": len(results) == 100 and all(result["status"] == "passed" for result in results),
    }
    json_path = output_dir / "report.json"
    markdown_path = output_dir / "report.md"
    report["report_json"] = str(json_path)
    report["report_markdown"] = str(markdown_path)
    write_report(report, json_path, markdown_path)
    return report


def write_report(report: dict[str, Any], json_path: Path, markdown_path: Path) -> None:
    atomic_write(json_path, json.dumps(report, indent=2, sort_keys=True) + "\n")
    if report.get("schema") == "jsm.phase1.top100-results.v1":
        lines = [
            "# Phase 1 real-registry top-100 acceptance",
            "",
            f"**Result: {'PASS' if report['gate_ready'] else 'NOT MET'}** — {report['passed']} passed, {report['failed']} failed, {report['executed_package_count']} executed of 100.",
            "",
            f"Generated `{report['generated_at']}` using `{report['binary']}` against `{report['registry']}`.",
            "",
            "| Package | Result | Failed stage |",
            "|---|---|---|",
        ]
        for result in report["results"]:
            failed = next((item["stage"] for item in result["commands"] if item.get("status") != "passed"), "—")
            lines.append(f"| `{result['package']}` | {result['status']} | {failed} |")
        atomic_write(markdown_path, "\n".join(lines) + "\n")
    else:
        atomic_write(markdown_path, render_report_markdown(report))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--verify", action="store_true", help="run local quality and fake-registry checks, then evaluate the Phase 1 gate")
    modes.add_argument("--snapshot-top100", type=Path, metavar="FILE", help="fetch and pin a popularity-weighted npm registry search snapshot of 100 packages")
    modes.add_argument("--run-top100", type=Path, metavar="SNAPSHOT", help="exercise init/add/frozen install/remove against all packages in a snapshot")
    modes.add_argument("--self-test", action="store_true", help="run the harness unit tests")
    parser.add_argument("--jsm", type=Path, default=ROOT / "target" / "release" / "jsm", help="JSM executable for --run-top100")
    parser.add_argument("--limit", type=int, default=100, help="maximum packages to run from the snapshot (default: 100)")
    parser.add_argument("--command-timeout", type=int, default=300, help="timeout per CLI command in seconds")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "benches" / "results" / "phase1-top100", help="top-100 report directory")
    parser.add_argument("--json-out", type=Path, default=ROOT / "docs" / "phase-1-status.json")
    parser.add_argument("--markdown-out", type=Path, default=ROOT / "docs" / "phase-1-status.md")
    args = parser.parse_args(argv)
    if args.limit < 1 or args.limit > 100 or args.command_timeout < 1:
        parser.error("--limit must be 1..100 and --command-timeout must be positive")
    if args.self_test:
        result = subprocess.run([sys.executable, "-m", "unittest", "scripts.test_phase1_gate", "-v"], cwd=ROOT, check=False)
        return result.returncode
    if args.snapshot_top100:
        try:
            snapshot = snapshot_top100(args.snapshot_top100)
        except Exception as error:  # network and schema errors should be actionable
            print(f"Unable to snapshot npm top-100 acceptance corpus: {scrub(str(error))}", file=sys.stderr)
            return 1
        print(f"Saved {len(snapshot['packages'])} packages to {args.snapshot_top100}")
        print(f"Selection: {snapshot['selection']['note']}")
        return 0
    if args.run_top100:
        try:
            report = run_top100(args.run_top100, args.jsm, args.limit, args.command_timeout, args.output_dir)
        except Exception as error:
            print(f"Unable to run top-100 acceptance: {scrub(str(error))}", file=sys.stderr)
            return 1
        print(f"Acceptance: {report['passed']} passed, {report['failed']} failed; report: {report['report_markdown']}")
        return 0 if report["gate_ready"] else 1

    report = build_report(verify=args.verify)
    write_report(report, args.json_out, args.markdown_out)
    print(render_report_markdown(report))
    print(f"\nJSON report: {args.json_out}\nMarkdown report: {args.markdown_out}")
    if args.verify:
        return 0 if report["gate_ready"] else 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
