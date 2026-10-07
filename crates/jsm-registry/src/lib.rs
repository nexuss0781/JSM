//! Resilient npm-compatible registry metadata access.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use jsm_core::PackageName;
use reqwest::blocking::{Client, ClientBuilder, Response};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

pub const CRATE_NAME: &str = "jsm-registry";
const ABBREVIATED: &str = "application/vnd.npm.install-v1+json";
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Packument {
    pub name: Option<String>,
    #[serde(default)]
    pub versions: HashMap<String, PackageMetadata>,
    #[serde(rename = "dist-tags", default)]
    pub dist_tags: HashMap<String, String>,
    #[serde(default)]
    pub time: HashMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageMetadata {
    pub name: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
    #[serde(rename = "optionalDependencies", default)]
    pub optional_dependencies: HashMap<String, String>,
    #[serde(rename = "peerDependencies", default)]
    pub peer_dependencies: HashMap<String, String>,
    #[serde(rename = "peerDependenciesMeta", default)]
    pub peer_dependencies_meta: HashMap<String, PeerDependencyMeta>,
    #[serde(default, deserialize_with = "deserialize_engine_map")]
    pub engines: HashMap<String, String>,
    #[serde(default)]
    pub os: Vec<String>,
    #[serde(default)]
    pub cpu: Vec<String>,
    #[serde(default)]
    pub libc: Vec<String>,
    #[serde(default)]
    pub scripts: HashMap<String, String>,
    #[serde(default)]
    pub deprecated: Option<String>,
    #[serde(default)]
    pub dist: Dist,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerDependencyMeta {
    #[serde(default)]
    pub optional: bool,
}
fn deserialize_engine_map<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<HashMap<String, String>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default())
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Dist {
    pub tarball: Option<String>,
    pub shasum: Option<String>,
    pub integrity: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheEntry {
    pub body: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}
#[derive(Clone)]
pub enum Auth {
    Bearer(String),
    Basic { username: String, password: String },
}
impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Bearer([REDACTED])"),
            Self::Basic { .. } => f.write_str("Basic([REDACTED])"),
        }
    }
}
impl Auth {
    fn valid(&self) -> bool {
        match self {
            Self::Bearer(t) => !t.is_empty() && !t.chars().any(char::is_control),
            Self::Basic { username, password } => {
                !username.is_empty()
                    && !username.chars().any(char::is_control)
                    && !password.chars().any(char::is_control)
            }
        }
    }
}
#[derive(Debug, Clone)]
pub struct RegistryConfig {
    pub default_registry: String,
    pub scoped_registries: HashMap<String, String>,
    pub default_auth: Option<Auth>,
    pub scoped_auth: HashMap<String, Auth>,
    pub registry_auth: HashMap<String, Auth>,
    pub timeout: Duration,
    pub retries: usize,
    pub offline: bool,
    pub prefer_offline: bool,
    pub cache_dir: Option<PathBuf>,
    pub request_timeout: Option<Duration>,
    pub total_timeout: Option<Duration>,
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub custom_ca_bundle: Option<PathBuf>,
    pub strict_ssl: bool,
}
impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            default_registry: "https://registry.npmjs.org".into(),
            scoped_registries: HashMap::new(),
            default_auth: None,
            scoped_auth: HashMap::new(),
            registry_auth: HashMap::new(),
            timeout: Duration::from_secs(30),
            retries: 2,
            offline: false,
            prefer_offline: false,
            cache_dir: default_cache_dir(),
            request_timeout: None,
            total_timeout: None,
            proxy: None,
            no_proxy: None,
            custom_ca_bundle: None,
            strict_ssl: true,
        }
    }
}
fn default_cache_dir() -> Option<PathBuf> {
    std::env::var_os("JSM_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(|p| PathBuf::from(p).join("jsm/registry"))
                .or_else(|| {
                    std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache/jsm/registry"))
                })
        })
}
impl RegistryConfig {
    pub fn registry_for(&self, name: &str) -> &str {
        name.split('/')
            .next()
            .filter(|s| s.starts_with('@'))
            .and_then(|s| self.scoped_registries.get(s))
            .map(String::as_str)
            .unwrap_or(&self.default_registry)
    }
    fn auth_for(&self, name: &str, registry: &str) -> Option<&Auth> {
        name.split('/')
            .next()
            .filter(|s| s.starts_with('@'))
            .and_then(|s| self.scoped_auth.get(s))
            .or_else(|| {
                self.registry_auth
                    .iter()
                    .filter(|(k, _)| registry_auth_matches(registry, k))
                    .max_by_key(|(k, _)| k.len())
                    .map(|(_, a)| a)
            })
            .or(self.default_auth.as_ref())
    }
}
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("invalid package name")]
    InvalidPackage,
    #[error("invalid registry URL")]
    InvalidRegistry,
    #[error("invalid registry authentication configuration")]
    InvalidAuth,
    #[error("offline and package metadata is not cached")]
    OfflineMiss,
    #[error("registry returned HTTP status {0}")]
    Http(u16),
    #[error("registry request failed")]
    Transport,
    #[error("registry response body was invalid JSON")]
    InvalidJson,
    #[error("registry response body was invalid JSON: {0}")]
    InvalidJsonBody(String),
    #[error("invalid registry package metadata: {0}")]
    InvalidMetadata(String),
    #[error("registry response was empty or truncated")]
    Truncated,
    #[error("registry request timed out")]
    Timeout,
    #[error("cached metadata is invalid")]
    InvalidCache,
    #[error("invalid custom CA bundle")]
    InvalidCa,
}
/// Registry backend boundary for sparse indexes, mirrors, and local stores.
pub trait RegistryBackend {
    fn packument(&self, name: &str) -> Result<Packument, RegistryError>;
}
/// Concrete npm registry implementation.
pub type RegistryClient = Registry;
#[derive(Clone)]
pub struct Registry {
    config: RegistryConfig,
    client: Client,
    cache: Arc<Mutex<HashMap<String, CacheEntry>>>,
}
impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}
/// A streaming tarball body which resumes only after a body read failure.
pub struct ResumableTarball {
    registry: Registry,
    name: String,
    url: String,
    response: Option<Response>,
    bytes_read: u64,
    total: Option<u64>,
    retries_left: usize,
}

impl ResumableTarball {
    fn new(registry: Registry, name: String, url: String, response: Response) -> Self {
        let total = response.content_length();
        let retries_left = registry.config.retries;
        Self {
            registry,
            name,
            url,
            response: Some(response),
            bytes_read: 0,
            total,
            retries_left,
        }
    }

    fn resume(&mut self, cause: io::Error) -> io::Result<()> {
        if self.retries_left == 0 {
            return Err(cause);
        }
        self.retries_left -= 1;
        let range = format!("bytes={}-", self.bytes_read);
        let auth = self.registry.auth_for_tarball(&self.name, &self.url);
        let mut request = self
            .registry
            .client
            .get(&self.url)
            .header(reqwest::header::RANGE, &range);
        if let Some(a) = auth {
            request = apply_auth(request, a);
        }
        let response = request
            .send()
            .map_err(|error| io::Error::other(format!("resuming tarball: {error}")))?;
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server did not honor HTTP Range",
            ));
        }
        let content_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "resumed response omitted Content-Range",
                )
            })?;
        let (start, end, total) = parse_content_range(content_range)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Range"))?;
        if start != self.bytes_read
            || end < start
            || total <= end
            || self.total.is_some_and(|known| known != total)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid resumed Content-Range",
            ));
        }
        self.total = Some(total);
        self.response = Some(response);
        Ok(())
    }
}

impl Read for ResumableTarball {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            let response = self.response.as_mut().expect("resumable response missing");
            match response.read(buffer) {
                Ok(count) if count > 0 => {
                    self.bytes_read = self.bytes_read.saturating_add(count as u64);
                    if self.total.is_some_and(|total| self.bytes_read > total) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "response exceeded Content-Length",
                        ));
                    }
                    return Ok(count);
                }
                Ok(_) => {
                    if self.total.is_some_and(|total| self.bytes_read >= total) {
                        return Ok(0);
                    }
                    let cause = io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "tarball response ended early",
                    );
                    self.response = None;
                    self.resume(cause)?;
                }
                Err(error) => {
                    self.response = None;
                    self.resume(error)?;
                }
            }
        }
    }
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split_whitespace();
    if parts.next()? != "bytes" {
        return None;
    }
    let mut range_total = parts.next()?.split('/');
    let range = range_total.next()?;
    let total = range_total.next()?.parse().ok()?;
    if range_total.next().is_some() {
        return None;
    }
    let mut bounds = range.split('-');
    Some((
        bounds.next()?.parse().ok()?,
        bounds.next()?.parse().ok()?,
        total,
    ))
}

impl Registry {
    pub fn new(config: RegistryConfig) -> Result<Self, RegistryError> {
        for u in std::iter::once(&config.default_registry).chain(config.scoped_registries.values())
        {
            if !valid_registry_url(u) {
                return Err(RegistryError::InvalidRegistry);
            }
        }
        if config.default_auth.as_ref().is_some_and(|a| !a.valid())
            || config.scoped_auth.values().any(|a| !a.valid())
            || config
                .registry_auth
                .iter()
                .any(|(k, a)| !valid_registry_auth_key(k) || !a.valid())
        {
            return Err(RegistryError::InvalidAuth);
        }
        let mut b = ClientBuilder::new()
            .pool_max_idle_per_host(8)
            .http2_adaptive_window(true)
            .timeout(config.total_timeout.or(Some(config.timeout)))
            .connect_timeout(config.request_timeout.or(Some(config.timeout)))
            .use_rustls_tls()
            .gzip(true);
        if let Some(p) = &config.proxy {
            let mut proxy = reqwest::Proxy::all(p).map_err(|_| RegistryError::InvalidRegistry)?;
            if let Some(no_proxy) = &config.no_proxy {
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(no_proxy));
            }
            b = b.proxy(proxy);
        }
        if let Some(ca) = &config.custom_ca_bundle {
            let bytes = fs::read(ca).map_err(|_| RegistryError::InvalidCa)?;
            let cert =
                reqwest::Certificate::from_pem(&bytes).map_err(|_| RegistryError::InvalidCa)?;
            b = b.add_root_certificate(cert);
        }
        if !config.strict_ssl {
            b = b.danger_accept_invalid_certs(true);
        }
        let client = b.build().map_err(|_| RegistryError::Transport)?;
        let s = Self {
            config,
            client,
            cache: Arc::new(Mutex::new(HashMap::new())),
        };
        s.load_cache();
        Ok(s)
    }
    pub fn config(&self) -> &RegistryConfig {
        &self.config
    }
    pub fn cached(&self, name: &str) -> Option<CacheEntry> {
        self.cache.lock().ok()?.get(name).cloned()
    }
    pub fn insert_cache(&self, name: impl Into<String>, entry: CacheEntry) {
        let n = name.into();
        if let Ok(mut c) = self.cache.lock() {
            c.insert(n.clone(), entry.clone());
        }
        self.persist(&n, &entry)
    }
    pub fn clear_cache(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.clear();
        }
    }
    pub fn packument(&self, name: &str) -> Result<Packument, RegistryError> {
        validate_name(name)?;
        let cached = self.cached(name);
        if self.config.offline {
            return cached
                .map(|e| parse_packument(&e.body))
                .unwrap_or(Err(RegistryError::OfflineMiss));
        }
        if self.config.prefer_offline
            && let Some(c) = cached.clone()
        {
            return parse_packument(&c.body);
        }
        let url = format!(
            "{}/{}",
            self.config.registry_for(name).trim_end_matches('/'),
            package_path(name)
        );
        self.fetch(&url, name, cached)
    }

    /// Download a tarball without buffering it, reusing the configured client.
    /// Credentials are only sent to the package registry or an explicitly
    /// matching registry-auth key, never to an unrelated CDN host.
    pub fn download_tarball(
        &self,
        name: &str,
        url: &str,
    ) -> Result<reqwest::blocking::Response, RegistryError> {
        validate_name(name)?;
        let parsed = reqwest::Url::parse(url).map_err(|_| RegistryError::InvalidRegistry)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(RegistryError::InvalidRegistry);
        }
        let auth = self.auth_for_tarball(name, url);
        let mut last = RegistryError::Transport;
        for attempt in 0..=self.config.retries {
            let mut request = self.client.get(url);
            if let Some(a) = auth {
                request = apply_auth(request, a);
            }
            match request.send() {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status().as_u16();
                    if !retryable(status) {
                        return Err(RegistryError::Http(status));
                    }
                    let retry_after = response
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    last = RegistryError::Http(status);
                    drop(response);
                    sleep_retry(attempt, self.config.retries, retry_after.as_deref());
                }
                Err(error) => {
                    last = if error.is_timeout() {
                        RegistryError::Timeout
                    } else {
                        RegistryError::Transport
                    };
                    if attempt < self.config.retries {
                        sleep_retry(attempt, self.config.retries, None);
                    }
                }
            }
        }
        Err(last)
    }

    /// Stream a tarball and transparently resume a failed response body with HTTP Range.
    ///
    /// Bytes are counted only after they have been returned to the caller, so a retry
    /// starts exactly at the last verified byte and can never duplicate input.
    pub fn download_tarball_resumable(
        &self,
        name: &str,
        url: &str,
    ) -> Result<ResumableTarball, RegistryError> {
        let response = self.download_tarball(name, url)?;
        Ok(ResumableTarball::new(
            self.clone(),
            name.to_owned(),
            url.to_owned(),
            response,
        ))
    }

    fn auth_for_tarball(&self, name: &str, url: &str) -> Option<&Auth> {
        let registry = self.config.registry_for(name);
        if registry_url_matches(url, registry) {
            return self.config.auth_for(name, registry);
        }
        self.config
            .registry_auth
            .iter()
            .filter(|(key, _)| registry_auth_matches(url, key))
            .max_by_key(|(key, _)| key.len())
            .map(|(_, auth)| auth)
    }

    fn fetch(
        &self,
        url: &str,
        name: &str,
        cached: Option<CacheEntry>,
    ) -> Result<Packument, RegistryError> {
        let mut last = RegistryError::Transport;
        for attempt in 0..=self.config.retries {
            let mut req = self.client.get(url).header("Accept", ABBREVIATED);
            if let Some(c) = &cached {
                if let Some(v) = &c.etag {
                    req = req.header("If-None-Match", v)
                }
                if let Some(v) = &c.last_modified {
                    req = req.header("If-Modified-Since", v)
                }
            }
            if let Some(a) = self.config.auth_for(name, self.config.registry_for(name)) {
                req = apply_auth(req, a)
            }
            match req.send() {
                Ok(r) => {
                    let status = r.status().as_u16();
                    if status == 304 {
                        return cached
                            .map(|c| parse_packument(&c.body))
                            .unwrap_or(Err(RegistryError::InvalidCache));
                    }
                    if status == 406 || status == 415 {
                        return self.fetch_full(url, name, cached.clone());
                    }
                    if (200..300).contains(&status) {
                        return self.consume(r, name);
                    }
                    if retryable(status) {
                        last = RegistryError::Http(status);
                        sleep_retry(
                            attempt,
                            self.config.retries,
                            r.headers().get("Retry-After").and_then(|v| v.to_str().ok()),
                        );
                    } else {
                        return Err(RegistryError::Http(status));
                    }
                }
                Err(e) => {
                    last = if e.is_timeout() {
                        RegistryError::Timeout
                    } else {
                        RegistryError::Transport
                    };
                    if attempt < self.config.retries {
                        sleep_retry(attempt, self.config.retries, None)
                    }
                }
            }
        }
        Err(last)
    }
    fn fetch_full(
        &self,
        url: &str,
        name: &str,
        cached: Option<CacheEntry>,
    ) -> Result<Packument, RegistryError> {
        let mut req = self.client.get(url).header("Accept", "application/json");
        if let Some(c) = &cached {
            if let Some(v) = &c.etag {
                req = req.header("If-None-Match", v)
            }
            if let Some(v) = &c.last_modified {
                req = req.header("If-Modified-Since", v)
            }
        }
        if let Some(a) = self.config.auth_for(name, self.config.registry_for(name)) {
            req = apply_auth(req, a)
        }
        let r = req.send().map_err(|e| {
            if e.is_timeout() {
                RegistryError::Timeout
            } else {
                RegistryError::Transport
            }
        })?;
        if r.status().as_u16() == 304 {
            return cached
                .map(|c| parse_packument(&c.body))
                .unwrap_or(Err(RegistryError::InvalidCache));
        }
        if !r.status().is_success() {
            return Err(RegistryError::Http(r.status().as_u16()));
        }
        self.consume(r, name)
    }
    fn consume(
        &self,
        r: reqwest::blocking::Response,
        name: &str,
    ) -> Result<Packument, RegistryError> {
        let etag = r
            .headers()
            .get("ETag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let lm = r
            .headers()
            .get("Last-Modified")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = read_metadata_body(r, MAX_METADATA_BYTES)?;
        if body.is_empty() {
            return Err(RegistryError::Truncated);
        }
        let p = parse_packument(&body)?;
        self.insert_cache(
            name.to_owned(),
            CacheEntry {
                body,
                etag,
                last_modified: lm,
            },
        );
        Ok(p)
    }
    fn load_cache(&self) {
        let Some(dir) = &self.config.cache_dir else {
            return;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        if let Ok(mut c) = self.cache.lock() {
            for e in entries.flatten() {
                let path = e.path();
                if let Ok(bytes) = fs::read(&path)
                    && let Ok(entry) = serde_json::from_slice::<CacheEntry>(&bytes)
                    && let Some(name) = path.file_stem().and_then(|value| value.to_str())
                {
                    c.insert(name.replace("%2F", "/"), entry);
                }
            }
        }
    }
    fn persist(&self, name: &str, e: &CacheEntry) {
        let Some(dir) = &self.config.cache_dir else {
            return;
        };
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        let path = dir.join(cache_key(name));
        if let Ok(bytes) = serde_json::to_vec(e)
            && let Ok(mut file) = fs::File::create(path)
        {
            let _ = file.write_all(&bytes);
        }
    }
}

fn read_metadata_body(reader: impl Read, max_bytes: usize) -> Result<String, RegistryError> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| RegistryError::Truncated)?;
    if bytes.len() > max_bytes {
        return Err(RegistryError::InvalidMetadata(format!(
            "registry metadata exceeds the {max_bytes}-byte response limit"
        )));
    }
    String::from_utf8(bytes).map_err(|error| RegistryError::InvalidJsonBody(error.to_string()))
}

impl Registry {
    pub fn new_with_cache(
        config: RegistryConfig,
        cache_dir: impl AsRef<Path>,
    ) -> Result<Self, RegistryError> {
        let mut c = config;
        c.cache_dir = Some(cache_dir.as_ref().to_path_buf());
        Self::new(c)
    }
}
impl RegistryBackend for Registry {
    fn packument(&self, name: &str) -> Result<Packument, RegistryError> {
        self.packument(name)
    }
}
fn parse_packument(body: &str) -> Result<Packument, RegistryError> {
    let p: Packument =
        serde_json::from_str(body).map_err(|e| RegistryError::InvalidJsonBody(e.to_string()))?;
    if p.versions.is_empty() && p.dist_tags.is_empty() {
        return Err(RegistryError::Truncated);
    }
    for (v, m) in &p.versions {
        if !valid_component(v)
            || m.name
                .as_deref()
                .is_some_and(|name| PackageName::new(name.to_owned()).is_err())
        {
            return Err(RegistryError::InvalidMetadata(
                "unsafe package metadata".into(),
            ));
        }
        if let Some(t) = &m.dist.tarball
            && !(t.starts_with("http://") || t.starts_with("https://"))
        {
            return Err(RegistryError::InvalidMetadata("invalid tarball URL".into()));
        }
    }
    Ok(p)
}
fn validate_name(n: &str) -> Result<(), RegistryError> {
    PackageName::new(n.to_owned())
        .map(|_| ())
        .map_err(|_| RegistryError::InvalidPackage)
}
fn valid_component(s: &str) -> bool {
    !s.is_empty()
        && !s.chars().any(|c| c.is_control() || c == '/' || c == '\\')
        && !s.contains("..")
}
fn package_path(n: &str) -> String {
    n.replace('/', "%2F")
}
fn cache_key(n: &str) -> String {
    format!("{}.json", n.replace('/', "%2F"))
}
fn retryable(s: u16) -> bool {
    (500..600).contains(&s) || s == 408 || s == 429
}
fn sleep_retry(attempt: usize, max: usize, retry_after: Option<&str>) {
    if attempt >= max {
        return;
    }
    let parsed = retry_after.and_then(parse_retry_after);
    let base = Duration::from_millis(50u64.saturating_mul(1u64 << attempt.min(6)));
    let jitter = Duration::from_millis(
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64)
            % base.as_millis().max(1) as u64,
    );
    thread::sleep(parsed.unwrap_or(base + jitter).min(Duration::from_secs(30)))
}

/// Parse both forms permitted by RFC 9110: a non-negative delay in seconds or
/// an IMF-fixdate.  Keeping this small parser local avoids making the registry
/// client depend on a second date crate merely for Retry-After.
fn parse_retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let (_, rest) = value.trim().split_once(",")?;
    let mut fields = rest.split_whitespace();
    let day = fields.next()?.parse::<u32>().ok()?;
    let month = match fields.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = fields.next()?.parse::<i64>().ok()?;
    let date = fields.next()?;
    let (hour, minute, second) = date.split_once(':').and_then(|(h, tail)| {
        let (m, s) = tail.split_once(':')?;
        Some((
            h.parse::<u64>().ok()?,
            m.parse::<u64>().ok()?,
            s.parse::<u64>().ok()?,
        ))
    })?;
    if hour > 23 || minute > 59 || second > 59 || !(1..=31).contains(&day) {
        return None;
    }
    let target = unix_seconds(year, month, day as i64, hour, minute, second)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(Duration::from_secs(target.saturating_sub(now) as u64))
}

// Days-from-civil, valid for the Gregorian calendar and independent of the
// process timezone (Retry-After dates are always GMT).
fn unix_seconds(
    year: i64,
    month: u32,
    day: i64,
    hour: u64,
    minute: u64,
    second: u64,
) -> Option<i64> {
    if !(1..=12).contains(&month) || year < 1970 {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from(month) + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64)
}
fn apply_auth(r: reqwest::blocking::RequestBuilder, a: &Auth) -> reqwest::blocking::RequestBuilder {
    r.header("Authorization", auth_header(a))
}
fn auth_header(a: &Auth) -> String {
    match a {
        Auth::Bearer(t) => format!("Bearer {t}"),
        Auth::Basic { username, password } => format!(
            "Basic {}",
            STANDARD.encode(format!("{username}:{password}"))
        ),
    }
}
fn valid_registry_url(u: &str) -> bool {
    let Some((s, r)) = u.split_once("://") else {
        return false;
    };
    if !matches!(s, "http" | "https") || u.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return false;
    }
    let a = r.split(['/', '?', '#']).next().unwrap_or_default();
    !a.is_empty() && !a.contains('@')
}
fn registry_auth_matches(r: &str, k: &str) -> bool {
    let Some(t) = r.strip_prefix("https:").or_else(|| r.strip_prefix("http:")) else {
        return false;
    };
    let t = t.trim_end_matches('/');
    let k = k.trim_end_matches('/');
    t == k || t.strip_prefix(k).is_some_and(|x| x.starts_with('/'))
}
fn registry_url_matches(url: &str, registry: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let Ok(registry) = reqwest::Url::parse(registry) else {
        return false;
    };
    url.scheme() == registry.scheme()
        && url.host_str() == registry.host_str()
        && url.port_or_known_default() == registry.port_or_known_default()
        && url
            .path()
            .starts_with(registry.path().trim_end_matches('/'))
}
fn valid_registry_auth_key(k: &str) -> bool {
    k.starts_with("//")
        && k.trim_end_matches('/').len() > 2
        && !k.contains('@')
        && !k.chars().any(|c| c.is_control() || c.is_whitespace())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates() {
        assert!(validate_name("left-pad").is_ok());
        assert!(validate_name("@scope/pkg").is_ok());
        assert!(validate_name("../x").is_err());
        assert_eq!(package_path("@scope/pkg"), "@scope%2Fpkg")
    }
    #[test]
    fn auth_redacts() {
        let a = Auth::Basic {
            username: "u".into(),
            password: "p".into(),
        };
        assert_eq!(auth_header(&a), "Basic dTpw");
        assert!(!format!("{a:?}").contains('p'))
    }

    #[test]
    fn scoped_package_metadata_is_accepted_but_unsafe_names_are_rejected() {
        let scoped =
            parse_packument(r#"{"versions":{"7.0.0":{"name":"@babel/parser","version":"7.0.0"}}}"#);
        assert!(scoped.is_ok());

        let unsafe_name =
            parse_packument(r#"{"versions":{"1.0.0":{"name":"../escape","version":"1.0.0"}}}"#);
        assert!(matches!(
            unsafe_name,
            Err(RegistryError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn metadata_response_body_is_bounded() {
        let accepted = read_metadata_body(io::Cursor::new(b"{}"), 2).unwrap();
        assert_eq!(accepted, "{}");
        assert!(matches!(
            read_metadata_body(io::Cursor::new(b"{} "), 2),
            Err(RegistryError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn npm_camel_case_optional_and_peer_dependency_fields_are_parsed() {
        let packument = parse_packument(
            r#"{"versions":{"1.0.0":{"version":"1.0.0","optionalDependencies":{"optional":"^2.0.0"},"peerDependencies":{"required-peer":"^3.0.0","optional-peer":"^4.0.0"},"peerDependenciesMeta":{"optional-peer":{"optional":true}}}}}"#,
        )
        .unwrap();
        let metadata = &packument.versions["1.0.0"];
        assert_eq!(metadata.optional_dependencies["optional"], "^2.0.0");
        assert_eq!(metadata.peer_dependencies["required-peer"], "^3.0.0");
        assert!(metadata.peer_dependencies_meta["optional-peer"].optional);
    }

    #[test]
    fn npm_deprecated_version_metadata_is_parsed() {
        let packument = parse_packument(
            r#"{"versions":{"1.0.0":{"version":"1.0.0","deprecated":"use the maintained release"}}}"#,
        )
        .unwrap();
        assert_eq!(
            packument.versions["1.0.0"].deprecated.as_deref(),
            Some("use the maintained release")
        );
    }

    #[test]
    fn engines_arrays_ok() {
        let p = parse_packument(r#"{"versions":{"1.0.0":{"version":"1.0.0","engines":["node"]}}}"#)
            .unwrap();
        assert!(p.versions["1.0.0"].engines.is_empty())
    }
    #[test]
    fn scoped_auth_is_host_bound() {
        let mut c = RegistryConfig::default();
        c.registry_auth
            .insert("//private.example/".into(), Auth::Bearer("x".into()));
        assert!(c.auth_for("x", "https://private.example/path").is_some());
        assert!(
            c.auth_for("x", "https://private.example.evil/path")
                .is_none()
        )
    }

    #[test]
    fn retry_after_accepts_seconds_and_imf_fixdate() {
        assert_eq!(parse_retry_after("7"), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("not-a-date"), None);
        let date = "Wed, 01 Jan 2090 00:00:00 GMT";
        assert!(parse_retry_after(date).is_some_and(|d| d > Duration::from_secs(1)));
    }

    #[test]
    fn invalid_proxy_and_custom_ca_settings_fail_closed() {
        let proxy = RegistryConfig {
            proxy: Some("not a valid proxy URL".into()),
            ..RegistryConfig::default()
        };
        assert!(matches!(
            Registry::new(proxy),
            Err(RegistryError::InvalidRegistry)
        ));

        let missing_ca = std::env::temp_dir()
            .join(format!("jsm-missing-ca-{}", std::process::id()))
            .join("nonexistent.pem");
        let ca = RegistryConfig {
            custom_ca_bundle: Some(missing_ca),
            ..RegistryConfig::default()
        };
        assert!(matches!(Registry::new(ca), Err(RegistryError::InvalidCa)));
    }

    #[test]
    fn tarball_auth_is_not_sent_to_unrelated_hosts() {
        let mut config = RegistryConfig {
            default_registry: "https://registry.example.test/".into(),
            default_auth: Some(Auth::Bearer("registry-secret".into())),
            ..RegistryConfig::default()
        };
        config.registry_auth.insert(
            "//cdn.example.test/private/".into(),
            Auth::Bearer("cdn-secret".into()),
        );
        let client = Registry::new(config).unwrap();
        assert!(
            client
                .auth_for_tarball("pkg", "https://registry.example.test/pkg.tgz")
                .is_some()
        );
        assert!(
            client
                .auth_for_tarball("pkg", "https://cdn.example.test/public/pkg.tgz")
                .is_none()
        );
        assert!(
            client
                .auth_for_tarball("pkg", "https://cdn.example.test/private/pkg.tgz")
                .is_some()
        );
    }

    #[test]
    fn offline_cache_hit_and_miss_and_persistence() {
        let dir = std::env::temp_dir().join(format!(
            "jsm-registry-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let online = RegistryConfig {
            default_registry: "http://127.0.0.1:1".into(),
            ..RegistryConfig::default()
        };
        let client = Registry::new_with_cache(online, &dir).unwrap();
        client.insert_cache(
            "cached-pkg",
            CacheEntry {
                body: r#"{"name":"cached-pkg","dist-tags":{"latest":"1.0.0"}}"#.into(),
                etag: Some("\"v1\"".into()),
                last_modified: None,
            },
        );

        let offline = RegistryConfig {
            offline: true,
            ..RegistryConfig::default()
        };
        let restored = Registry::new_with_cache(offline, &dir).unwrap();
        assert!(restored.packument("cached-pkg").is_ok());
        assert!(matches!(
            restored.packument("missing-pkg"),
            Err(RegistryError::OfflineMiss)
        ));
        assert_eq!(
            restored.cached("cached-pkg").unwrap().etag.as_deref(),
            Some("\"v1\"")
        );
        let _ = fs::remove_dir_all(dir);
    }
}
