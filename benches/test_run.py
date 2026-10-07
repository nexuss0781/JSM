from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from benches.run import (
    PHASE1_BENCHMARK_PROTOCOL,
    clear_directory,
    container_exec_command,
    configure_tool_cache_dirs,
    install_command,
    load_prior_medians,
    render_markdown,
    scenario_environment,
    tool_specs,
)


class BenchmarkPhaseModeTests(unittest.TestCase):
    def test_scenario_environment_does_not_leak_ci_into_cold_runs(self) -> None:
        base = {"CI": "1", "PATH": "/bin"}
        cold = scenario_environment(base, ci_mode=False)
        ci = scenario_environment(base, ci_mode=True)
        self.assertNotIn("CI", cold)
        self.assertEqual(cold["PATH"], "/bin")
        self.assertEqual(ci["CI"], "1")

    def test_container_exec_applies_ci_only_when_requested(self) -> None:
        root = Path("/tmp/jsm-benchmark-root")
        project = root / "project"
        cold = container_exec_command("docker", "container", ["jsm", "install"], project, root)
        ci = container_exec_command(
            "docker", "container", ["jsm", "install"], project, root, environment={"CI": "1"}
        )
        self.assertNotIn("CI=1", cold)
        self.assertIn("CI=1", ci)

    def test_prior_medians_ignore_failed_benchmark_runs(self) -> None:
        history = [
            {
                "results": [
                    {
                        "tool": "jsm",
                        "fixture": "small",
                        "scenario": "cold",
                        "status": "failed",
                        "median_seconds": 99.0,
                    }
                ]
            },
            {
                "results": [
                    {
                        "tool": "jsm",
                        "fixture": "small",
                        "scenario": "cold",
                        "status": "passed",
                        "median_seconds": 1.25,
                    }
                ]
            },
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "history.jsonl"
            path.write_text("\n".join(json.dumps(row) for row in history) + "\n", encoding="utf-8")
            self.assertEqual(load_prior_medians(path)["jsm|small|cold"], 1.25)

    def test_jsm_cas_and_registry_caches_are_cleared_per_sample(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            cache_root = Path(directory) / "cache"
            cache_root.mkdir()
            environment: dict[str, str] = {}
            configure_tool_cache_dirs(environment, "jsm", cache_root)
            store = Path(environment["JSM_STORE_DIR"])
            registry_cache = Path(environment["JSM_CACHE_DIR"])
            self.assertEqual(store, cache_root / "jsm-store")
            self.assertEqual(registry_cache, cache_root / "jsm-registry")
            store.mkdir()
            registry_cache.mkdir()
            clear_directory(cache_root)
            self.assertFalse(store.exists())
            self.assertFalse(registry_cache.exists())

    def test_prior_medians_ignore_runs_from_a_different_protocol(self) -> None:
        history = [
            {
                "measurement_protocol": "legacy-unisolated-cache",
                "results": [
                    {
                        "tool": "jsm",
                        "fixture": "small",
                        "scenario": "cold",
                        "status": "passed",
                        "median_seconds": 99.0,
                    }
                ],
            },
            {
                "measurement_protocol": PHASE1_BENCHMARK_PROTOCOL,
                "results": [
                    {
                        "tool": "jsm",
                        "fixture": "small",
                        "scenario": "cold",
                        "status": "passed",
                        "median_seconds": 1.25,
                    }
                ],
            },
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "history.jsonl"
            path.write_text("\n".join(json.dumps(row) for row in history) + "\n", encoding="utf-8")
            self.assertEqual(
                load_prior_medians(path, protocol=PHASE1_BENCHMARK_PROTOCOL)["jsm|small|cold"],
                1.25,
            )

    def test_real_jsm_command_uses_configured_binary_and_phase1_flags(self) -> None:
        with patch.dict(os.environ, {"JSM_BINARY": "/tmp/jsm-test-binary"}):
            self.assertEqual(tool_specs()["jsm"]["executable"], "/tmp/jsm-test-binary")
            self.assertEqual(
                install_command("jsm", ci=True, offline=True),
                ["/tmp/jsm-test-binary", "--offline", "install", "--frozen-lockfile"],
            )

    def test_phase0_stub_remains_separate_from_real_jsm(self) -> None:
        stub = install_command("jsm-stub")
        self.assertEqual(stub[-1], "install")
        self.assertIn("jsm_stub.py", stub[1])
        self.assertNotEqual(tool_specs()["jsm"]["executable"], stub[0])

    def test_phase1_report_is_not_labeled_as_stub_smoke(self) -> None:
        report = {
            "benchmark_scope": "phase1",
            "generated_at": "2026-10-07T00:00:00Z",
            "fixture": "small",
            "fixture_revision": 1,
            "seed": 7,
            "git_revision": "abc123",
            "working_tree_clean": True,
            "network": {"latency_ms": 0, "bandwidth_bytes_per_second": 0},
            "container": {"runtime": "none (host mode)", "version": "n/a", "isolation_summary": "isolated fixture projects"},
            "results": [],
            "tool_versions": {},
            "environment": {},
        }
        markdown = render_markdown(report, {})
        self.assertIn("Phase 1 Benchmark Comparison", markdown)
        self.assertIn("actual configured JSM executable", markdown)
        self.assertNotIn("jsm-stub` is deliberately not an installer", markdown)


if __name__ == "__main__":
    unittest.main()
