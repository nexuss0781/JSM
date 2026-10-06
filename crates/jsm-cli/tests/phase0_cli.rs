use assert_cmd::Command;
use serde_json::Value;

#[test]
fn help_exposes_phase_zero_flags() {
    let output = Command::cargo_bin("jsm")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in ["--verbose", "--quiet", "--trace"] {
        assert!(help.contains(flag), "missing {flag} in help: {help}");
    }
}

#[test]
fn sample_install_produces_chrome_trace_with_each_pipeline_stage() {
    let directory = tempfile::tempdir().unwrap();
    let trace_path = directory.path().join("install-trace.json");
    let output = Command::cargo_bin("jsm")
        .unwrap()
        .arg("--trace")
        .arg(&trace_path)
        .arg("phase0-demo")
        .env("JSM_LOG", "error")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let trace: Value = serde_json::from_slice(&std::fs::read(trace_path).unwrap()).unwrap();
    let events = trace
        .as_array()
        .or_else(|| trace["traceEvents"].as_array())
        .expect("Chrome traceEvents array");
    for stage in ["resolve", "fetch", "extract", "write", "link", "build"] {
        assert!(
            events.iter().any(|event| event["name"] == stage),
            "missing {stage} in {events:?}"
        );
    }
}

#[test]
fn log_boundary_redacts_sensitive_fields() {
    let output = Command::cargo_bin("jsm")
        .unwrap()
        .arg("--verbose")
        .arg("phase0-demo")
        .env_remove("JSM_LOG")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("phase0-hidden-canary"),
        "secret leaked: {stderr}"
    );
    assert!(
        stderr.contains("[REDACTED]"),
        "redaction marker missing: {stderr}"
    );
}

#[test]
fn environment_filter_and_quiet_mode_control_logs() {
    let from_environment = Command::cargo_bin("jsm")
        .unwrap()
        .arg("phase0-demo")
        .env("JSM_LOG", "debug")
        .output()
        .unwrap();
    assert!(from_environment.status.success());
    let environment_stderr = String::from_utf8_lossy(&from_environment.stderr);
    assert!(environment_stderr.contains("DEBUG"));
    assert!(environment_stderr.contains("[REDACTED]"));

    let quiet = Command::cargo_bin("jsm")
        .unwrap()
        .arg("--quiet")
        .arg("phase0-demo")
        .env("JSM_LOG", "debug")
        .output()
        .unwrap();
    assert!(quiet.status.success());
    let quiet_stderr = String::from_utf8_lossy(&quiet.stderr);
    assert!(!quiet_stderr.contains("INFO"));
    assert!(!quiet_stderr.contains("DEBUG"));
}
