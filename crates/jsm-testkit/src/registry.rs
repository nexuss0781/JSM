use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::FixtureArtifact;
use tiny_http::{Header, Response, Server, StatusCode};

/// Request behavior controls for deterministic failure and network-shape tests.
#[derive(Debug, Clone, Default)]
pub struct RegistryBehavior {
    pub latency: Duration,
    pub error_status: Option<u16>,
    pub truncate_payload_at: Option<usize>,
    pub bandwidth_bytes_per_second: Option<u64>,
    pub require_bearer_token: Option<String>,
}

#[derive(Default)]
struct RegistryState {
    packages: HashMap<String, BTreeMap<String, (String, FixtureArtifact)>>,
    artifacts_by_id: HashMap<String, FixtureArtifact>,
    behavior: RegistryBehavior,
    requests: usize,
    tarball_requests: usize,
    metadata_etag: Option<String>,
    metadata_last_modified: Option<String>,
    require_authorization: Option<String>,
    retry_after: Option<String>,
    error_status_sequence: VecDeque<Option<u16>>,
    supports_ranges: bool,
    invalid_range: bool,
}

/// Hermetic in-process HTTP registry. It binds only to loopback and starts no external service.
pub struct FakeRegistry {
    base_url: String,
    state: Arc<Mutex<RegistryState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeRegistry {
    pub fn start() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let server = Server::http("127.0.0.1:0")?;
        let address = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| io::Error::other("fake registry was not assigned an IP address"))?;
        let base_url = format!("http://{address}");
        let state = Arc::new(Mutex::new(RegistryState::default()));
        let worker_state = Arc::clone(&state);
        let worker_base_url = base_url.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("jsm-fake-registry".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let request = match server.recv_timeout(Duration::from_millis(25)) {
                        Ok(Some(request)) => request,
                        Ok(None) => continue,
                        Err(_) => continue,
                    };
                    let is_tarball = request.url().starts_with("/tarballs/");
                    let (mut status, mut body, content_type, behavior, supports_ranges, invalid_range) = {
                        let mut state = worker_state.lock().expect("registry state lock");
                        state.requests += 1;
                        state.tarball_requests += usize::from(is_tarball);
                        let behavior = state.behavior.clone();
                        let supports_ranges = state.supports_ranges;
                        let invalid_range = state.invalid_range;
                        let authorization = request.headers().iter().find(|header| {
                            header.field.equiv("Authorization")
                        }).map(|header| header.value.as_str());
                        let expected_bearer = behavior
                            .require_bearer_token
                            .as_deref()
                            .map(|token| format!("Bearer {token}"));
                        let auth_failed = expected_bearer
                            .as_deref()
                            .is_some_and(|expected| authorization != Some(expected))
                            || state
                                .require_authorization
                                .as_deref()
                                .is_some_and(|expected| authorization != Some(expected));
                        if auth_failed {
                            (401, b"authentication required".to_vec(), "text/plain", behavior, supports_ranges, invalid_range)
                        } else if let Some(status) = state
                            .error_status_sequence
                            .pop_front()
                            .flatten()
                            .or(behavior.error_status)
                        {
                            (status, format!("{{\"error\":\"controlled status {status}\"}}").into_bytes(), "application/json", behavior, supports_ranges, invalid_range)
                        } else if let Some(name) = request.url().strip_prefix("/meta/") {
                            let etag_matches = state.metadata_etag.as_deref().is_some_and(|etag| {
                                request.headers().iter().any(|header| {
                                    header.field.equiv("If-None-Match") && header.value.as_str() == etag
                                })
                            });
                            let modified_matches = state.metadata_last_modified.as_deref().is_some_and(|modified| {
                                request.headers().iter().any(|header| {
                                    header.field.equiv("If-Modified-Since") && header.value.as_str() == modified
                                })
                            });
                            if etag_matches || modified_matches {
                                (304, Vec::new(), "application/json", behavior, supports_ranges, invalid_range)
                            } else {
                            match state.packages.get(name) {
                                Some(versions) if !versions.is_empty() => {
                                    let mut version_metadata = serde_json::Map::new();
                                    for (version, (id, artifact)) in versions {
                                        let mut entry = artifact
                                            .package_json
                                            .as_object()
                                            .cloned()
                                            .unwrap_or_default();
                                        entry.insert(
                                            "name".into(),
                                            serde_json::json!(artifact.package_id.name().as_str()),
                                        );
                                        entry.insert("version".into(), serde_json::json!(version));
                                        entry.insert(
                                            "dist".into(),
                                            serde_json::json!({
                                                "tarball": format!("{}/tarballs/{id}.tgz", worker_base_url),
                                                "integrity": artifact.integrity.to_string()
                                            }),
                                        );
                                        version_metadata.insert(
                                            version.clone(),
                                            serde_json::Value::Object(entry),
                                        );
                                    }
                                    let latest = versions
                                        .values()
                                        .max_by(|(_, left), (_, right)| {
                                            left.package_id.version().cmp(right.package_id.version())
                                        })
                                        .map(|(_, artifact)| artifact.package_id.version().to_string())
                                        .expect("non-empty fake package versions");
                                    let package_name = versions
                                        .values()
                                        .next()
                                        .expect("non-empty fake package versions")
                                        .1
                                        .package_id
                                        .name()
                                        .as_str();
                                    let metadata = serde_json::json!({
                                        "name": package_name,
                                        "versions": version_metadata,
                                        "dist-tags": { "latest": latest }
                                    });
                                    (200, serde_json::to_vec(&metadata).expect("serialize fixture metadata"), "application/json", behavior, supports_ranges, invalid_range)
                                }
                                None => (404, b"package not found".to_vec(), "text/plain", behavior, supports_ranges, invalid_range),
                                Some(_) => (404, b"package not found".to_vec(), "text/plain", behavior, supports_ranges, invalid_range),
                            }
                            }
                        } else if let Some(id) = request.url().strip_prefix("/tarballs/").and_then(|value| value.strip_suffix(".tgz")) {
                            match state.artifacts_by_id.get(id) {
                                Some(artifact) => (200, artifact.tarball.clone(), "application/octet-stream", behavior, supports_ranges, invalid_range),
                                None => (404, b"tarball not found".to_vec(), "text/plain", behavior, supports_ranges, invalid_range),
                            }
                        } else {
                            (404, b"not found".to_vec(), "text/plain", behavior, supports_ranges, invalid_range)
                        }
                    };
                    if !behavior.latency.is_zero() {
                        thread::sleep(behavior.latency);
                    }
                    if let Some(rate) = behavior.bandwidth_bytes_per_second.filter(|rate| *rate > 0) {
                        let seconds = body.len() as f64 / rate as f64;
                        thread::sleep(Duration::from_secs_f64(seconds.min(60.0)));
                    }
                    let range_start = if is_tarball {
                        request
                            .headers()
                            .iter()
                            .find(|header| header.field.equiv("Range"))
                            .and_then(|header| header.value.as_str().strip_prefix("bytes="))
                            .and_then(|value| value.strip_suffix("-"))
                            .and_then(|value| value.parse::<usize>().ok())
                    } else {
                        None
                    };
                    let truncated = is_tarball && range_start.is_none() && behavior.truncate_payload_at.is_some();
                    let mut range_header = None;
                    if is_tarball && supports_ranges && let Some(start) = range_start {
                        if start >= body.len() {
                            status = 416;
                            body.clear();
                        } else {
                            let total = body.len();
                            let actual_start = if invalid_range { start.saturating_add(1) } else { start };
                            body = body.get(actual_start..).unwrap_or_default().to_vec();
                            status = 206;
                            range_header = Some(format!("bytes {actual_start}-{}/{}", total - 1, total));
                        }
                    }
                    if truncated && let Some(limit) = behavior.truncate_payload_at {
                        body.truncate(limit);
                    }
                    let header = Header::from_bytes("Content-Type", content_type)
                        .expect("static HTTP content-type header");
                    let mut response = Response::from_data(body)
                        .with_status_code(StatusCode(status))
                        .with_header(header);
                    if truncated {
                        response = response.with_chunked_threshold(0);
                    }
                    if let Some(value) = range_header {
                        response = response.with_header(
                            Header::from_bytes("Content-Range", value).expect("range header"),
                        );
                    }
                    let response_state = worker_state.lock().expect("registry state lock");
                    if let Some(etag) = response_state.metadata_etag.as_deref() && !is_tarball {
                        response = response.with_header(Header::from_bytes("ETag", etag).expect("etag header"));
                    }
                    if let Some(modified) = response_state.metadata_last_modified.as_deref() && !is_tarball {
                        response = response.with_header(Header::from_bytes("Last-Modified", modified).expect("last-modified header"));
                    }
                    if let Some(retry_after) = response_state.retry_after.as_deref() && !(200..300).contains(&status) {
                        response = response.with_header(Header::from_bytes("Retry-After", retry_after).expect("retry-after header"));
                    }
                    let _ = request.respond(response);
                }
            })?;

        Ok(Self {
            base_url,
            state,
            stop,
            worker: Some(worker),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn add_package(&self, artifact: FixtureArtifact) {
        let name = artifact.package_id.name().to_string();
        let version = artifact.package_id.version().to_string();
        let mut state = self.state.lock().expect("registry state lock");
        let id = format!("fixture-{}", state.artifacts_by_id.len() + 1);
        state.artifacts_by_id.insert(id.clone(), artifact.clone());
        state
            .packages
            .entry(name)
            .or_default()
            .insert(version, (id, artifact));
    }

    pub fn set_behavior(&self, behavior: RegistryBehavior) {
        self.state.lock().expect("registry state lock").behavior = behavior;
    }

    pub fn set_latency(&self, latency: Duration) {
        self.state
            .lock()
            .expect("registry state lock")
            .behavior
            .latency = latency;
    }

    pub fn set_error_status(&self, status: Option<u16>) {
        self.state
            .lock()
            .expect("registry state lock")
            .behavior
            .error_status = status;
    }

    /// Set a per-request status sequence; `None` entries serve the normal response.
    pub fn set_error_status_sequence(&self, statuses: impl IntoIterator<Item = Option<u16>>) {
        self.state
            .lock()
            .expect("registry state lock")
            .error_status_sequence = statuses.into_iter().collect();
    }

    pub fn set_required_bearer_token(&self, token: Option<String>) {
        self.state
            .lock()
            .expect("registry state lock")
            .behavior
            .require_bearer_token = token;
    }

    pub fn set_range_support(&self, enabled: bool) {
        self.state
            .lock()
            .expect("registry state lock")
            .supports_ranges = enabled;
    }

    pub fn set_invalid_range(&self, enabled: bool) {
        self.state
            .lock()
            .expect("registry state lock")
            .invalid_range = enabled;
    }

    pub fn set_truncation(&self, byte_limit: Option<usize>) {
        self.state
            .lock()
            .expect("registry state lock")
            .behavior
            .truncate_payload_at = byte_limit;
    }

    pub fn set_bandwidth_limit(&self, bytes_per_second: Option<u64>) {
        self.state
            .lock()
            .expect("registry state lock")
            .behavior
            .bandwidth_bytes_per_second = bytes_per_second;
    }

    pub fn set_metadata_validators(&self, etag: Option<String>, last_modified: Option<String>) {
        let mut state = self.state.lock().expect("registry state lock");
        state.metadata_etag = etag;
        state.metadata_last_modified = last_modified;
    }

    pub fn set_required_authorization(&self, authorization: Option<String>) {
        self.state
            .lock()
            .expect("registry state lock")
            .require_authorization = authorization;
    }

    pub fn set_retry_after(&self, retry_after: Option<String>) {
        self.state.lock().expect("registry state lock").retry_after = retry_after;
    }

    pub fn request_count(&self) -> usize {
        self.state.lock().expect("registry state lock").requests
    }

    pub fn tarball_request_count(&self) -> usize {
        self.state
            .lock()
            .expect("registry state lock")
            .tarball_requests
    }
}

impl Drop for FakeRegistry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixturePackage;

    #[test]
    fn metadata_validators_return_not_modified() {
        let registry = FakeRegistry::start().unwrap();
        registry.add_package(
            FixturePackage::new("cache-pkg", "1.0.0")
                .unwrap()
                .build()
                .unwrap(),
        );
        registry.set_metadata_validators(
            Some("\"v1\"".into()),
            Some("Wed, 01 Jan 2020 00:00:00 GMT".into()),
        );
        let url = format!("{}/meta/cache-pkg", registry.base_url());
        let first = ureq::get(&url).call().unwrap();
        assert_eq!(first.status(), 200);
        assert!(first.header("ETag").is_some());
        let second = ureq::get(&url).set("If-None-Match", "\"v1\"").call();
        match second {
            Ok(response) => assert_eq!(response.status(), 304),
            Err(ureq::Error::Status(status, _)) => assert_eq!(status, 304),
            Err(error) => panic!("unexpected conditional response: {error:?}"),
        }
    }

    #[test]
    fn authorization_is_required_only_when_configured() {
        let registry = FakeRegistry::start().unwrap();
        registry.add_package(
            FixturePackage::new("auth-pkg", "1.0.0")
                .unwrap()
                .build()
                .unwrap(),
        );
        registry.set_required_authorization(Some("Bearer scoped-secret".into()));
        let url = format!("{}/meta/auth-pkg", registry.base_url());
        assert!(matches!(
            ureq::get(&url).call(),
            Err(ureq::Error::Status(401, _))
        ));
        assert_eq!(
            ureq::get(&url)
                .set("Authorization", "Bearer scoped-secret")
                .call()
                .unwrap()
                .status(),
            200
        );
    }
}
