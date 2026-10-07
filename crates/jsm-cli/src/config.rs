//! Layered configuration and safe `.npmrc`/`jsm.toml` parsing.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::{collections::BTreeMap, env, fmt, fs, path::Path};

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub registry: Option<String>,
    pub scopes: BTreeMap<String, String>,
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub prefer_offline: Option<bool>,
    pub strict_ssl: Option<bool>,
    pub cafile: Option<String>,
    pub store_dir: Option<String>,
    pub default_auth: Option<Credential>,
    pub scoped_auth: BTreeMap<String, Credential>,
    pub registry_auth: BTreeMap<String, Credential>,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    Bearer(String),
    Basic { username: String, password: String },
}
impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Bearer([REDACTED])"),
            Self::Basic { .. } => f.write_str("Basic([REDACTED])"),
        }
    }
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("registry", &self.registry)
            .field("scopes", &self.scopes)
            .field("proxy", &self.proxy)
            .field("no_proxy", &self.no_proxy)
            .field("prefer_offline", &self.prefer_offline)
            .field("strict_ssl", &self.strict_ssl)
            .field("cafile", &self.cafile)
            .field("store_dir", &self.store_dir)
            .field("default_auth", &self.default_auth)
            .field("scoped_auth", &self.scoped_auth)
            .finish()
    }
}
impl Config {
    pub fn auth_configured(&self) -> bool {
        self.default_auth.is_some()
            || !self.scoped_auth.is_empty()
            || !self.registry_auth.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Defaults,
    User,
    Workspace,
    Project,
    Environment,
    Cli,
}

#[derive(Debug, Clone, Default)]
pub struct LayeredConfig {
    values: BTreeMap<Layer, Config>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}:{line}: invalid key `{key}`: {message}")]
    Invalid {
        path: String,
        line: usize,
        key: String,
        message: String,
    },
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl Config {
    pub fn parse_npmrc(path: impl Into<String>, text: &str) -> Result<Self, ConfigError> {
        Self::parse_lines(path.into(), text)
    }
    pub fn parse_jsm_toml(path: impl Into<String>, text: &str) -> Result<Self, ConfigError> {
        let path = path.into();
        let table = toml::from_str::<toml::Table>(text).map_err(|error| {
            let line = error
                .span()
                .map(|span| {
                    text.as_bytes()[..span.start.min(text.len())]
                        .iter()
                        .filter(|b| **b == b'\n')
                        .count()
                        + 1
                })
                .unwrap_or(1);
            invalid(&path, line, "<toml>", &error.to_string())
        })?;
        let mut config = Config::default();
        for (key, value) in table {
            let line = toml_key_line(text, &key);
            let value = match (key.as_str(), value) {
                (
                    "prefer-offline" | "prefer_offline" | "strict-ssl" | "strict_ssl",
                    toml::Value::Boolean(value),
                ) => value.to_string(),
                ("prefer-offline" | "prefer_offline" | "strict-ssl" | "strict_ssl", _) => {
                    return Err(invalid(&path, line, &key, "expected a TOML boolean"));
                }
                (_, toml::Value::String(value)) => value,
                (_, _) => return Err(invalid(&path, line, &key, "expected a TOML string")),
            };
            config.apply_key_mode(&path, line, &key, &value, true)?;
        }
        Ok(config)
    }
    fn parse_lines(path: String, text: &str) -> Result<Self, ConfigError> {
        let mut c = Config::default();
        for (i, original) in text.lines().enumerate() {
            let line_no = i + 1;
            let line = original.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((rawk, rawv)) = line.split_once('=') else {
                return Err(invalid(&path, line_no, line, "expected key = value"));
            };
            let key = rawk.trim().trim_matches('"');
            if key.is_empty() {
                return Err(invalid(&path, line_no, key, "key must not be empty"));
            }
            let val = unquote(strip_comment(rawv.trim()));
            c.apply_key_mode(&path, line_no, key, &val, false)?;
        }
        Ok(c)
    }
    pub fn from_npmrc(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let p = path.as_ref();
        Self::parse_npmrc(
            p.display().to_string(),
            &fs::read_to_string(p).map_err(|source| ConfigError::Io {
                path: p.display().to_string(),
                source,
            })?,
        )
    }
    pub fn from_jsm_toml(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let p = path.as_ref();
        Self::parse_jsm_toml(
            p.display().to_string(),
            &fs::read_to_string(p).map_err(|source| ConfigError::Io {
                path: p.display().to_string(),
                source,
            })?,
        )
    }
    pub fn from_jsmrc(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let p = path.as_ref();
        let text = fs::read_to_string(p).map_err(|source| ConfigError::Io {
            path: p.display().to_string(),
            source,
        })?;
        let value: Value = serde_json::from_str(&text).map_err(|e| {
            invalid(
                &p.display().to_string(),
                e.line().max(1),
                "<json>",
                &e.to_string(),
            )
        })?;
        let obj = value
            .as_object()
            .ok_or_else(|| invalid(&p.display().to_string(), 1, "<root>", "expected an object"))?;
        let mut c = Config::default();
        for (k, v) in obj {
            let s = v
                .as_str()
                .map(str::to_owned)
                .or_else(|| v.as_bool().map(|b| b.to_string()))
                .ok_or_else(|| {
                    invalid(&p.display().to_string(), 1, k, "expected string or boolean")
                })?;
            c.apply_key_mode(&p.display().to_string(), 1, k, &s, true)?;
        }
        Ok(c)
    }
    fn apply_key_mode(
        &mut self,
        path: &str,
        line: usize,
        key: &str,
        value: &str,
        strict: bool,
    ) -> Result<(), ConfigError> {
        let value = expand(value);
        match key {
            "registry" => self.registry = Some(value),
            "proxy" | "http-proxy" | "https-proxy" => self.proxy = Some(value),
            "no-proxy" | "no_proxy" => self.no_proxy = Some(value),
            "prefer-offline" | "prefer_offline" => {
                self.prefer_offline = parse_bool(&value)
                    .ok_or_else(|| invalid(path, line, key, "expected true or false"))
                    .map(Some)?
            }
            "strict-ssl" | "strict_ssl" => {
                self.strict_ssl = parse_bool(&value)
                    .ok_or_else(|| invalid(path, line, key, "expected true or false"))
                    .map(Some)?
            }
            "cafile" => self.cafile = Some(value),
            "store-dir" | "store_dir" => {
                if value.trim().is_empty() {
                    return Err(invalid(path, line, key, "store path must not be empty"));
                }
                self.store_dir = Some(value);
            }
            "_authToken" | "auth-token" => self.default_auth = Some(Credential::Bearer(value)),
            "auth" | "_auth" => self.default_auth = Some(parse_basic(path, line, key, &value)?),
            k if k.starts_with('@') && k.ends_with(":registry") => {
                validate_scope(k).map_err(|m| invalid(path, line, key, &m))?;
                self.scopes.insert(k[..k.len() - 9].to_string(), value);
            }
            k if k.starts_with('@') && (k.ends_with(":_authToken") || k.ends_with(":_auth")) => {
                let scope = k.split(':').next().unwrap();
                validate_scope(scope).map_err(|m| invalid(path, line, key, &m))?;
                let credential = if k.ends_with("_authToken") {
                    Credential::Bearer(value)
                } else {
                    parse_basic(path, line, key, &value)?
                };
                self.scoped_auth.insert(scope.to_string(), credential);
            }
            k if k.starts_with("//") && (k.ends_with(":_authToken") || k.ends_with(":_auth")) => {
                let (registry, suffix) = k.rsplit_once(':').unwrap();
                if registry.len() <= 2 || registry.chars().any(char::is_whitespace) {
                    return Err(invalid(path, line, key, "invalid registry auth key"));
                }
                let credential = if suffix == "_authToken" {
                    Credential::Bearer(value)
                } else {
                    parse_basic(path, line, key, &value)?
                };
                self.registry_auth.insert(registry.to_string(), credential);
            }
            _ if strict => return Err(invalid(path, line, key, "unknown configuration key")),
            // npmrc commonly contains settings owned by other tools.
            _ => {}
        }
        Ok(())
    }
    pub fn get(&self, key: &str) -> Option<String> {
        match key {
            "registry" => self.registry.clone(),
            "proxy" | "http-proxy" | "https-proxy" => self.proxy.clone(),
            "no-proxy" | "no_proxy" => self.no_proxy.clone(),
            "prefer-offline" | "prefer_offline" => self.prefer_offline.map(|v| v.to_string()),
            "strict-ssl" | "strict_ssl" => self.strict_ssl.map(|v| v.to_string()),
            "cafile" => self.cafile.clone(),
            "store-dir" | "store_dir" => self.store_dir.clone(),
            "auth" | "_auth" | "_authToken" | "auth-token" => {
                self.auth_configured().then(|| "<redacted>".into())
            }
            k if k.starts_with('@') && k.ends_with(":registry") => {
                self.scopes.get(&k[..k.len() - 9]).cloned()
            }
            k if k.starts_with('@') && (k.ends_with(":_auth") || k.ends_with(":_authToken")) => {
                self.scoped_auth
                    .get(k.split(':').next().unwrap())
                    .map(|_| "<redacted>".into())
            }
            k if k.starts_with("//") && (k.ends_with(":_auth") || k.ends_with(":_authToken")) => {
                let registry = k.rsplit_once(':').map(|(prefix, _)| prefix)?;
                self.registry_auth
                    .get(registry)
                    .map(|_| "<redacted>".into())
            }
            _ => None,
        }
    }
    pub fn set_key(&mut self, key: &str, value: &str) -> Result<(), ConfigError> {
        self.apply_key_mode("<config>", 0, key, value, true)
    }
    pub fn delete_key(&mut self, key: &str) -> bool {
        match key {
            "registry" => self.registry.take().is_some(),
            "proxy" | "http-proxy" | "https-proxy" => self.proxy.take().is_some(),
            "no-proxy" | "no_proxy" => self.no_proxy.take().is_some(),
            "prefer-offline" | "prefer_offline" => self.prefer_offline.take().is_some(),
            "strict-ssl" | "strict_ssl" => self.strict_ssl.take().is_some(),
            "cafile" => self.cafile.take().is_some(),
            "store-dir" | "store_dir" => self.store_dir.take().is_some(),
            "auth" | "_auth" | "_authToken" | "auth-token" => self.default_auth.take().is_some(),
            k if k.starts_with('@') && k.ends_with(":registry") => {
                self.scopes.remove(&k[..k.len() - 9]).is_some()
            }
            k if k.starts_with('@') && (k.ends_with(":_auth") || k.ends_with(":_authToken")) => {
                self.scoped_auth
                    .remove(k.split(':').next().unwrap())
                    .is_some()
            }
            k if k.starts_with("//") && (k.ends_with(":_auth") || k.ends_with(":_authToken")) => k
                .rsplit_once(':')
                .and_then(|(registry, _)| self.registry_auth.remove(registry))
                .is_some(),
            _ => false,
        }
    }
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        if let Some(v) = &self.registry {
            out.push_str(&format!("registry = \"{}\"\n", escape(v)));
        }
        if let Some(v) = &self.proxy {
            out.push_str(&format!("proxy = \"{}\"\n", escape(v)));
        }
        if let Some(v) = &self.no_proxy {
            out.push_str(&format!("no-proxy = \"{}\"\n", escape(v)));
        }
        if let Some(v) = self.prefer_offline {
            out.push_str(&format!("prefer-offline = {v}\n"));
        }
        if let Some(v) = self.strict_ssl {
            out.push_str(&format!("strict-ssl = {v}\n"));
        }
        if let Some(v) = &self.cafile {
            out.push_str(&format!("cafile = \"{}\"\n", escape(v)));
        }
        if let Some(v) = &self.store_dir {
            out.push_str(&format!("store-dir = \"{}\"\n", escape(v)));
        }
        if let Some(v) = &self.default_auth {
            let (key, value) = encode_credential(v);
            out.push_str(&format!("{key} = \"{}\"\n", escape(&value)));
        }
        for (k, v) in &self.scopes {
            out.push_str(&format!("\"{k}:registry\" = \"{}\"\n", escape(v)));
        }
        for (k, v) in &self.scoped_auth {
            let (suffix, value) = encode_credential(v);
            out.push_str(&format!("\"{k}:{suffix}\" = \"{}\"\n", escape(&value)));
        }
        for (k, v) in &self.registry_auth {
            let (suffix, value) = encode_credential(v);
            out.push_str(&format!("\"{k}:{suffix}\" = \"{}\"\n", escape(&value)));
        }
        out
    }
    pub fn list(&self) -> BTreeMap<String, String> {
        let mut o = BTreeMap::new();
        for k in [
            "registry",
            "proxy",
            "no-proxy",
            "prefer-offline",
            "strict-ssl",
            "cafile",
            "store-dir",
            "auth",
        ] {
            if let Some(v) = self.get(k) {
                o.insert(k.into(), v);
            }
        }
        for (k, v) in &self.scopes {
            o.insert(format!("{k}:registry"), v.clone());
        }
        for k in self.scoped_auth.keys() {
            o.insert(format!("{k}:_authToken"), "<redacted>".into());
        }
        for k in self.registry_auth.keys() {
            o.insert(format!("{k}:_authToken"), "<redacted>".into());
        }
        o
    }
}
impl LayeredConfig {
    pub fn set(&mut self, l: Layer, c: Config) {
        self.values.insert(l, c);
    }
    pub fn layer(&self, l: Layer) -> Option<&Config> {
        self.values.get(&l)
    }
    pub fn resolve(&self) -> Config {
        let mut o = Config::default();
        for l in [
            Layer::Defaults,
            Layer::User,
            Layer::Workspace,
            Layer::Project,
            Layer::Environment,
            Layer::Cli,
        ] {
            if let Some(c) = self.values.get(&l) {
                if c.registry.is_some() {
                    o.registry = c.registry.clone();
                }
                if c.proxy.is_some() {
                    o.proxy = c.proxy.clone();
                }
                if c.no_proxy.is_some() {
                    o.no_proxy = c.no_proxy.clone();
                }
                if c.prefer_offline.is_some() {
                    o.prefer_offline = c.prefer_offline;
                }
                if c.strict_ssl.is_some() {
                    o.strict_ssl = c.strict_ssl;
                }
                if c.cafile.is_some() {
                    o.cafile = c.cafile.clone();
                }
                if c.store_dir.is_some() {
                    o.store_dir = c.store_dir.clone();
                }
                if c.default_auth.is_some() {
                    o.default_auth = c.default_auth.clone();
                }
                for (s, v) in &c.scopes {
                    o.scopes.insert(s.clone(), v.clone());
                }
                for (s, v) in &c.scoped_auth {
                    o.scoped_auth.insert(s.clone(), v.clone());
                }
                for (s, v) in &c.registry_auth {
                    o.registry_auth.insert(s.clone(), v.clone());
                }
            }
        }
        o
    }
    pub fn get(&self, k: &str) -> Option<String> {
        self.resolve().get(k)
    }
    pub fn list(&self) -> BTreeMap<String, String> {
        self.resolve().list()
    }
    pub fn set_key(&mut self, l: Layer, k: &str, v: &str) -> Result<(), ConfigError> {
        self.values.entry(l).or_default().set_key(k, v)
    }
    pub fn delete_key(&mut self, l: Layer, k: &str) -> bool {
        self.values.get_mut(&l).is_some_and(|c| c.delete_key(k))
    }
    pub fn from_environment() -> Config {
        Self::try_from_environment().unwrap_or_default()
    }
    pub fn try_from_environment() -> Result<Config, ConfigError> {
        let mut c = Config::default();
        if let Ok(v) = env::var("JSM_REGISTRY") {
            c.registry = Some(expand(&v));
        }
        if let Ok(v) = env::var("JSM_PROXY") {
            c.proxy = Some(expand(&v));
        }
        if let Ok(v) = env::var("JSM_NO_PROXY") {
            c.no_proxy = Some(expand(&v));
        }
        if let Ok(v) = env::var("JSM_PREFER_OFFLINE") {
            c.prefer_offline = Some(parse_bool(&v).ok_or_else(|| {
                invalid(
                    "environment",
                    0,
                    "JSM_PREFER_OFFLINE",
                    "expected true or false",
                )
            })?);
        }
        if let Ok(v) = env::var("JSM_STRICT_SSL") {
            c.strict_ssl = Some(parse_bool(&v).ok_or_else(|| {
                invalid("environment", 0, "JSM_STRICT_SSL", "expected true or false")
            })?);
        }
        if let Ok(v) = env::var("JSM_CAFILE") {
            c.cafile = Some(expand(&v));
        }
        if let Ok(v) = env::var("JSM_STORE_DIR")
            && !v.trim().is_empty()
        {
            c.store_dir = Some(expand(&v));
        }
        if let Ok(v) = env::var("JSM_AUTH_TOKEN") {
            c.default_auth = Some(Credential::Bearer(v));
        }
        if let Ok(v) = env::var("JSM_AUTH") {
            c.default_auth = Some(parse_basic("environment", 0, "JSM_AUTH", &v)?);
        }
        for (k, v) in env::vars() {
            if let Some(s) = k
                .strip_prefix("JSM_SCOPE_")
                .and_then(|s| s.strip_suffix("_REGISTRY"))
            {
                c.scopes.insert(
                    format!("@{}", s.to_ascii_lowercase().replace('_', "-")),
                    expand(&v),
                );
            }
        }
        let mut npm = parse_npm_environment(env::vars())?;
        if c.registry.is_none() {
            c.registry = npm.registry.take();
        }
        if c.proxy.is_none() {
            c.proxy = npm.proxy.take();
        }
        if c.no_proxy.is_none() {
            c.no_proxy = npm.no_proxy.take();
        }
        if c.prefer_offline.is_none() {
            c.prefer_offline = npm.prefer_offline;
        }
        if c.strict_ssl.is_none() {
            c.strict_ssl = npm.strict_ssl;
        }
        if c.cafile.is_none() {
            c.cafile = npm.cafile.take();
        }
        if c.store_dir.is_none() {
            c.store_dir = npm.store_dir.take();
        }
        if c.default_auth.is_none() {
            c.default_auth = npm.default_auth.take();
        }
        for (key, value) in npm.scopes {
            c.scopes.entry(key).or_insert(value);
        }
        for (key, value) in npm.scoped_auth {
            c.scoped_auth.entry(key).or_insert(value);
        }
        for (key, value) in npm.registry_auth {
            c.registry_auth.entry(key).or_insert(value);
        }
        Ok(c)
    }
}

fn parse_npm_environment(
    vars: impl IntoIterator<Item = (String, String)>,
) -> Result<Config, ConfigError> {
    let mut config = Config::default();
    let mut username = None;
    let mut password = None;
    for (env_key, value) in vars {
        let normalized = env_key.to_ascii_lowercase();
        let Some(raw_key) = normalized.strip_prefix("npm_config_") else {
            continue;
        };
        match raw_key {
            "username" => {
                username = Some(value);
                continue;
            }
            "password" => {
                password = Some(value);
                continue;
            }
            _ => {}
        }
        if let Some(key) = npm_environment_key(raw_key) {
            config.apply_key_mode("environment", 0, &key, &value, false)?;
        }
    }
    if config.default_auth.is_none()
        && let (Some(username), Some(password)) = (username, password)
        && !username.is_empty()
    {
        config.default_auth = Some(Credential::Basic { username, password });
    }
    Ok(config)
}

fn npm_environment_key(key: &str) -> Option<String> {
    Some(match key {
        "http_proxy" | "https_proxy" => "proxy".into(),
        "no_proxy" => "no-proxy".into(),
        "prefer_offline" => "prefer-offline".into(),
        "strict_ssl" => "strict-ssl".into(),
        "store_dir" => "store-dir".into(),
        "auth_token" | "_authtoken" => "_authToken".into(),
        key if key.starts_with('@') || key.starts_with("//") => {
            if let Some((prefix, suffix)) = key.rsplit_once(':') {
                match suffix {
                    "_authtoken" => format!("{prefix}:_authToken"),
                    "_auth" => format!("{prefix}:_auth"),
                    _ => key.into(),
                }
            } else {
                key.into()
            }
        }
        key => key.into(),
    })
}

fn invalid(path: &str, line: usize, key: &str, message: &str) -> ConfigError {
    ConfigError::Invalid {
        path: path.into(),
        line,
        key: key.into(),
        message: message.into(),
    }
}
fn toml_key_line(text: &str, key: &str) -> usize {
    text.lines()
        .position(|line| {
            line.trim_start()
                .split_once('=')
                .is_some_and(|(raw_key, _)| {
                    raw_key.trim().trim_matches('"').trim_matches('\'') == key
                })
        })
        .map_or(1, |index| index + 1)
}
fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}
fn validate_scope(v: &str) -> Result<(), String> {
    if v.len() < 2 || !v.starts_with('@') || v[1..].contains(char::is_whitespace) {
        Err("invalid package scope".into())
    } else {
        Ok(())
    }
}
fn strip_comment(v: &str) -> String {
    let mut quote = None;
    for (i, ch) in v.char_indices() {
        if ch == '"' || ch == '\'' {
            quote = if quote.is_some() { None } else { Some(ch) }
        } else if ch == '#' && quote.is_none() {
            return v[..i].trim().into();
        }
    }
    v.into()
}
fn unquote(v: String) -> String {
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        v[1..v.len() - 1].into()
    } else {
        v
    }
}
fn expand(v: &str) -> String {
    let mut o = String::new();
    let mut r = v;
    while let Some(i) = r.find("${") {
        o.push_str(&r[..i]);
        let Some(j) = r[i + 2..].find('}') else {
            o.push_str(&r[i..]);
            return o;
        };
        let name = &r[i + 2..i + 2 + j];
        if valid_env_name(name) {
            o.push_str(&env::var(name).unwrap_or_default());
        } else {
            // Do not interpret malformed or nested expressions as input.
            o.push_str(&r[i..i + 3 + j]);
        }
        r = &r[i + 3 + j..];
    }
    o.push_str(r);
    o
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn encode_credential(c: &Credential) -> (&'static str, String) {
    match c {
        Credential::Bearer(v) => ("_authToken", v.clone()),
        Credential::Basic { username, password } => {
            ("_auth", STANDARD.encode(format!("{username}:{password}")))
        }
    }
}
fn parse_basic(path: &str, line: usize, key: &str, value: &str) -> Result<Credential, ConfigError> {
    let decoded = STANDARD
        .decode(value.trim())
        .map_err(|_| invalid(path, line, key, "expected Base64-encoded username:password"))?;
    let decoded = String::from_utf8(decoded)
        .map_err(|_| invalid(path, line, key, "Basic auth must be valid UTF-8"))?;
    let (username, password) = decoded
        .split_once(':')
        .filter(|(username, _)| !username.is_empty())
        .ok_or_else(|| invalid(path, line, key, "expected Base64-encoded username:password"))?;
    Ok(Credential::Basic {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}
fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_and_environment_expansion_are_deterministic() {
        let (home_variable, home) = match env::var("HOME") {
            Ok(home) => ("HOME", home),
            Err(_) => (
                "USERPROFILE",
                env::var("USERPROFILE").expect("test environment supplies a home directory"),
            ),
        };
        let user = Config::parse_npmrc(
            "user/.npmrc",
            "registry=https://user.invalid\nstore-dir=/user/store\n",
        )
        .unwrap();
        let workspace = Config::parse_jsm_toml(
            "jsm.toml",
            &format!("registry = \"${{{home_variable}}}/registry\"\n"),
        )
        .unwrap();
        let project = Config::parse_npmrc(
            "project/.npmrc",
            "registry=https://project.invalid\nstore-dir=/project/store\n",
        )
        .unwrap();
        let mut layers = LayeredConfig::default();
        layers.set(
            Layer::Defaults,
            Config::parse_npmrc("defaults", "registry=https://default.invalid\n").unwrap(),
        );
        layers.set(Layer::User, user);
        layers.set(Layer::Workspace, workspace);
        layers.set(Layer::Project, project);
        layers.set(
            Layer::Environment,
            Config::parse_npmrc(
                "environment",
                "registry=https://env.invalid\nstore-dir=/env/store\nno-proxy=localhost\nprefer-offline=true\n",
            )
            .unwrap(),
        );
        assert_eq!(
            layers.get("registry").as_deref(),
            Some("https://env.invalid")
        );
        assert_eq!(layers.get("store-dir").as_deref(), Some("/env/store"));
        assert_eq!(layers.get("no-proxy").as_deref(), Some("localhost"));
        assert_eq!(layers.get("prefer-offline").as_deref(), Some("true"));
        layers.set(
            Layer::Cli,
            Config::parse_npmrc(
                "cli",
                "registry=https://cli.invalid\nstore-dir=/cli/store\n",
            )
            .unwrap(),
        );
        assert_eq!(
            layers.get("registry").as_deref(),
            Some("https://cli.invalid")
        );
        assert_eq!(layers.get("store-dir").as_deref(), Some("/cli/store"));
        assert_eq!(
            Config::parse_jsm_toml(
                "expansion",
                &format!("registry = \"https://${{{home_variable}}}/registry\"\n"),
            )
            .unwrap()
            .registry
            .as_deref(),
            Some(format!("https://{home}/registry").as_str())
        );
    }

    #[test]
    fn npm_config_environment_covers_auth_tls_proxy_and_scoped_registry() {
        let config = parse_npm_environment(
            [
                ("NPM_CONFIG_REGISTRY", "https://npm-env.invalid"),
                ("npm_config_http_proxy", "http://http-proxy.invalid"),
                ("npm_config_https_proxy", "https://proxy.invalid"),
                ("npm_config_no_proxy", "localhost,.example.test"),
                ("npm_config_strict_ssl", "false"),
                ("npm_config_cafile", "/tmp/test-ca.pem"),
                ("npm_config_@ACME:registry", "https://scope.invalid"),
                ("npm_config_@ACME:_authToken", "scope-secret"),
                (
                    "npm_config_//registry.example.test/:_authToken",
                    "host-secret",
                ),
                ("npm_config_username", "basic-user"),
                ("npm_config_password", "basic-password"),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned())),
        )
        .unwrap();

        assert_eq!(config.registry.as_deref(), Some("https://npm-env.invalid"));
        assert_eq!(config.proxy.as_deref(), Some("https://proxy.invalid"));
        assert_eq!(config.no_proxy.as_deref(), Some("localhost,.example.test"));
        assert_eq!(config.strict_ssl, Some(false));
        assert_eq!(config.cafile.as_deref(), Some("/tmp/test-ca.pem"));
        assert_eq!(
            config.scopes.get("@acme").map(String::as_str),
            Some("https://scope.invalid")
        );
        assert!(matches!(
            config.scoped_auth.get("@acme"),
            Some(Credential::Bearer(token)) if token == "scope-secret"
        ));
        assert!(matches!(
            config.registry_auth.get("//registry.example.test/"),
            Some(Credential::Bearer(token)) if token == "host-secret"
        ));
        assert!(matches!(
            config.default_auth,
            Some(Credential::Basic { ref username, ref password })
                if username == "basic-user" && password == "basic-password"
        ));
        let rendered = format!("{config:?} {:?}", config.list());
        assert!(!rendered.contains("scope-secret"));
        assert!(!rendered.contains("host-secret"));
        assert!(!rendered.contains("basic-password"));
    }

    #[test]
    fn npmrc_host_auth_is_redacted_and_basic_auth_is_decoded() {
        let config = Config::parse_npmrc(
            "project/.npmrc",
            "//registry.example.test/:_authToken=fake-token\n_auth=dXNlcjpwYXNz\n",
        )
        .unwrap();
        assert!(matches!(
            config.registry_auth.get("//registry.example.test/"),
            Some(Credential::Bearer(token)) if token == "fake-token"
        ));
        assert!(matches!(
            config.default_auth,
            Some(Credential::Basic { ref username, ref password }) if username == "user" && password == "pass"
        ));
        assert_eq!(config.get("auth").as_deref(), Some("<redacted>"));
        assert_eq!(
            config.get("//registry.example.test/:_authToken").as_deref(),
            Some("<redacted>")
        );
        let rendered = format!("{config:?} {:?}", config.list());
        assert!(!rendered.contains("fake-token"));
        assert!(!rendered.contains("pass"));
    }

    #[test]
    fn host_auth_round_trips_through_valid_toml_with_redacted_listing() {
        let mut config = Config::default();
        config
            .set_key("no-proxy", "localhost,.example.test")
            .unwrap();
        config.set_key("prefer-offline", "true").unwrap();
        config.set_key("store-dir", "/tmp/jsm-store").unwrap();
        config
            .set_key("@acme:registry", "https://registry.example.test")
            .unwrap();
        config.set_key("@acme:_authToken", "fake-token").unwrap();
        config
            .set_key("//registry.example.test/:_authToken", "private-token")
            .unwrap();
        let text = config.to_toml();
        assert!(text.contains("\"@acme:registry\""));
        assert!(text.contains("\"//registry.example.test/:_authToken\""));
        assert!(text.contains("no-proxy = \"localhost,.example.test\""));
        assert!(text.contains("prefer-offline = true"));
        assert!(text.contains("store-dir = \"/tmp/jsm-store\""));
        let restored = Config::parse_jsm_toml("jsm.toml", &text).unwrap();
        assert_eq!(
            restored.get("no-proxy").as_deref(),
            Some("localhost,.example.test")
        );
        assert_eq!(restored.get("prefer-offline").as_deref(), Some("true"));
        assert_eq!(restored.get("store-dir").as_deref(), Some("/tmp/jsm-store"));
        assert_eq!(
            restored.get("@acme:registry").as_deref(),
            Some("https://registry.example.test")
        );
        assert_eq!(
            restored.get("@acme:_authToken").as_deref(),
            Some("<redacted>")
        );
        assert_eq!(
            restored
                .get("//registry.example.test/:_authToken")
                .as_deref(),
            Some("<redacted>")
        );
        let safe = format!("{:?}", restored);
        assert!(!safe.contains("fake-token"));
        assert!(!safe.contains("private-token"));
    }

    #[test]
    fn empty_store_directory_is_rejected_with_location() {
        let error = Config::parse_npmrc("project/.npmrc", "store-dir=  \n").unwrap_err();
        assert!(error.to_string().contains("project/.npmrc:1"));
        assert!(error.to_string().contains("store path must not be empty"));
    }

    #[test]
    fn malformed_configuration_reports_file_line_and_key() {
        let error =
            Config::parse_npmrc("project/.npmrc", "registry=x\nprefer-offline=maybe").unwrap_err();
        assert!(error.to_string().contains("project/.npmrc:2"));
        assert!(error.to_string().contains("prefer-offline"));
    }

    #[test]
    fn jsm_toml_rejects_unknown_keys_but_npmrc_ignores_them() {
        let error =
            Config::parse_jsm_toml("project/jsm.toml", "future-setting = true").unwrap_err();
        assert!(error.to_string().contains("project/jsm.toml:1"));
        assert!(error.to_string().contains("future-setting"));
        assert!(Config::parse_npmrc("project/.npmrc", "future-setting=true").is_ok());
    }

    #[test]
    fn jsm_toml_uses_toml_grammar_and_enforces_schema_value_types() {
        let config = Config::parse_jsm_toml(
            "project/jsm.toml",
            "registry = \"https://registry.example.test/#mirror\" # comment\nprefer-offline = true\n\"@acme:registry\" = \"https://registry.example.test\"\n",
        )
        .unwrap();
        assert_eq!(
            config.registry.as_deref(),
            Some("https://registry.example.test/#mirror")
        );
        assert_eq!(config.get("prefer-offline").as_deref(), Some("true"));
        assert_eq!(
            config.get("@acme:registry").as_deref(),
            Some("https://registry.example.test")
        );

        let bool_error =
            Config::parse_jsm_toml("project/jsm.toml", "prefer-offline = \"true\"\n").unwrap_err();
        assert!(bool_error.to_string().contains("jsm.toml:1"));
        assert!(bool_error.to_string().contains("expected a TOML boolean"));

        let table_error = Config::parse_jsm_toml(
            "project/jsm.toml",
            "[registry]\nurl = \"https://registry.example.test\"\n",
        )
        .unwrap_err();
        assert!(table_error.to_string().contains("registry"));
        assert!(table_error.to_string().contains("expected a TOML string"));

        let syntax_error =
            Config::parse_jsm_toml("project/jsm.toml", "registry = [\n").unwrap_err();
        assert!(syntax_error.to_string().contains("jsm.toml:2"));
        assert!(syntax_error.to_string().contains("<toml>"));
    }

    #[test]
    fn malformed_environment_values_are_reported_and_expansion_is_not_shell_code() {
        let error = Config::parse_npmrc("project/.npmrc", "prefer-offline=sometimes").unwrap_err();
        assert!(error.to_string().contains("prefer-offline"));
        let config = Config::parse_jsm_toml(
            "project/jsm.toml",
            "registry = \"https://${A; echo pwned}/registry\"",
        )
        .unwrap();
        assert_eq!(
            config.registry.as_deref(),
            Some("https://${A; echo pwned}/registry")
        );
    }
}
