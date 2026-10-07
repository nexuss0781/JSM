use std::{fs, process::Command as ProcessCommand, thread};

use assert_cmd::Command;
use jsm_store::{ReferenceRegistry, Store};
use jsm_testkit::{FakeRegistry, FixtureArtifact, FixturePackage};
use serde_json::Value;

fn cli(cwd: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("jsm").unwrap();
    command
        .arg("--cwd")
        .arg(cwd)
        .arg("--store-dir")
        .arg(cwd.join(".phase2-store"))
        .env("JSM_LOG", "error")
        .env("CI", "0")
        .env_remove("JSM_CI");
    command
}

fn fixture(name: &str, version: &str, body: &str) -> FixtureArtifact {
    FixturePackage::new(name, version)
        .unwrap()
        .file("lib/version.txt", body.as_bytes().to_vec())
        .unwrap()
        .build()
        .unwrap()
}

fn install_one(registry: &FakeRegistry, project: &std::path::Path) {
    fs::write(
        project.join("package.json"),
        r#"{"name":"phase2-recovery","version":"1.0.0","dependencies":{"fixture-pkg":"^1.0.0"}}"#,
    )
    .unwrap();
    let install = cli(project)
        .args([
            "--registry",
            &format!("{}/meta", registry.base_url()),
            "install",
        ])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
}

#[test]
fn interrupted_install_restores_last_good_state_at_each_commit_boundary() {
    for boundary in [
        "after-journal",
        "after-node-modules-backup",
        "after-lockfile-backup",
        "after-link",
        "after-lockfile",
        "after-references",
    ] {
        let registry = FakeRegistry::start().unwrap();
        registry.add_package(fixture("fixture-pkg", "1.0.0", "one\n"));
        let project = tempfile::tempdir().unwrap();
        install_one(&registry, project.path());
        assert_eq!(
            fs::read_to_string(
                project
                    .path()
                    .join("node_modules/fixture-pkg/lib/version.txt")
            )
            .unwrap(),
            "one\n"
        );

        registry.add_package(fixture("fixture-pkg", "2.0.0", "two\n"));
        let crash = cli(project.path())
            .args([
                "--registry",
                &format!("{}/meta", registry.base_url()),
                "install",
            ])
            .env("JSM_TEST_CRASH_AT", boundary)
            .output()
            .unwrap();
        assert!(
            !crash.status.success(),
            "fault injection at {boundary} did not terminate install"
        );
        assert!(
            project.path().join(".jsm-install-journal.json").exists(),
            "{boundary} did not leave a recovery journal"
        );

        // Recovery runs before lock validation. Frozen mode must therefore see the old
        // package.json-compatible lock and restore the previous known-good tree/refs.
        let recovery = cli(project.path())
            .args(["--offline", "install", "--frozen-lockfile"])
            .output()
            .unwrap();
        assert!(
            recovery.status.success(),
            "recovery after {boundary}: {}",
            String::from_utf8_lossy(&recovery.stderr)
        );
        assert_eq!(
            fs::read_to_string(
                project
                    .path()
                    .join("node_modules/fixture-pkg/lib/version.txt")
            )
            .unwrap(),
            "one\n"
        );
        assert!(!project.path().join(".jsm-install-journal.json").exists());
        let lock = jsm_lockfile::Lockfile::read(project.path().join("jsm.lock")).unwrap();
        assert_eq!(lock.packages.values().next().unwrap().version, "1.0.0");
        let store = Store::new(project.path().join(".phase2-store")).unwrap();
        let registry_index = ReferenceRegistry::open(store.root()).unwrap();
        let record = registry_index
            .packages()
            .unwrap()
            .into_iter()
            .find(|package| package.name == "fixture-pkg" && package.version == "1.0.0")
            .unwrap();
        assert_eq!(record.reference_count, 1);
    }
}

#[test]
fn store_management_commands_return_json_and_refuse_referenced_removal() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("fixture-pkg", "1.0.0", "one\n"));
    let project = tempfile::tempdir().unwrap();
    install_one(&registry, project.path());
    let registry_url = format!("{}/meta", registry.base_url());

    let status = cli(project.path())
        .args(["--json", "store", "status"])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let value: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["schema"], "jsm.v1.store.status");
    assert!(value["package_count"].as_u64().unwrap() >= 1);

    let list = cli(project.path())
        .args(["--json", "store", "list", "--filter", "fixture*"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let value: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["packages"][0]["name"], "fixture-pkg");

    let usage = cli(project.path())
        .args(["--json", "store", "usage", "fixture-pkg@1.0.0"])
        .output()
        .unwrap();
    assert!(usage.status.success());
    let value: Value = serde_json::from_slice(&usage.stdout).unwrap();
    assert_eq!(value["projects"].as_array().unwrap().len(), 1);

    let refused = cli(project.path())
        .args(["store", "remove", "fixture-pkg@1.0.0", "--yes"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("referenced"));

    let dry = cli(project.path())
        .args([
            "--json",
            "store",
            "remove",
            "fixture-pkg@1.0.0",
            "--force",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(dry.status.success());
    let value: Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert!(value["reclaimed_physical_bytes"].as_u64().unwrap() > 0);

    let verify = cli(project.path())
        .args(["--json", "store", "verify", "--full"])
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let value: Value = serde_json::from_slice(&verify.stdout).unwrap();
    assert_eq!(value["schema"], "jsm.v1.store.verify");
    assert_eq!(value["findings"].as_array().unwrap().len(), 0);

    let versions = cli(project.path())
        .args([
            "--json",
            "--registry",
            &registry_url,
            "versions",
            "fixture-pkg",
        ])
        .output()
        .unwrap();
    assert!(versions.status.success());
    let value: Value = serde_json::from_slice(&versions.stdout).unwrap();
    assert_eq!(value["schema"], "jsm.v1.versions");
    assert_eq!(value["versions"][0]["local"], true);

    let usage_value: Value = serde_json::from_slice(&usage.stdout).unwrap();
    let project_id = usage_value["projects"][0]["project_id"].as_str().unwrap();
    let preview = cli(project.path())
        .args(["--json", "store", "forget-project", project_id, "--dry-run"])
        .output()
        .unwrap();
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview_value: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview_value["schema"], "jsm.v1.store.forget-project");
    assert_eq!(preview_value["released_package_references"], 1);
    let forget = cli(project.path())
        .args(["store", "forget-project", project_id, "--yes"])
        .output()
        .unwrap();
    assert!(
        forget.status.success(),
        "{}",
        String::from_utf8_lossy(&forget.stderr)
    );

    let dry_remove = cli(project.path())
        .args([
            "--json",
            "store",
            "remove",
            "fixture-pkg@1.0.0",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(
        dry_remove.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_remove.stderr)
    );
    let dry_value: Value = serde_json::from_slice(&dry_remove.stdout).unwrap();
    let actual_remove = cli(project.path())
        .args(["--json", "store", "remove", "fixture-pkg@1.0.0", "--yes"])
        .output()
        .unwrap();
    assert!(
        actual_remove.status.success(),
        "{}",
        String::from_utf8_lossy(&actual_remove.stderr)
    );
    let actual_value: Value = serde_json::from_slice(&actual_remove.stdout).unwrap();
    assert_eq!(
        dry_value["reclaimed_logical_bytes"],
        actual_value["reclaimed_logical_bytes"]
    );
    assert_eq!(
        dry_value["reclaimed_physical_bytes"],
        actual_value["reclaimed_physical_bytes"]
    );
}

#[test]
fn fifty_processes_share_one_download_for_the_same_package_identity() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("single-flight-pkg", "1.0.0", "shared\n"));
    let project = tempfile::tempdir().unwrap();
    let store = project.path().join(".phase2-store");
    let registry_url = format!("{}/meta", registry.base_url());
    let binary = Command::cargo_bin("jsm")
        .unwrap()
        .get_program()
        .to_os_string();
    let mut workers = Vec::new();
    for _ in 0..50 {
        let binary = binary.clone();
        let cwd = project.path().to_path_buf();
        let store = store.clone();
        let registry_url = registry_url.clone();
        workers.push(thread::spawn(move || {
            ProcessCommand::new(binary)
                .args(["--json", "--cwd"])
                .arg(cwd)
                .arg("--store-dir")
                .arg(store)
                .arg("--registry")
                .arg(registry_url)
                .args(["store", "add", "single-flight-pkg@1.0.0"])
                .env("JSM_LOG", "error")
                .env("CI", "0")
                .output()
                .unwrap()
        }));
    }
    for worker in workers {
        let result = worker.join().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(
        registry.tarball_request_count(),
        1,
        "identical concurrent identities should download one archive"
    );
    let store = Store::new(store).unwrap();
    assert_eq!(store.list_package_manifests().unwrap().len(), 1);
}

#[test]
fn concurrent_install_and_gc_never_remove_a_live_tree_or_reference() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("gc-race-pkg", "1.0.0", "safe\n"));
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"gc-race","version":"1.0.0","dependencies":{"gc-race-pkg":"1.0.0"}}"#,
    )
    .unwrap();
    let binary = Command::cargo_bin("jsm")
        .unwrap()
        .get_program()
        .to_os_string();
    let store_dir = project.path().join(".phase2-store");
    let registry_url = format!("{}/meta", registry.base_url());
    let cwd = project.path().to_path_buf();
    let install_binary = binary.clone();
    let install_store = store_dir.clone();
    let install_registry = registry_url.clone();
    let install = thread::spawn(move || {
        ProcessCommand::new(install_binary)
            .args(["--cwd"])
            .arg(&cwd)
            .arg("--store-dir")
            .arg(&install_store)
            .arg("--registry")
            .arg(&install_registry)
            .arg("install")
            .env("JSM_LOG", "error")
            .env("CI", "0")
            .output()
            .unwrap()
    });
    let gc = ProcessCommand::new(binary)
        .args(["--cwd"])
        .arg(project.path())
        .arg("--store-dir")
        .arg(&store_dir)
        .args(["store", "gc", "--yes"])
        .env("JSM_LOG", "error")
        .env("CI", "0")
        .output()
        .unwrap();
    let installed = install.join().unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    assert_eq!(
        fs::read_to_string(
            project
                .path()
                .join("node_modules/gc-race-pkg/lib/version.txt")
        )
        .unwrap(),
        "safe\n"
    );
    let store = Store::new(store_dir).unwrap();
    let record = store
        .reference_registry()
        .unwrap()
        .packages()
        .unwrap()
        .into_iter()
        .find(|row| row.name == "gc-race-pkg" && row.version == "1.0.0")
        .unwrap();
    assert_eq!(record.reference_count, 1);
    assert_eq!(registry.tarball_request_count(), 1);
}

#[test]
fn doctor_reports_ndjson_and_shell_support_have_stable_contracts() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("fixture-pkg", "1.0.0", "ok\n"));
    registry.add_package(fixture("jsm", "1.0.0", "doctor probe\n"));
    let project = tempfile::tempdir().unwrap();
    install_one(&registry, project.path());
    let registry_url = format!("{}/meta", registry.base_url());

    let report = cli(project.path())
        .args(["--json", "--registry", &registry_url, "doctor", "--report"])
        .env(
            "HTTPS_PROXY",
            "http://alice:do-not-leak@example.invalid:8123",
        )
        .output()
        .unwrap();
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report_value: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report_value["schema"], "jsm.v1.doctor");
    assert!(!report_value.to_string().contains("do-not-leak"));
    assert!(
        report_value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|check| check["status"].is_string()
                && check["explanation"].is_string()
                && check["remedy"].is_string())
    );
    let checks = report_value["checks"].as_array().unwrap();
    let check_names = checks
        .iter()
        .map(|check| check["name"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    for name in [
        "store-integrity",
        "project-references",
        "link-capabilities",
        "volume-layout",
        "long-paths",
        "case-sensitivity",
        "file-locking",
        "proxy-ca",
        "registry-reachability",
        "clock",
        "disk-space",
        "node-version",
        "jsm-version",
        "windows-defender-dev-drive",
    ] {
        assert!(check_names.contains(name), "doctor omitted {name}");
    }
    let registry_check = checks
        .iter()
        .find(|check| check["name"] == "registry-reachability")
        .unwrap();
    assert_eq!(registry_check["status"], "ok");
    let unavailable = cli(project.path())
        .args([
            "--json",
            "--registry",
            "http://127.0.0.1:1/meta",
            "doctor",
            "--report",
        ])
        .output()
        .unwrap();
    assert!(unavailable.status.success());
    let unavailable: Value = serde_json::from_slice(&unavailable.stdout).unwrap();
    let registry_check = unavailable["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "registry-reachability")
        .unwrap();
    assert_eq!(registry_check["status"], "warning");

    let ndjson = cli(project.path())
        .args([
            "--json",
            "--progress=ndjson",
            "--offline",
            "install",
            "--frozen-lockfile",
        ])
        .output()
        .unwrap();
    assert!(
        ndjson.status.success(),
        "{}",
        String::from_utf8_lossy(&ndjson.stderr)
    );
    let events = String::from_utf8(ndjson.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.len() >= 2);
    assert!(events.iter().all(|event| {
        event["schema"]
            .as_str()
            .is_some_and(|schema| schema.starts_with("jsm.v1."))
    }));
    assert!(
        events
            .iter()
            .any(|event| event["schema"] == "jsm.v1.progress")
    );
    assert!(
        events
            .iter()
            .any(|event| event["schema"] == "jsm.v1.install")
    );

    let package_completions = cli(project.path())
        .args(["__complete", "packages", "fixture-pkg"])
        .output()
        .unwrap();
    assert!(package_completions.status.success());
    assert!(String::from_utf8_lossy(&package_completions.stdout).contains("fixture-pkg@1.0.0"));
    let project_prefix = project.path().to_string_lossy().into_owned();
    let project_completions = cli(project.path())
        .args(["__complete", "projects", &project_prefix])
        .output()
        .unwrap();
    assert!(project_completions.status.success());
    assert!(String::from_utf8_lossy(&project_completions.stdout).contains(&project_prefix));

    for shell in ["bash", "zsh", "fish", "power-shell", "elvish"] {
        let completion = cli(project.path())
            .args(["completion", shell])
            .output()
            .unwrap();
        assert!(
            completion.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&completion.stderr)
        );
        assert!(
            !completion.stdout.is_empty(),
            "{shell} completion script was empty"
        );
        assert!(
            String::from_utf8_lossy(&completion.stdout).contains("__complete"),
            "{shell} lacks its dynamic completion hook"
        );
    }
    let help = cli(project.path())
        .args(["help", "store"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("forget-project"));
}

#[test]
fn phase2_schema_is_valid_and_covers_all_stable_envelopes() {
    use std::collections::BTreeSet;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schemas/phase2-cli-output.schema.json");
    let schema: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    let definitions = schema["$defs"].as_object().unwrap();
    let mut ids = BTreeSet::new();
    for reference in schema["oneOf"].as_array().unwrap() {
        let name = reference["$ref"]
            .as_str()
            .unwrap()
            .strip_prefix("#/$defs/")
            .unwrap();
        let definition = definitions
            .get(name)
            .unwrap_or_else(|| panic!("unresolved schema reference {name}"));
        let id = definition["allOf"][1]["properties"]["schema"]["const"]
            .as_str()
            .or_else(|| definition["properties"]["schema"]["const"].as_str())
            .unwrap_or_else(|| panic!("schema definition {name} has no stable schema ID"));
        ids.insert(id.to_owned());
    }
    let expected = [
        "jsm.v1.store.path",
        "jsm.v1.store.status",
        "jsm.v1.store.list",
        "jsm.v1.store.versions",
        "jsm.v1.store.info",
        "jsm.v1.store.usage",
        "jsm.v1.store.add",
        "jsm.v1.store.remove",
        "jsm.v1.store.forget-project",
        "jsm.v1.store.prune",
        "jsm.v1.store.gc",
        "jsm.v1.store.pin",
        "jsm.v1.store.unpin",
        "jsm.v1.store.pinned",
        "jsm.v1.store.verify",
        "jsm.v1.versions",
        "jsm.v1.doctor",
        "jsm.v1.lock.verify",
        "jsm.v1.lock.merge",
        "jsm.v1.lock.install-merge-driver",
        "jsm.v1.progress",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(ids, expected);
}
