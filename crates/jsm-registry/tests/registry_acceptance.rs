use std::{io::Read, time::Duration};

use jsm_registry::{Auth, CacheEntry, Registry, RegistryConfig, RegistryError};
use jsm_testkit::{FakeRegistry, FixturePackage};

fn fixture(name: &str) -> jsm_testkit::FixtureArtifact {
    FixturePackage::new(name, "1.0.0").unwrap().build().unwrap()
}

fn config(registry: &FakeRegistry) -> RegistryConfig {
    RegistryConfig {
        default_registry: format!("{}/meta", registry.base_url()),
        retries: 0,
        ..RegistryConfig::default()
    }
}

#[test]
fn conditional_metadata_revalidation_uses_cached_body_on_304() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("revalidate-pkg"));
    registry.set_metadata_validators(
        Some("\"v1\"".into()),
        Some("Wed, 01 Jan 2020 00:00:00 GMT".into()),
    );
    let client = Registry::new(config(&registry)).unwrap();

    let first = client.packument("revalidate-pkg").unwrap();
    let second = client.packument("revalidate-pkg").unwrap();

    assert_eq!(
        first.versions.keys().collect::<Vec<_>>(),
        second.versions.keys().collect::<Vec<_>>()
    );
    assert_eq!(registry.request_count(), 2);
    assert_eq!(
        client.cached("revalidate-pkg").unwrap().etag.as_deref(),
        Some("\"v1\"")
    );
}

#[test]
fn abbreviated_metadata_406_falls_back_to_full_packument() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("full-packument-pkg"));
    registry.set_error_status_sequence([Some(406), None]);
    let client = Registry::new(config(&registry)).unwrap();

    let packument = client.packument("full-packument-pkg").unwrap();

    assert!(packument.versions.contains_key("1.0.0"));
    assert_eq!(registry.request_count(), 2);
}

#[test]
fn five_xx_retries_are_bounded_and_eventually_succeed() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("retry-pkg"));
    registry.set_error_status_sequence([Some(503), Some(503), None]);
    let mut cfg = config(&registry);
    cfg.retries = 2;
    let client = Registry::new(cfg).unwrap();

    assert!(client.packument("retry-pkg").is_ok());
    assert_eq!(registry.request_count(), 3);

    registry.set_error_status(Some(503));
    assert!(matches!(
        client.packument("retry-pkg"),
        Err(RegistryError::Http(503))
    ));
    assert_eq!(registry.request_count(), 6);
}

#[test]
fn request_timeout_is_reported_without_retrying_when_disabled() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("slow-pkg"));
    registry.set_latency(Duration::from_secs(1));
    let mut cfg = config(&registry);
    cfg.timeout = Duration::from_millis(250);
    cfg.request_timeout = Some(Duration::from_secs(2));
    let client = Registry::new(cfg).unwrap();

    assert!(matches!(
        client.packument("slow-pkg"),
        Err(RegistryError::Timeout)
    ));
    assert_eq!(registry.request_count(), 1);
}

#[test]
fn authentication_failures_do_not_leak_credentials_and_valid_bearer_succeeds() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("private-pkg"));
    registry.set_required_bearer_token(Some("expected-secret".into()));
    let mut cfg = config(&registry);
    cfg.default_auth = Some(Auth::Bearer("wrong-secret".into()));
    let client = Registry::new(cfg).unwrap();

    assert!(matches!(
        client.packument("private-pkg"),
        Err(RegistryError::Http(401))
    ));
    assert!(!format!("{client:?}").contains("wrong-secret"));
    assert!(!format!("{:?}", Auth::Bearer("wrong-secret".into())).contains("wrong-secret"));

    let mut cfg = config(&registry);
    cfg.default_auth = Some(Auth::Bearer("expected-secret".into()));
    assert!(Registry::new(cfg).unwrap().packument("private-pkg").is_ok());
}

#[test]
fn tarball_url_userinfo_is_rejected_without_network_access() {
    let registry = FakeRegistry::start().unwrap();
    let client = Registry::new(config(&registry)).unwrap();
    let host = registry.base_url().trim_start_matches("http://");
    let url = format!("http://embedded:secret@{host}/tarball/pkg.tgz");

    assert!(matches!(
        client.download_tarball("safe-pkg", &url),
        Err(RegistryError::InvalidRegistry)
    ));
    assert_eq!(registry.request_count(), 0);
}

#[test]
fn offline_cache_hit_and_miss_are_distinct() {
    let registry = FakeRegistry::start().unwrap();
    let mut cfg = config(&registry);
    cfg.offline = true;
    let client = Registry::new(cfg).unwrap();
    client.insert_cache(
        "cached-pkg",
        CacheEntry {
            body: r#"{"name":"cached-pkg","dist-tags":{"latest":"1.0.0"}}"#.into(),
            etag: Some("\"cached\"".into()),
            last_modified: None,
        },
    );

    assert!(client.packument("cached-pkg").is_ok());
    assert!(matches!(
        client.packument("missing-pkg"),
        Err(RegistryError::OfflineMiss)
    ));
    assert_eq!(registry.request_count(), 0);
}

#[test]
fn truncated_tarball_is_resumed_with_range_and_reconstructed_exactly() {
    let registry = FakeRegistry::start().unwrap();
    let artifact = fixture("resume-pkg");
    let expected = artifact.tarball.clone();
    registry.add_package(artifact);
    registry.set_range_support(true);
    registry.set_truncation(Some(37));
    let mut cfg = config(&registry);
    cfg.retries = 1;
    let client = Registry::new(cfg).unwrap();
    let metadata = client.packument("resume-pkg").unwrap();
    let url = metadata.versions["1.0.0"].dist.tarball.as_ref().unwrap();

    let mut response = client
        .download_tarball_resumable("resume-pkg", url)
        .unwrap();
    let mut actual = Vec::new();
    response.read_to_end(&mut actual).unwrap();

    assert_eq!(actual, expected);
    assert_eq!(registry.request_count(), 3);
}

#[test]
fn invalid_resume_range_is_rejected_instead_of_silently_corrupting_data() {
    let registry = FakeRegistry::start().unwrap();
    registry.add_package(fixture("bad-resume-pkg"));
    registry.set_range_support(true);
    registry.set_invalid_range(true);
    registry.set_truncation(Some(37));
    let mut cfg = config(&registry);
    cfg.retries = 1;
    let client = Registry::new(cfg).unwrap();
    let metadata = client.packument("bad-resume-pkg").unwrap();
    let url = metadata.versions["1.0.0"].dist.tarball.as_ref().unwrap();
    let mut response = client
        .download_tarball_resumable("bad-resume-pkg", url)
        .unwrap();

    let mut actual = Vec::new();
    assert!(response.read_to_end(&mut actual).is_err());
}
