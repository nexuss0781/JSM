from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from scripts.finalize_semver_fuzz import summarize_run
from scripts.phase1_gate import (
    BASELINE_COMMANDS,
    discover_cli_surface,
    is_approved_non_gating_deferral,
    package_install_path,
    read_phase1_checklist,
    render_report_markdown,
    scrub,
    validate_phase1_benchmark,
    validate_memory_stress,
    validate_semver_differential,
    validate_semver_fuzz,
    validate_top100_report,
    validate_top100_snapshot,
)


class Phase1GateHarnessTests(unittest.TestCase):
    def test_reads_phase1_items_and_gate_from_todo(self) -> None:
        checks = read_phase1_checklist()
        groups = {item["group"] for item in checks}
        self.assertIn("1.1 CLI skeleton", groups)
        self.assertIn("1.14 Integrity and safety baseline", groups)
        self.assertIn("Phase 1 gate", groups)
        self.assertGreaterEqual(len(checks), 30)
        first = next(item for item in checks if item["group"] == "1.1 CLI skeleton")
        todo_lines = Path("TODO.md").read_text(encoding="utf-8").splitlines()
        marker = "x" if first["done"] else " "
        self.assertEqual(
            todo_lines[first["source_line"] - 1].strip(),
            f"- [{marker}] " + first["text"],
        )
        fuzz = next(item for item in checks if "24-hour" in item["text"].lower())
        self.assertFalse(fuzz["done"])
        self.assertTrue(fuzz["deferred_non_gating"])
        self.assertTrue(is_approved_non_gating_deferral(fuzz))

    def test_only_the_24_hour_semver_fuzz_item_can_be_non_gating(self) -> None:
        fuzz = {
            "group": "1.4 Semver engine",
            "text": "Complete the 24-hour SemVer fuzz campaign. [DEFERRED: NON-GATING]",
            "done": False,
            "deferred_non_gating": True,
        }
        self.assertTrue(is_approved_non_gating_deferral(fuzz))
        self.assertFalse(is_approved_non_gating_deferral({**fuzz, "done": True}))
        self.assertFalse(is_approved_non_gating_deferral({**fuzz, "group": "1.12 Bin links"}))
        self.assertFalse(is_approved_non_gating_deferral({**fuzz, "text": "Other test [DEFERRED: NON-GATING]"}))

    def test_gate_report_shows_deferred_fuzz_as_non_gating_not_passed(self) -> None:
        fuzz_text = "Complete the 24-hour SemVer fuzz campaign. [DEFERRED: NON-GATING]"
        evidence = {
            "top100_report_valid": True,
            "top100_report": "top100.json",
            "top100_report_status": "100 package runs passed",
            "phase1_benchmark_valid": True,
            "phase1_benchmark_report": "benchmark.json",
            "phase1_benchmark_status": "benchmark passed",
            "semver_differential_valid": True,
            "semver_differential_report": "semver.json",
            "semver_differential_status": "90,800 cases agree",
            "semver_fuzz_valid": False,
            "semver_fuzz_deferred_non_gating": True,
            "semver_fuzz_report": "fuzz.json",
            "semver_fuzz_status": "24-hour run not completed; explicitly deferred; no valid report claimed",
            "memory_stress_valid": True,
            "memory_stress_report": "memory.json",
            "memory_stress_status": "2,000 packages passed",
        }
        report = {
            "gate_ready": True,
            "generated_at": "2026-10-07T00:00:00Z",
            "git_revision": "abc123",
            "validation_mode": "verify",
            "validations": [],
            "blockers": [],
            "non_gating_deferrals": [{"text": fuzz_text}],
            "checklist": {
                "complete": 52,
                "total": 53,
                "deferred_non_gating": 1,
                "items": [{"group": "1.4 Semver engine", "text": fuzz_text, "done": False}],
            },
            "cli_commands": {command: True for command in BASELINE_COMMANDS},
            "evidence": evidence,
        }
        markdown = render_report_markdown(report)
        self.assertIn("Gate result: PASS", markdown)
        self.assertIn("DEFERRED", markdown)
        self.assertIn("not completed", markdown)
        self.assertNotIn("24-hour SemVer fuzz | PASS", markdown)

    def test_validates_one_hundred_unique_package_names(self) -> None:
        names = [f"fixture-{index}" for index in range(99)] + ["@fixture/scoped"]
        snapshot = {
            "schema": "jsm.phase1.top100.v1",
            "packages": [{"name": name} for name in names],
        }
        self.assertEqual(validate_top100_snapshot(snapshot), names)

    def test_rejects_wrong_count_duplicates_and_unsafe_names(self) -> None:
        too_few = {"schema": "jsm.phase1.top100.v1", "packages": [{"name": "one"}]}
        with self.assertRaisesRegex(ValueError, "exactly 100"):
            validate_top100_snapshot(too_few)
        duplicates = [f"fixture-{index}" for index in range(99)] + ["fixture-0"]
        with self.assertRaisesRegex(ValueError, "unique"):
            validate_top100_snapshot({"schema": "jsm.phase1.top100.v1", "packages": [{"name": name} for name in duplicates]})
        unsafe = [f"fixture-{index}" for index in range(99)] + ["../outside"]
        with self.assertRaisesRegex(ValueError, "invalid package name"):
            validate_top100_snapshot({"schema": "jsm.phase1.top100.v1", "packages": [{"name": name} for name in unsafe]})
        malformed = [f"fixture-{index}" for index in range(99)] + ["unscoped/name"]
        with self.assertRaisesRegex(ValueError, "invalid package name"):
            validate_top100_snapshot({"schema": "jsm.phase1.top100.v1", "packages": [{"name": name} for name in malformed]})

    def test_gate_evidence_requires_a_complete_passing_suite(self) -> None:
        top100 = {
            "schema": "jsm.phase1.top100-results.v1",
            "gate_ready": True,
            "requested_package_count": 100,
            "executed_package_count": 100,
            "passed": 100,
            "failed": 0,
            "source_snapshot_sha256": "a" * 64,
            "results": [
                {
                    "package": f"fixture-{index}",
                    "status": "passed",
                    "commands": [
                        {"stage": stage, "status": "passed"}
                        for stage in (
                            "init", "add", "manifest-after-add", "frozen-install",
                            "installed-package", "remove", "manifest-after-remove",
                            "installed-package-removed",
                        )
                    ],
                }
                for index in range(100)
            ],
        }
        self.assertTrue(validate_top100_report(top100)[0])
        top100["passed"] = 99
        self.assertFalse(validate_top100_report(top100)[0])

        benchmark = {
            "schema": "jsm.phase1.benchmark.v1",
            "benchmark_scope": "phase1",
            "results": [
                {"tool": tool, "scenario": scenario, "status": "passed"}
                for tool in ("jsm", "npm", "pnpm")
                for scenario in ("cold", "ci")
            ],
        }
        self.assertTrue(validate_phase1_benchmark(benchmark)[0])
        benchmark["results"].pop()
        self.assertFalse(validate_phase1_benchmark(benchmark)[0])

    def test_memory_stress_requires_all_packages_within_512_mib(self) -> None:
        report = {
            "schema": "jsm.phase1.memory-stress.v1",
            "status": "passed",
            "exit_code": 0,
            "package_count": 2_000,
            "installed_count": 2_000,
            "peak_rss_kib": 128 * 1024,
            "memory_ceiling_mib": 512,
            "registry": {"packuments": 2_000, "tarballs": 2_000},
        }
        self.assertTrue(validate_memory_stress(report)[0])
        report["installed_count"] = 1_999
        self.assertFalse(validate_memory_stress(report)[0])
        report["installed_count"] = 2_000
        report["peak_rss_kib"] = 512 * 1024 + 1
        self.assertFalse(validate_memory_stress(report)[0])

    def test_semver_evidence_requires_full_agreement_and_clean_24_hour_fuzz(self) -> None:
        differential = {
            "schema": "jsm.semver.differential.v1",
            "case_count": 90_800,
            "agreement_cases": 90_800,
            "mismatch_count": 0,
            "agreement_percent": 100.0,
        }
        self.assertTrue(validate_semver_differential(differential)[0])
        differential["mismatch_count"] = 1
        self.assertFalse(validate_semver_differential(differential)[0])

        fuzz = {
            "schema": "jsm.phase1.semver-fuzz-evidence.v1",
            "status": "completed",
            "requested_fuzz_seconds": 86_400,
            "duration_seconds": 86_400,
            "exit_code": 0,
            "received_signal": None,
            "artifact_count": 0,
            "crash_artifact_count": 0,
            "timeout_artifact_count": 0,
        }
        self.assertTrue(validate_semver_fuzz(fuzz)[0])
        fuzz["duration_seconds"] = 86_399
        self.assertFalse(validate_semver_fuzz(fuzz)[0])
        fuzz["duration_seconds"] = 86_400
        fuzz["timeout_artifact_count"] = 1
        self.assertFalse(validate_semver_fuzz(fuzz)[0])

    def test_fuzz_finalizer_requires_full_run_and_no_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            run_dir = Path(temporary)
            artifacts = run_dir / "artifacts"
            artifacts.mkdir()
            log = run_dir / "fuzz.log"
            log.write_text("Done 123456 runs in 86400 second(s)\n", encoding="utf-8")
            result = {
                "schema": "jsm.phase1.semver-fuzz.v1",
                "status": "completed",
                "started_at": "2026-10-07T00:00:00Z",
                "finished_at": "2026-10-08T00:00:00Z",
                "duration_seconds": 86410,
                "requested_fuzz_seconds": 86_400,
                "exit_code": 0,
                "received_signal": None,
                "command": ["cargo", "+nightly", "fuzz", "run", "semver_range", "--", "-max_total_time=86400"],
                "artifact_directory": str(artifacts),
                "log": str(log),
            }
            (run_dir / "result.json").write_text(json.dumps(result), encoding="utf-8")
            evidence = summarize_run(run_dir)
            self.assertEqual(evidence["status"], "completed")
            self.assertEqual(evidence["fuzz_executions"], 123_456)

            (artifacts / "timeout-example").write_bytes(b"failure")
            failed = summarize_run(run_dir)
            self.assertEqual(failed["status"], "failed")
            self.assertEqual(failed["timeout_artifact_count"], 1)

    def test_cli_surface_does_not_mistake_phase0_demo_for_phase1_commands(self) -> None:
        found = discover_cli_surface("Commands:\n  phase0-demo  Emit sample trace\n")
        self.assertEqual(set(found), set(BASELINE_COMMANDS))
        self.assertFalse(any(found.values()))
        self.assertTrue(discover_cli_surface("Commands:\n  init  Create a project\n")["init"])

    def test_scoped_package_path_is_built_as_components(self) -> None:
        self.assertEqual(package_install_path(Path("/tmp/project"), "@scope/pkg"), Path("/tmp/project/node_modules/@scope/pkg"))

    def test_sensitive_values_are_scrubbed_from_diagnostics(self) -> None:
        output = scrub("Authorization: Bearer abc.secret; https://user:pass@example.test/path")
        self.assertNotIn("abc.secret", output)
        self.assertNotIn("user:pass", output)
        self.assertIn("[REDACTED]", output)


if __name__ == "__main__":
    unittest.main()
