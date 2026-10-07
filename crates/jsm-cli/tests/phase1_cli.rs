use std::collections::BTreeSet;
use std::fs;

use assert_cmd::Command;
use jsm_testkit::{FakeRegistry, FixturePackage};
use serde_json::Value;

fn cli(cwd: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("jsm").unwrap();
    command
        .arg("--cwd")
        .arg(cwd)
        .env("JSM_LOG", "error")
        .env("CI", "0")
        .env_remove("JSM_CI");
    command
}

fn assert_schema_value(schema: &Value, value: &Value, path: &str) {
    if let Some(expected) = schema.get("const") {
        assert_eq!(value, expected, "{path} must match its schema const");
    }
    if let Some(types) = schema.get("type") {
        let types = types
            .as_array()
            .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_else(|| {
                vec![
                    types
                        .as_str()
                        .expect("schema type must be a string or array"),
                ]
            });
        let matches = types.iter().any(|kind| match *kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "null" => value.is_null(),
            other => panic!("unsupported test schema type `{other}` at {path}"),
        });
        assert!(matches, "{path} has the wrong JSON type: {value}");
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_i64) {
        let number = value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
            .expect("minimum applies to an integer");
        assert!(number >= minimum, "{path} is below its schema minimum");
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            assert!(value.get(key).is_some(), "{path}.{key} is required");
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            let allowed = properties.keys().collect::<BTreeSet<_>>();
            for key in value
                .as_object()
                .expect("schema object must be represented by JSON object")
                .keys()
            {
                assert!(allowed.contains(key), "unexpected property {path}.{key}");
            }
        }
        for (key, child_schema) in properties {
            if let Some(child) = value.get(key) {
                assert_schema_value(child_schema, child, &format!("{path}.{key}"));
            }
        }
    }
    if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
        for (index, child) in values.iter().enumerate() {
            assert_schema_value(items, child, &format!("{path}[{index}]"));
        }
    }
}

fn assert_cli_json_contract(bytes: &[u8], command: &str) -> Value {
    let value: Value = serde_json::from_slice(bytes).expect("command output must be JSON");
    let schema: Value = serde_json::from_str(include_str!(
        "../../../docs/schemas/phase1-cli-output.schema.json"
    ))
    .expect("Phase 1 CLI schema must be valid JSON");
    let variants = schema["oneOf"]
        .as_array()
        .expect("schema must define oneOf");
    let variant = variants
        .iter()
        .find(|variant| variant["properties"]["command"]["const"] == command)
        .unwrap_or_else(|| panic!("no CLI output schema for {command}"));
    assert_schema_value(variant, &value, command);

    let snapshots: Value =
        serde_json::from_str(include_str!("fixtures/phase1-json-envelope.snap.json"))
            .expect("JSON envelope snapshots must be valid JSON");
    let expected = snapshots[command]
        .as_array()
        .unwrap_or_else(|| panic!("no JSON envelope snapshot for {command}"))
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let actual = value
        .as_object()
        .expect("JSON output must be an object")
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "{command} JSON envelope snapshot changed");
    value
}

#[test]
fn phase_one_help_exposes_the_complete_command_surface() {
    let output = Command::cargo_bin("jsm")
        .unwrap()
        .arg("--help")
        .env("JSM_LOG", "error")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout)
        .replace("\r\n", "\n")
        .replace("jsm.exe", "jsm");
    let normalized_help = help
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        normalized_help + "\n",
        include_str!("fixtures/phase1-help.txt").replace("\r\n", "\n")
    );
    for command in [
        "init", "add", "install", "remove", "run", "exec", "list", "why", "config",
    ] {
        assert!(
            help.contains(command),
            "missing `{command}` in help: {help}"
        );
    }
    for flag in [
        "--cwd",
        "--json",
        "--quiet",
        "--registry",
        "--store-dir",
        "--offline",
        "--prefer-offline",
        "--non-interactive",
    ] {
        assert!(
            help.contains(flag),
            "missing global flag `{flag}` in help: {help}"
        );
    }
}

#[test]
fn install_alias_and_add_save_flags_are_available() {
    let install = Command::cargo_bin("jsm")
        .unwrap()
        .args(["i", "--help"])
        .env("JSM_LOG", "error")
        .output()
        .unwrap();
    assert!(install.status.success());
    let install_help = String::from_utf8_lossy(&install.stdout);
    assert!(install_help.contains("--frozen-lockfile"));
    assert!(install_help.contains("--prefer-offline"));

    for alias in ["up", "rm"] {
        let output = Command::cargo_bin("jsm")
            .unwrap()
            .args([alias, "--help"])
            .env("JSM_LOG", "error")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "alias `{alias}` was not accepted: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let add = Command::cargo_bin("jsm")
        .unwrap()
        .args(["add", "--help"])
        .env("JSM_LOG", "error")
        .output()
        .unwrap();
    assert!(add.status.success());
    let add_help = String::from_utf8_lossy(&add.stdout);
    assert!(add_help.contains("--save-exact"));
    assert!(add_help.contains("--save-prefix"));

    for (command, argument) in [
        ("init", "--yes"),
        ("add", "[PACKAGES]..."),
        ("install", "--frozen-lockfile"),
        ("remove", "[PACKAGES]..."),
        ("run", "<SCRIPT>"),
        ("exec", "<COMMAND>"),
        ("why", "<PACKAGE>"),
    ] {
        let output = Command::cargo_bin("jsm")
            .unwrap()
            .args([command, "--help"])
            .env("JSM_LOG", "error")
            .output()
            .unwrap();
        assert!(output.status.success(), "{command} --help failed");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(argument),
            "{command} help omits {argument}"
        );
    }

    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"alias-project","version":"1.0.0"}"#,
    )
    .unwrap();
    let list = cli(project.path()).args(["--json", "ls"]).output().unwrap();
    assert!(list.status.success());
    assert_cli_json_contract(&list.stdout, "list");
}

#[test]
fn unknown_command_suggests_a_similar_baseline_command() {
    let output = Command::cargo_bin("jsm")
        .unwrap()
        .arg("instal")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("similar subcommands"), "{stderr}");
    assert!(stderr.contains("install"), "{stderr}");
}

#[test]
fn init_yes_and_json_list_and_why_are_project_scoped() {
    let project = tempfile::tempdir().unwrap();

    let init = cli(project.path())
        .arg("--json")
        .arg("init")
        .arg("--yes")
        .output()
        .unwrap();
    assert!(init.status.success());
    let init_json = assert_cli_json_contract(&init.stdout, "init");
    assert_eq!(init_json["schema"], "jsm.v1.init");
    assert!(project.path().join("package.json").is_file());

    fs::write(
        project.path().join("package.json"),
        r#"{
  "name": "phase1-project",
  "version": "1.0.0",
  "dependencies": {"fixture-pkg": "^1.2.3"},
  "devDependencies": {"dev-only": "~2.0.0"}
}
"#,
    )
    .unwrap();

    let list = cli(project.path())
        .arg("--json")
        .arg("list")
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_json = assert_cli_json_contract(&list.stdout, "list");
    assert_eq!(list_json["schema"], "jsm.v1.list");
    assert_eq!(list_json["command"], "list");
    assert_eq!(list_json["packages"].as_array().unwrap().len(), 2);
    assert_eq!(list_json["packages"][0]["name"], "fixture-pkg");
    assert_eq!(list_json["packages"][0]["type"], "dependencies");

    let why = cli(project.path())
        .arg("--json")
        .arg("why")
        .arg("fixture-pkg")
        .output()
        .unwrap();
    assert!(
        why.status.success(),
        "{}",
        String::from_utf8_lossy(&why.stderr)
    );
    let why_json = assert_cli_json_contract(&why.stdout, "why");
    assert_eq!(why_json["schema"], "jsm.v1.why");
    assert_eq!(why_json["package"], "fixture-pkg");
    assert_eq!(why_json["direct"], true);
}

#[test]
fn install_uses_only_the_fake_registry_writes_lockfile_and_disables_scripts() {
    let registry = FakeRegistry::start().unwrap();
    let marker_project = tempfile::tempdir().unwrap();
    let marker = marker_project.path().join("postinstall-ran");
    let fixture = FixturePackage::new("fixture-pkg", "1.2.3")
        .unwrap()
        .file("lib/main.js", b"module.exports = 42;\n".to_vec())
        .unwrap()
        .script("postinstall", format!("touch {}", marker.display()))
        .engine("node", ">=20")
        .os("linux")
        .cpu("x64")
        .libc("glibc")
        .build()
        .unwrap();
    registry.add_package(fixture);

    fs::write(
        marker_project.path().join("package.json"),
        r#"{
  "name": "phase1-project",
  "version": "1.0.0",
  "dependencies": {"fixture-pkg": "^1.2.3"}
}
"#,
    )
    .unwrap();

    let output = cli(marker_project.path())
        .arg("--json")
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("jsm:"));

    let install_json = assert_cli_json_contract(&output.stdout, "install");
    assert_eq!(install_json["schema"], "jsm.v1.install");
    let package_dir = marker_project.path().join("node_modules/fixture-pkg");
    assert!(package_dir.join("package.json").is_file());
    assert_eq!(
        fs::read_to_string(package_dir.join("lib/main.js")).unwrap(),
        "module.exports = 42;\n"
    );
    assert!(marker_project.path().join("jsm.lock").is_file());
    let lock = jsm_lockfile::Lockfile::read(marker_project.path().join("jsm.lock")).unwrap();
    let package_key = &lock.importers["."].resolved_dependencies["fixture-pkg"];
    let locked_package = &lock.packages[package_key];
    assert_eq!(locked_package.engines["node"], ">=20");
    assert_eq!(locked_package.os, ["linux"]);
    assert_eq!(locked_package.cpu, ["x64"]);
    assert_eq!(locked_package.libc, ["glibc"]);
    assert!(locked_package.has_scripts);
    assert!(
        !marker.exists(),
        "fixture lifecycle script unexpectedly executed"
    );

    let node = std::process::Command::new("node")
        .current_dir(marker_project.path())
        .args([
            "-e",
            "if (require('fixture-pkg/lib/main.js') !== 42) process.exit(1)",
        ])
        .output()
        .unwrap();
    assert!(
        node.status.success(),
        "{}",
        String::from_utf8_lossy(&node.stderr)
    );

    cli(marker_project.path())
        .arg("--offline")
        .arg("install")
        .arg("--frozen-lockfile")
        .assert()
        .success();

    let manifest_path = marker_project.path().join("package.json");
    let original_manifest = fs::read_to_string(&manifest_path).unwrap();
    fs::write(
        &manifest_path,
        original_manifest.replace("^1.2.3", "^2.0.0"),
    )
    .unwrap();
    cli(marker_project.path())
        .arg("--offline")
        .arg("install")
        .arg("--frozen-lockfile")
        .assert()
        .code(6);
    fs::write(&manifest_path, &original_manifest).unwrap();

    let removed = cli(marker_project.path())
        .arg("--json")
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("remove")
        .arg("fixture-pkg")
        .output()
        .unwrap();
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let remove_json = assert_cli_json_contract(&removed.stdout, "remove");
    assert_eq!(remove_json["schema"], "jsm.v1.remove");
    assert!(
        !marker_project
            .path()
            .join("node_modules/fixture-pkg")
            .exists()
    );
    assert!(
        !fs::read_to_string(marker_project.path().join("jsm.lock"))
            .unwrap()
            .contains("fixture-pkg@1.2.3")
    );
    let after_remove = fs::read_to_string(&manifest_path).unwrap();
    assert_eq!(
        after_remove,
        original_manifest.replace("{\"fixture-pkg\": \"^1.2.3\"}", "{}")
    );
}

#[test]
fn install_uses_plain_progress_fallback_when_stderr_is_not_a_tty() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"empty-project","version":"1.0.0"}"#,
    )
    .unwrap();
    let store = project.path().join(".test-store");
    let output = cli(project.path())
        .args(["--store-dir"])
        .arg(&store)
        .args(["install", "--no-lockfile"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("jsm: Resolving, fetching, and linking dependencies"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("installed 0 packages"));

    let quiet = cli(project.path())
        .args(["--store-dir"])
        .arg(&store)
        .args(["--quiet", "install", "--no-lockfile"])
        .output()
        .unwrap();
    assert!(quiet.status.success());
    assert!(!String::from_utf8_lossy(&quiet.stderr).contains("jsm:"));
}

#[test]
fn install_failure_paths_never_publish_partial_project_state() {
    let registry = FakeRegistry::start().unwrap();
    let registry_url = format!("{}/meta", registry.base_url());

    let missing = tempfile::tempdir().unwrap();
    fs::write(
        missing.path().join("package.json"),
        r#"{"name":"missing-project","version":"1.0.0","dependencies":{"missing-pkg":"^1.0.0"}}"#,
    )
    .unwrap();
    let result = cli(missing.path())
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!missing.path().join("jsm.lock").exists());
    assert!(!missing.path().join("node_modules").exists());

    let mut corrupted = FixturePackage::new("corrupt-pkg", "1.0.0")
        .unwrap()
        .build()
        .unwrap();
    // Change a gzip timestamp byte: decompressed tar contents stay valid, but SRI no longer matches.
    corrupted.tarball[4] ^= 1;
    registry.add_package(corrupted);
    let integrity_failure = tempfile::tempdir().unwrap();
    fs::write(
        integrity_failure.path().join("package.json"),
        r#"{"name":"integrity-project","version":"1.0.0","dependencies":{"corrupt-pkg":"1.0.0"}}"#,
    )
    .unwrap();
    let result = cli(integrity_failure.path())
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!integrity_failure.path().join("jsm.lock").exists());
    assert!(
        !integrity_failure
            .path()
            .join("node_modules/corrupt-pkg")
            .exists()
    );

    let network_failure = tempfile::tempdir().unwrap();
    fs::write(
        network_failure.path().join("package.json"),
        r#"{"name":"network-project","version":"1.0.0","dependencies":{"corrupt-pkg":"1.0.0"}}"#,
    )
    .unwrap();
    registry.set_error_status(Some(503));
    let result = cli(network_failure.path())
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!network_failure.path().join("jsm.lock").exists());
    assert!(!network_failure.path().join("node_modules").exists());
}

#[test]
fn add_and_no_lockfile_install_work_with_the_fake_registry() {
    let registry = FakeRegistry::start().unwrap();
    let fixture = FixturePackage::new("fixture-pkg", "1.2.3")
        .unwrap()
        .build()
        .unwrap();
    registry.add_package(fixture);
    let registry_url = format!("{}/meta", registry.base_url());

    let add_project = tempfile::tempdir().unwrap();
    cli(add_project.path())
        .arg("init")
        .arg("--yes")
        .assert()
        .success();
    let original_manifest = "{\n  \"name\": \"add-project\",\n  \"version\": \"1.0.0\",\n  \"scripts\": {\n    \"test\": \"node test.js\"\n  }\n}\n";
    fs::write(add_project.path().join("package.json"), original_manifest).unwrap();
    let add = cli(add_project.path())
        .arg("--json")
        .arg("--registry")
        .arg(&registry_url)
        .arg("add")
        .arg("--save-prefix")
        .arg("~")
        .arg("fixture-pkg@1.2.3")
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let add_json = assert_cli_json_contract(&add.stdout, "add");
    assert_eq!(add_json["schema"], "jsm.v1.add");
    assert_eq!(
        fs::read_to_string(add_project.path().join("package.json")).unwrap(),
        "{\n  \"name\": \"add-project\",\n  \"version\": \"1.0.0\",\n  \"scripts\": {\n    \"test\": \"node test.js\"\n  },\n  \"dependencies\": {\n    \"fixture-pkg\": \"~1.2.3\"\n  }\n}\n"
    );
    assert!(add_project.path().join("jsm.lock").is_file());
    assert!(
        add_project
            .path()
            .join("node_modules/fixture-pkg/package.json")
            .is_file()
    );

    let exact_project = tempfile::tempdir().unwrap();
    fs::write(
        exact_project.path().join("package.json"),
        "{\"name\":\"exact-project\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();
    let exact = cli(exact_project.path())
        .arg("--registry")
        .arg(&registry_url)
        .arg("add")
        .arg("--save-exact")
        .arg("fixture-pkg@1.2.3")
        .output()
        .unwrap();
    assert!(
        exact.status.success(),
        "{}",
        String::from_utf8_lossy(&exact.stderr)
    );
    let exact_manifest: Value = serde_json::from_str(
        &fs::read_to_string(exact_project.path().join("package.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(exact_manifest["dependencies"]["fixture-pkg"], "1.2.3");

    let no_lock = tempfile::tempdir().unwrap();
    fs::write(
        no_lock.path().join("package.json"),
        r#"{"name":"no-lock-project","version":"1.0.0","dependencies":{"fixture-pkg":"1.2.3"}}"#,
    )
    .unwrap();
    let install = cli(no_lock.path())
        .arg("--json")
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .arg("--no-lockfile")
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let install_json = assert_cli_json_contract(&install.stdout, "install");
    assert_eq!(install_json["schema"], "jsm.v1.install");
    assert!(!no_lock.path().join("jsm.lock").exists());
    assert!(
        no_lock
            .path()
            .join("node_modules/fixture-pkg/package.json")
            .is_file()
    );
}

#[test]
fn missing_optional_root_dependency_does_not_block_install() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("available-pkg", "1.0.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"optional-project","version":"1.0.0","dependencies":{"available-pkg":"1.0.0"},"optionalDependencies":{"missing-optional":"^1.0.0"}}"#,
    )
    .unwrap();

    let output = cli(project.path())
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        project
            .path()
            .join("node_modules/available-pkg/package.json")
            .is_file()
    );
    assert!(
        !project
            .path()
            .join("node_modules/missing-optional")
            .exists()
    );
    let lock = jsm_lockfile::Lockfile::read(project.path().join("jsm.lock")).unwrap();
    assert!(
        !lock.importers["."]
            .resolved_optional_dependencies
            .contains_key("missing-optional")
    );
}

#[test]
fn npm_optional_dependency_overrides_duplicate_required_dependency() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("optional-override-pkg", "1.0.0")
            .unwrap()
            .dependency("unpublished-platform-binary", "^1.0.0")
            .unwrap()
            .optional_dependency("unpublished-platform-binary", "^1.0.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"optional-override-project","version":"1.0.0","dependencies":{"optional-override-pkg":"1.0.0"}}"#,
    )
    .unwrap();

    let output = cli(project.path())
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        project
            .path()
            .join("node_modules/optional-override-pkg/package.json")
            .is_file()
    );
    assert!(
        !project
            .path()
            .join("node_modules/unpublished-platform-binary")
            .exists()
    );
    let lock = jsm_lockfile::Lockfile::read(project.path().join("jsm.lock")).unwrap();
    let package_key = &lock.importers["."].resolved_dependencies["optional-override-pkg"];
    let package = &lock.packages[package_key];
    assert!(
        !package
            .dependencies
            .contains_key("unpublished-platform-binary")
    );
    assert!(
        !package
            .optional_dependencies
            .contains_key("unpublished-platform-binary")
    );
}

#[test]
fn install_prefers_non_deprecated_version_but_keeps_constraint_semantics() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("deprecated-fixture", "1.9.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    registry.add_package(
        FixturePackage::new("deprecated-fixture", "2.0.0")
            .unwrap()
            .deprecated("use the maintained release")
            .build()
            .unwrap(),
    );
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"deprecated-project","version":"1.0.0","dependencies":{"deprecated-fixture":"*"}}"#,
    )
    .unwrap();

    let output = cli(project.path())
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock = jsm_lockfile::Lockfile::read(project.path().join("jsm.lock")).unwrap();
    let selected = &lock.importers["."].resolved_dependencies["deprecated-fixture"];
    assert_eq!(lock.packages[selected].version, "1.9.0");
}

#[test]
fn optional_peer_dependency_is_not_automatically_installed() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("optional-peer-consumer", "1.0.0")
            .unwrap()
            .optional_peer_dependency("available-peer", "^1.0.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    registry.add_package(
        FixturePackage::new("available-peer", "1.4.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"optional-peer-project","version":"1.0.0","dependencies":{"optional-peer-consumer":"1.0.0"}}"#,
    )
    .unwrap();

    let output = cli(project.path())
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        project
            .path()
            .join("node_modules/optional-peer-consumer/package.json")
            .is_file()
    );
    let lock = jsm_lockfile::Lockfile::read(project.path().join("jsm.lock")).unwrap();
    assert!(
        !lock
            .packages
            .values()
            .any(|package| package.name == "available-peer")
    );
    assert!(!project.path().join("node_modules/available-peer").exists());
}

#[test]
fn invalid_environment_configuration_reports_the_key() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"invalid-config-project","version":"1.0.0"}"#,
    )
    .unwrap();

    let output = cli(project.path())
        .env("JSM_PREFER_OFFLINE", "sometimes")
        .arg("install")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("JSM_PREFER_OFFLINE"), "{diagnostic}");
    assert!(
        diagnostic.contains("expected true or false"),
        "{diagnostic}"
    );
}

#[test]
fn nested_versions_are_locked_and_resolved_from_each_parent_package() {
    let registry = FakeRegistry::start().unwrap();
    let shared_v1 = FixturePackage::new("shared", "1.8.0")
        .unwrap()
        .file("index.js", b"module.exports = 'one';\n".to_vec())
        .unwrap()
        .build()
        .unwrap();
    let shared_v2 = FixturePackage::new("shared", "2.4.0")
        .unwrap()
        .file("index.js", b"module.exports = 'two';\n".to_vec())
        .unwrap()
        .build()
        .unwrap();
    let left = FixturePackage::new("left", "1.0.0")
        .unwrap()
        .dependency("shared", "^1.0.0")
        .unwrap()
        .file(
            "index.js",
            b"module.exports = require('shared');\n".to_vec(),
        )
        .unwrap()
        .build()
        .unwrap();
    let right = FixturePackage::new("right", "1.0.0")
        .unwrap()
        .dependency("shared", "^2.0.0")
        .unwrap()
        .file(
            "index.js",
            b"module.exports = require('shared');\n".to_vec(),
        )
        .unwrap()
        .build()
        .unwrap();
    for fixture in [shared_v1, shared_v2, left, right] {
        registry.add_package(fixture);
    }

    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"nested-versions","version":"1.0.0","dependencies":{"left":"^1.0.0","right":"^1.0.0"}}"#,
    )
    .unwrap();
    let output = cli(project.path())
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lock = jsm_lockfile::Lockfile::read(project.path().join("jsm.lock")).unwrap();
    let left_key = &lock.importers["."].resolved_dependencies["left"];
    let right_key = &lock.importers["."].resolved_dependencies["right"];
    let left_shared_key = &lock.packages[left_key].dependencies["shared"];
    let right_shared_key = &lock.packages[right_key].dependencies["shared"];
    assert_eq!(lock.packages[left_shared_key].version, "1.8.0");
    assert_eq!(lock.packages[right_shared_key].version, "2.4.0");
    let node = std::process::Command::new("node")
        .current_dir(project.path())
        .args([
            "-e",
            "const left=require('left'); const right=require('right'); let rootVisible=true; try { require('shared'); } catch (_) { rootVisible=false; } if (left !== 'one' || right !== 'two' || rootVisible) process.exit(1)",
        ])
        .output()
        .unwrap();
    assert!(
        node.status.success(),
        "{}",
        String::from_utf8_lossy(&node.stderr)
    );

    let frozen = cli(project.path())
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("install")
        .arg("--frozen-lockfile")
        .output()
        .unwrap();
    assert!(
        frozen.status.success(),
        "repeated frozen install failed: {}",
        String::from_utf8_lossy(&frozen.stderr)
    );
}

#[test]
fn run_and_exec_launch_project_commands_and_forward_exit_codes() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"commands-test","version":"1.0.0","scripts":{"run-bin":"exit-code"}}"#,
    )
    .unwrap();
    let bin_dir = project.path().join("node_modules/.bin");
    fs::create_dir_all(&bin_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = bin_dir.join("exit-code");
        fs::write(&path, "#!/bin/sh\nexit 19\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    {
        fs::write(bin_dir.join("exit-code.ps1"), "exit 19\r\n").unwrap();
        fs::write(bin_dir.join("exit-code.cmd"), "@exit /b 19\r\n").unwrap();
    }

    cli(project.path())
        .arg("run")
        .arg("run-bin")
        .assert()
        .code(19);
    cli(project.path())
        .arg("exec")
        .arg("exit-code")
        .assert()
        .code(19);

    let output = cli(project.path())
        .arg("--json")
        .arg("run")
        .arg("run-bin")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(19));
    let response = assert_cli_json_contract(&output.stdout, "run");
    assert_eq!(response["schema"], "jsm.v1.run");
    assert_eq!(response["exitCode"], 19);

    let output = cli(project.path())
        .arg("--json")
        .arg("exec")
        .arg("exit-code")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(19));
    let response = assert_cli_json_contract(&output.stdout, "exec");
    assert_eq!(response["schema"], "jsm.v1.exec");
    assert_eq!(response["exitCode"], 19);
}

#[test]
fn exec_runs_a_registry_installed_package_bin_and_forwards_arguments() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("fixture-bin", "1.0.0")
            .unwrap()
            .file(
                "bin/tool.js",
                b"#!/usr/bin/env node\nconsole.log(process.argv.slice(2).join('|'));\n".to_vec(),
            )
            .unwrap()
            .bin("fixture-tool", "bin/tool.js")
            .unwrap()
            .build()
            .unwrap(),
    );
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"bin-integration","version":"1.0.0","dependencies":{"fixture-bin":"1.0.0"}}"#,
    )
    .unwrap();

    let install = cli(project.path())
        .arg("--store-dir")
        .arg(project.path().join(".phase1-bin-store"))
        .arg("--registry")
        .arg(format!("{}/meta", registry.base_url()))
        .arg("install")
        .arg("--no-lockfile")
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(String::from_utf8_lossy(&install.stderr).contains("jsm: Fetching fixture-bin@1.0.0"));
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;

        let sidecar = project
            .path()
            .join("node_modules/.bin/fixture-tool.jsm-bin.json");
        assert!(sidecar.is_file(), "generated bin target sidecar is missing");
        let encoded_target: Vec<u16> =
            serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        let target = std::path::PathBuf::from(std::ffi::OsString::from_wide(&encoded_target));
        assert!(
            target.is_absolute(),
            "generated bin target is not absolute: {target:?}"
        );
        assert!(
            target.ends_with(std::path::Path::new("bin/tool.js")),
            "generated bin target is unexpected: {target:?}"
        );
    }
    let output = cli(project.path())
        .arg("exec")
        .arg("fixture-tool")
        .arg("alpha")
        .arg("beta")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "package bin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "alpha|beta");
}

#[test]
fn add_dependency_type_flags_update_the_requested_manifest_fields() {
    let registry = FakeRegistry::start().unwrap();
    for name in ["prod-dep", "dev-dep", "optional-dep", "peer-dep"] {
        registry.add_package(FixturePackage::new(name, "1.2.3").unwrap().build().unwrap());
    }
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"dependency-types","version":"1.0.0"}
"#,
    )
    .unwrap();
    let registry_url = format!("{}/meta", registry.base_url());

    for (flag, package) in [
        ("", "prod-dep"),
        ("-D", "dev-dep"),
        ("-O", "optional-dep"),
        ("-P", "peer-dep"),
    ] {
        let mut command = cli(project.path());
        command.arg("--registry").arg(&registry_url).arg("add");
        if !flag.is_empty() {
            command.arg(flag);
        }
        let output = command.arg(format!("{package}@1.2.3")).output().unwrap();
        assert!(
            output.status.success(),
            "add {flag} {package} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let manifest: Value =
        serde_json::from_slice(&fs::read(project.path().join("package.json")).unwrap()).unwrap();
    assert_eq!(manifest["dependencies"]["prod-dep"], "^1.2.3");
    assert_eq!(manifest["devDependencies"]["dev-dep"], "^1.2.3");
    assert_eq!(manifest["optionalDependencies"]["optional-dep"], "^1.2.3");
    assert_eq!(manifest["peerDependencies"]["peer-dep"], "^1.2.3");
}

#[test]
fn config_project_scope_supports_typed_values_and_redacts_credentials() {
    let project = tempfile::tempdir().unwrap();
    let set = cli(project.path())
        .args(["config", "set", "registry", "https://project.invalid"])
        .output()
        .unwrap();
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    cli(project.path())
        .args(["config", "set", "@acme:registry", "https://scope.invalid"])
        .assert()
        .success();
    cli(project.path())
        .args(["config", "set", "auth-token", "super-secret"])
        .assert()
        .success();
    let listed = cli(project.path())
        .args(["--json", "config", "list"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    let json: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(json["values"]["registry"], "https://project.invalid");
    assert_eq!(json["values"]["@acme:registry"], "https://scope.invalid");
    assert_eq!(json["values"]["auth"], "<redacted>");
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("super-secret"));
    let raw = fs::read_to_string(project.path().join("jsm.toml")).unwrap();
    assert!(raw.contains("super-secret"));
    cli(project.path())
        .args(["config", "delete", "@acme:registry", "--project"])
        .assert()
        .success();
    let project_list = cli(project.path())
        .args(["--json", "config", "list", "--project"])
        .output()
        .unwrap();
    let project_values: Value = serde_json::from_slice(&project_list.stdout).unwrap();
    assert!(project_values["values"].get("@acme:registry").is_none());
    cli(project.path())
        .args(["config", "list", "--project", "--global"])
        .assert()
        .code(2);
    let got = cli(project.path())
        .args(["config", "get", "auth-token"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&got.stdout).trim(), "<redacted>");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(project.path().join("jsm.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let home = tempfile::tempdir().unwrap();
    let global = cli(project.path())
        .env("HOME", home.path())
        .args([
            "config",
            "set",
            "registry",
            "https://global.invalid",
            "--global",
        ])
        .output()
        .unwrap();
    assert!(
        global.status.success(),
        "{}",
        String::from_utf8_lossy(&global.stderr)
    );
    let global_config = fs::read_to_string(home.path().join("jsm.toml")).unwrap();
    assert!(global_config.contains("https://global.invalid"));
    let global_list = cli(project.path())
        .env("HOME", home.path())
        .args(["--json", "config", "list", "--global"])
        .output()
        .unwrap();
    let global_values: Value = serde_json::from_slice(&global_list.stdout).unwrap();
    assert_eq!(
        global_values["values"]["registry"],
        "https://global.invalid"
    );
    cli(project.path())
        .env("HOME", home.path())
        .args(["config", "delete", "registry", "--global"])
        .assert()
        .success();
    assert!(
        !fs::read_to_string(home.path().join("jsm.toml"))
            .unwrap()
            .contains("https://global.invalid")
    );
}

#[test]
fn workspace_root_registry_config_applies_to_member_projects() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("workspace-dep", "1.0.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    let workspace = tempfile::tempdir().unwrap();
    let member = workspace.path().join("packages/member");
    fs::create_dir_all(&member).unwrap();
    fs::write(
        workspace.path().join("package.json"),
        r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join(".npmrc"),
        format!("registry={}/meta\n", registry.base_url()),
    )
    .unwrap();
    fs::write(
        member.join("package.json"),
        r#"{"name":"member","version":"1.0.0","dependencies":{"workspace-dep":"^1.0.0"}}"#,
    )
    .unwrap();

    let output = cli(&member).arg("install").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        member
            .join("node_modules/workspace-dep/package.json")
            .is_file()
    );
}

#[test]
fn explicit_config_file_overrides_project_npmrc() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("custom-config-dep", "1.0.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    let project = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let explicit = config_dir.path().join(".npmrc");
    fs::write(
        project.path().join(".npmrc"),
        "registry=http://127.0.0.1:1/meta\n",
    )
    .unwrap();
    fs::write(
        &explicit,
        format!("registry={}/meta\n", registry.base_url()),
    )
    .unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"explicit-config","version":"1.0.0","dependencies":{"custom-config-dep":"^1.0.0"}}"#,
    )
    .unwrap();

    let output = cli(project.path())
        .arg("--config")
        .arg(&explicit)
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        project
            .path()
            .join("node_modules/custom-config-dep/package.json")
            .is_file()
    );
}

#[test]
fn registry_precedence_is_cli_then_environment_project_workspace_and_user() {
    use std::path::Path;

    let user_registry = FakeRegistry::start().unwrap();
    let workspace_registry = FakeRegistry::start().unwrap();
    let project_registry = FakeRegistry::start().unwrap();
    let environment_registry = FakeRegistry::start().unwrap();
    let cli_registry = FakeRegistry::start().unwrap();
    for registry in [
        &user_registry,
        &workspace_registry,
        &project_registry,
        &environment_registry,
        &cli_registry,
    ] {
        registry.add_package(
            FixturePackage::new("precedence-dep", "1.0.0")
                .unwrap()
                .build()
                .unwrap(),
        );
    }
    let registry_url = |registry: &FakeRegistry| format!("{}/meta", registry.base_url());

    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("jsm.toml"),
        format!("registry = \"{}\"\n", registry_url(&user_registry)),
    )
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(
        workspace.path().join("package.json"),
        r#"{"name":"workspace-root","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("jsm.toml"),
        format!("registry = \"{}\"\n", registry_url(&workspace_registry)),
    )
    .unwrap();
    let project = workspace.path().join("packages/project");
    let workspace_only = workspace.path().join("packages/workspace-only");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&workspace_only).unwrap();
    let package_json = r#"{"name":"precedence-project","version":"1.0.0","dependencies":{"precedence-dep":"^1.0.0"}}"#;
    fs::write(project.join("package.json"), package_json).unwrap();
    fs::write(workspace_only.join("package.json"), package_json).unwrap();
    fs::write(
        project.join(".npmrc"),
        format!("registry={}\n", registry_url(&project_registry)),
    )
    .unwrap();

    let install = |cwd: &Path, env_registry: Option<&str>, cli_url: Option<&str>| {
        let mut command = cli(cwd);
        command
            .env("HOME", home.path())
            .env("CI", "0")
            .env_remove("JSM_CI")
            .env_remove("JSM_REGISTRY")
            .env_remove("NPM_CONFIG_REGISTRY")
            .env_remove("npm_config_registry");
        if let Some(url) = env_registry {
            command.env("JSM_REGISTRY", url);
        }
        if let Some(url) = cli_url {
            command.arg("--registry").arg(url);
        }
        command
            .args(["install", "--no-lockfile"])
            .assert()
            .success();
    };

    install(
        &project,
        Some(&registry_url(&environment_registry)),
        Some(&registry_url(&cli_registry)),
    );
    assert!(cli_registry.request_count() > 0);
    assert_eq!(environment_registry.request_count(), 0);
    assert_eq!(project_registry.request_count(), 0);
    assert_eq!(workspace_registry.request_count(), 0);
    assert_eq!(user_registry.request_count(), 0);

    install(&project, Some(&registry_url(&environment_registry)), None);
    assert!(environment_registry.request_count() > 0);
    assert_eq!(project_registry.request_count(), 0);
    assert_eq!(workspace_registry.request_count(), 0);
    assert_eq!(user_registry.request_count(), 0);

    install(&project, None, None);
    assert!(project_registry.request_count() > 0);
    assert_eq!(workspace_registry.request_count(), 0);
    assert_eq!(user_registry.request_count(), 0);

    install(&workspace_only, None, None);
    assert!(workspace_registry.request_count() > 0);
    assert_eq!(user_registry.request_count(), 0);

    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("package.json"), package_json).unwrap();
    install(outside.path(), None, None);
    assert!(user_registry.request_count() > 0);
}

#[test]
fn install_uses_cli_environment_and_project_store_directory_overrides() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("store-path-dep", "1.0.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    let registry_url = format!("{}/meta", registry.base_url());
    let manifest = r#"{"name":"store-path-test","version":"1.0.0","dependencies":{"store-path-dep":"^1.0.0"}}"#;

    let cli_project = tempfile::tempdir().unwrap();
    fs::write(cli_project.path().join("package.json"), manifest).unwrap();
    let cli_store = cli_project.path().join("explicit-store");
    let shadow_store = cli_project.path().join("environment-shadow");
    let output = cli(cli_project.path())
        .env("JSM_STORE_DIR", &shadow_store)
        .arg("--store-dir")
        .arg(&cli_store)
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI store override failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(cli_store.join("VERSION").is_file());
    assert!(
        !shadow_store.exists(),
        "CLI store path must override the environment"
    );

    let env_project = tempfile::tempdir().unwrap();
    fs::write(env_project.path().join("package.json"), manifest).unwrap();
    let env_store = env_project.path().join("environment-store");
    let output = cli(env_project.path())
        .env("JSM_STORE_DIR", &env_store)
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "environment store override failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(env_store.join("VERSION").is_file());

    let config_project = tempfile::tempdir().unwrap();
    fs::write(config_project.path().join("package.json"), manifest).unwrap();
    fs::write(
        config_project.path().join("jsm.toml"),
        "store-dir = \"./configured-store\"\n",
    )
    .unwrap();
    let output = cli(config_project.path())
        .env("JSM_STORE_DIR", "")
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "project-config store override failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        config_project
            .path()
            .join("configured-store/VERSION")
            .is_file()
    );
}

#[test]
fn ci_mode_automatically_requires_a_current_lockfile() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("ci-frozen-dep", "1.0.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("package.json"),
        r#"{"name":"ci-frozen-test","version":"1.0.0","dependencies":{"ci-frozen-dep":"^1.0.0"}}"#,
    )
    .unwrap();
    let registry_url = format!("{}/meta", registry.base_url());

    let missing = cli(project.path())
        .env("CI", "1")
        .env("JSM_CI", "")
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .output()
        .unwrap();
    assert!(
        !missing.status.success(),
        "CI install without a lockfile must fail"
    );
    assert_eq!(missing.status.code(), Some(6));
    assert!(!project.path().join("jsm.lock").exists());
    assert!(!project.path().join("node_modules/ci-frozen-dep").exists());
    assert_eq!(
        registry.request_count(),
        0,
        "frozen CI failure must not resolve online"
    );

    cli(project.path())
        .env("CI", "1")
        .env("JSM_CI", "")
        .arg("--registry")
        .arg(&registry_url)
        .args(["install", "--frozen-lockfile=auto"])
        .assert()
        .code(6);
    assert_eq!(registry.request_count(), 0);

    cli(project.path())
        .env("CI", "0")
        .env_remove("JSM_CI")
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .assert()
        .success();
    cli(project.path())
        .env("CI", "true")
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .assert()
        .success();

    let manifest_path = project.path().join("package.json");
    let original_manifest = fs::read_to_string(&manifest_path).unwrap();
    fs::write(
        &manifest_path,
        original_manifest.replace("^1.0.0", "^2.0.0"),
    )
    .unwrap();
    let lock_before = fs::read(project.path().join("jsm.lock")).unwrap();
    let requests_before_stale_lock = registry.request_count();
    cli(project.path())
        .env("CI", "true")
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .assert()
        .code(6);
    assert_eq!(
        fs::read(project.path().join("jsm.lock")).unwrap(),
        lock_before
    );
    assert_eq!(
        registry.request_count(),
        requests_before_stale_lock,
        "frozen stale-lock failure must not resolve online"
    );

    fs::write(&manifest_path, &original_manifest).unwrap();
    fs::remove_file(project.path().join("jsm.lock")).unwrap();
    cli(project.path())
        .env("CI", "true")
        .arg("--registry")
        .arg(&registry_url)
        .args(["install", "--frozen-lockfile=never"])
        .assert()
        .success();
    assert!(project.path().join("jsm.lock").is_file());
}

#[cfg(unix)]
fn assert_signal_cancels_install_without_committing_partial_project_state(
    signal_name: &str,
    signal_label: &str,
) {
    use std::{
        process::{Command as ProcessCommand, Stdio},
        thread,
        time::{Duration, Instant},
    };

    let registry = FakeRegistry::start().unwrap();
    registry.add_package(
        FixturePackage::new("slow-dep", "1.0.0")
            .unwrap()
            .build()
            .unwrap(),
    );
    registry.set_latency(Duration::from_secs(2));
    let project = tempfile::tempdir().unwrap();
    let manifest =
        r#"{"name":"cancel-test","version":"1.0.0","dependencies":{"slow-dep":"^1.0.0"}}"#;
    fs::write(project.path().join("package.json"), manifest).unwrap();
    let registry_url = format!("{}/meta", registry.base_url());
    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_jsm"))
        .arg("--cwd")
        .arg(project.path())
        .arg("--registry")
        .arg(&registry_url)
        .arg("install")
        .env("JSM_LOG", "error")
        .env("CI", "0")
        .env_remove("JSM_CI")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let request_deadline = Instant::now() + Duration::from_secs(15);
    while registry.request_count() == 0 && Instant::now() < request_deadline {
        assert!(
            child.try_wait().unwrap().is_none(),
            "CLI exited before starting the registry request"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        registry.request_count() > 0,
        "CLI did not reach the slow registry"
    );
    let signal = ProcessCommand::new("kill")
        .arg(format!("-{signal_name}"))
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(signal.success(), "failed to send {signal_label} to the CLI");

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("CLI did not stop after SIGTERM");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(130));
    assert_eq!(
        fs::read_to_string(project.path().join("package.json")).unwrap(),
        manifest
    );
    assert!(!project.path().join("jsm.lock").exists());
    assert!(!project.path().join("node_modules").exists());
}

#[cfg(unix)]
#[test]
fn sigterm_cancels_install_without_committing_partial_project_state() {
    assert_signal_cancels_install_without_committing_partial_project_state("TERM", "SIGTERM");
}

#[cfg(unix)]
#[test]
fn sigint_cancels_install_without_committing_partial_project_state() {
    assert_signal_cancels_install_without_committing_partial_project_state("INT", "SIGINT");
}
