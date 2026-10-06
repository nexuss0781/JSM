use std::{fs, time::Duration};

use jsm_testkit::{
    FakeRegistry, FixturePackage, RegistryBehavior, TempProject, install_fixture_from_registry,
};

#[test]
fn fixture_installs_from_loopback_registry_and_is_snapshottable() {
    let registry = FakeRegistry::start().unwrap();
    let fixture = FixturePackage::new("fixture-pkg", "1.2.3")
        .unwrap()
        .file("lib/main.js", b"module.exports = 42;\n".to_vec())
        .unwrap()
        .script("postinstall", "this script must never execute")
        .build()
        .unwrap();
    registry.add_package(fixture);
    registry.set_behavior(RegistryBehavior {
        latency: Duration::from_millis(1),
        error_status: None,
        truncate_payload_at: None,
        bandwidth_bytes_per_second: Some(1_000_000),
        require_bearer_token: None,
    });

    let project = TempProject::new().unwrap();
    let destination = project.path().join("node_modules/fixture-pkg");
    let id =
        install_fixture_from_registry(registry.base_url(), "fixture-pkg", &destination).unwrap();

    assert_eq!(id.name().as_str(), "fixture-pkg");
    assert_eq!(id.version().to_string(), "1.2.3");
    assert_eq!(
        fs::read_to_string(destination.join("lib/main.js")).unwrap(),
        "module.exports = 42;\n"
    );
    assert!(destination.join("package.json").is_file());
    assert!(
        jsm_testkit::filesystem_snapshot(&destination)
            .unwrap()
            .len()
            >= 3
    );
}

#[test]
fn truncated_fixture_is_rejected_before_any_destination_is_visible() {
    let registry = FakeRegistry::start().unwrap();
    let fixture = FixturePackage::new("truncated-pkg", "1.0.0")
        .unwrap()
        .build()
        .unwrap();
    registry.add_package(fixture);
    registry.set_truncation(Some(8));

    let project = TempProject::new().unwrap();
    let destination = project.path().join("node_modules/truncated-pkg");
    let error = install_fixture_from_registry(registry.base_url(), "truncated-pkg", &destination)
        .unwrap_err();
    assert_eq!(error.code(), jsm_core::ErrorCode::Integrity);
    assert!(!destination.exists());
}

#[test]
fn fake_registry_status_failures_are_controlled_and_reported() {
    let registry = FakeRegistry::start().unwrap();
    let fixture = FixturePackage::new("error-pkg", "1.0.0")
        .unwrap()
        .build()
        .unwrap();
    registry.add_package(fixture);
    registry.set_error_status(Some(503));

    let project = TempProject::new().unwrap();
    let destination = project.path().join("node_modules/error-pkg");
    let error =
        install_fixture_from_registry(registry.base_url(), "error-pkg", &destination).unwrap_err();
    assert_eq!(error.code(), jsm_core::ErrorCode::Network);
    assert!(!destination.exists());
}

#[test]
fn traversal_archive_is_rejected_before_publication() {
    let registry = FakeRegistry::start().unwrap();
    let fixture = FixturePackage::new("evil-pkg", "1.0.0")
        .unwrap()
        .malicious_file("../escape.txt", b"must not escape".to_vec())
        .build()
        .unwrap();
    registry.add_package(fixture);

    let project = TempProject::new().unwrap();
    let destination = project.path().join("node_modules/evil-pkg");
    let error =
        install_fixture_from_registry(registry.base_url(), "evil-pkg", &destination).unwrap_err();
    assert_eq!(error.code(), jsm_core::ErrorCode::Security);
    assert!(!destination.exists());
    assert!(!project.path().join("node_modules/escape.txt").exists());
}

#[test]
fn lifecycle_scripts_are_not_executed_during_fixture_install() {
    let registry = FakeRegistry::start().unwrap();
    let project = TempProject::new().unwrap();
    let marker = project.path().join("postinstall-ran");
    let script = format!("touch {}", marker.display());
    let fixture = FixturePackage::new("script-pkg", "1.0.0")
        .unwrap()
        .script("postinstall", script)
        .build()
        .unwrap();
    registry.add_package(fixture);

    let destination = project.path().join("node_modules/script-pkg");
    install_fixture_from_registry(registry.base_url(), "script-pkg", &destination).unwrap();
    assert!(destination.join("package.json").exists());
    assert!(
        !marker.exists(),
        "fixture lifecycle script unexpectedly executed"
    );
}

#[test]
fn fake_registry_can_require_bearer_authentication() {
    let registry = FakeRegistry::start().unwrap();
    let fixture = FixturePackage::new("auth-pkg", "1.0.0")
        .unwrap()
        .build()
        .unwrap();
    registry.add_package(fixture);
    registry.set_required_bearer_token(Some("fixture-only-token".into()));

    let project = TempProject::new().unwrap();
    let destination = project.path().join("node_modules/auth-pkg");
    let error =
        install_fixture_from_registry(registry.base_url(), "auth-pkg", &destination).unwrap_err();
    assert_eq!(error.code(), jsm_core::ErrorCode::Network);
    assert!(!destination.exists());
}
