use std::{
    collections::HashMap,
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
    packages: HashMap<String, (String, FixtureArtifact)>,
    artifacts_by_id: HashMap<String, FixtureArtifact>,
    behavior: RegistryBehavior,
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
                    let (status, mut body, content_type, behavior) = {
                        let state = worker_state.lock().expect("registry state lock");
                        let behavior = state.behavior.clone();
                        let auth_failed = behavior.require_bearer_token.as_deref().is_some_and(|token| {
                            let expected = format!("Bearer {token}");
                            !request.headers().iter().any(|header| {
                                header.field.equiv("Authorization")
                                    && header.value.as_str() == expected.as_str()
                            })
                        });
                        if auth_failed {
                            (401, b"authentication required".to_vec(), "text/plain", behavior)
                        } else if let Some(status) = behavior.error_status {
                            (status, format!("{{\"error\":\"controlled status {status}\"}}").into_bytes(), "application/json", behavior)
                        } else if let Some(name) = request.url().strip_prefix("/meta/") {
                            match state.packages.get(name) {
                                Some((id, artifact)) => {
                                    let version = artifact.package_id.version().to_string();
                                    let name = artifact.package_id.name().as_str();
                                    let metadata = serde_json::json!({
                                        "name": name,
                                        "versions": {
                                            version.clone(): {
                                                "name": name,
                                                "version": version,
                                                "dist": {
                                                    "tarball": format!("{}/tarballs/{id}.tgz", worker_base_url),
                                                    "integrity": artifact.integrity.to_string()
                                                }
                                            }
                                        },
                                        "dist-tags": { "latest": artifact.package_id.version().to_string() }
                                    });
                                    (200, serde_json::to_vec(&metadata).expect("serialize fixture metadata"), "application/json", behavior)
                                }
                                None => (404, b"package not found".to_vec(), "text/plain", behavior),
                            }
                        } else if let Some(id) = request.url().strip_prefix("/tarballs/").and_then(|value| value.strip_suffix(".tgz")) {
                            match state.artifacts_by_id.get(id) {
                                Some(artifact) => (200, artifact.tarball.clone(), "application/octet-stream", behavior),
                                None => (404, b"tarball not found".to_vec(), "text/plain", behavior),
                            }
                        } else {
                            (404, b"not found".to_vec(), "text/plain", behavior)
                        }
                    };
                    if !behavior.latency.is_zero() {
                        thread::sleep(behavior.latency);
                    }
                    if let Some(rate) = behavior.bandwidth_bytes_per_second.filter(|rate| *rate > 0) {
                        let seconds = body.len() as f64 / rate as f64;
                        thread::sleep(Duration::from_secs_f64(seconds.min(60.0)));
                    }
                    if is_tarball && let Some(limit) = behavior.truncate_payload_at {
                        body.truncate(limit);
                    }
                    let header = Header::from_bytes("Content-Type", content_type)
                        .expect("static HTTP content-type header");
                    let response = Response::from_data(body)
                        .with_status_code(StatusCode(status))
                        .with_header(header);
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
        let id = format!(
            "fixture-{}",
            self.state
                .lock()
                .expect("registry state lock")
                .artifacts_by_id
                .len()
                + 1
        );
        let mut state = self.state.lock().expect("registry state lock");
        state.artifacts_by_id.insert(id.clone(), artifact.clone());
        state.packages.insert(name, (id, artifact));
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

    pub fn set_required_bearer_token(&self, token: Option<String>) {
        self.state
            .lock()
            .expect("registry state lock")
            .behavior
            .require_bearer_token = token;
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
}

impl Drop for FakeRegistry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
