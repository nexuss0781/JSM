//! Build system and native compilation cache for jsm.
//!
//! This crate provides:
//! - Build key computation for deterministic native artifact caching
//! - Local build cache with content-addressed storage
//! - Parallel build scheduler with topological ordering
//! - Sandboxed build execution (Landlock/seccomp/macOS sandbox/Windows Job Objects)

use jsm_core::{Integrity, PackageName, Version};
use jsm_security::{SandboxPolicy, SandboxError, Sandbox};
use jsm_store::{Store, PackageManifest, StagedBlob};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

pub const CRATE_NAME: &str = "jsm-build";

/// Build key identifies a specific compiled variant of a package for an environment.
/// Two builds with the same key produce bit-for-bit identical outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct BuildKey {
    /// Package integrity (sha512 of tarball)
    pub package_integrity: String,
    /// Hash of all lifecycle scripts that run during build
    pub script_hash: String,
    /// Node ABI version (e.g., "108" for Node 18)
    pub node_abi: String,
    /// Target platform
    pub platform: BuildPlatform,
    /// Toolchain fingerprint (compiler versions, flags)
    pub toolchain_fingerprint: String,
    /// Allowlisted environment variables that affect build
    pub allowlisted_env: BTreeMap<String, String>,
    /// Build keys of native dependencies (transitive)
    pub dependency_build_keys: BTreeSet<String>,
    /// Sandbox policy hash
    pub sandbox_policy_hash: String,
}

/// Platform information for build key
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct BuildPlatform {
    pub os: String,
    pub cpu: String,
    pub libc: String,
}

impl BuildKey {
    /// Compute a build key for a package.
    pub fn compute(
        package_integrity: &Integrity,
        script_hash: &str,
        node_abi: &str,
        platform: BuildPlatform,
        toolchain_fingerprint: &str,
        allowlisted_env: BTreeMap<String, String>,
        dependency_build_keys: BTreeSet<String>,
        sandbox_policy: &SandboxPolicy,
    ) -> Self {
        let sandbox_policy_hash = Self::hash_sandbox_policy(sandbox_policy);
        
        Self {
            package_integrity: package_integrity.to_string(),
            script_hash: script_hash.to_string(),
            node_abi: node_abi.to_string(),
            platform,
            toolchain_fingerprint: toolchain_fingerprint.to_string(),
            allowlisted_env,
            dependency_build_keys,
            sandbox_policy_hash,
        }
    }

    /// Get the cache key string (hex-encoded SHA256 of the serialized key).
    pub fn cache_key(&self) -> String {
        let serialized = serde_json::to_vec(self).expect("BuildKey serialization should not fail");
        let hash = Sha256::digest(&serialized);
        hex::encode(hash)
    }

    fn hash_sandbox_policy(policy: &SandboxPolicy) -> String {
        let serialized = serde_json::to_vec(policy).expect("SandboxPolicy serialization should not fail");
        let hash = Sha256::digest(&serialized);
        hex::encode(hash)
    }
}

/// Build result containing the compiled artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    /// Build key used for this build
    pub build_key: BuildKey,
    /// Content hashes of output files (relative path -> sha256)
    pub outputs: BTreeMap<String, String>,
    /// Build duration in milliseconds
    pub duration_ms: u64,
    /// Build timestamp
    pub built_at: u64,
    /// Build command that was executed
    pub command: String,
    /// Build stdout (truncated)
    pub stdout: String,
    /// Build stderr (truncated)
    pub stderr: String,
    /// Whether build succeeded
    pub success: bool,
    /// Exit code if failed
    pub exit_code: Option<i32>,
}

/// Local build cache storing build results by build key.
pub struct BuildCache {
    store: Store,
    cache_dir: PathBuf,
    index: Arc<Mutex<BuildCacheIndex>>,
}

/// In-memory index of cached builds for fast lookup.
#[derive(Debug, Default, Serialize, Deserialize)]
struct BuildCacheIndex {
    entries: BTreeMap<String, BuildCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildCacheEntry {
    build_key: BuildKey,
    result: BuildResult,
    size_bytes: u64,
    last_accessed: u64,
    access_count: u64,
}

impl BuildCache {
    /// Open or create a build cache.
    pub fn new(store: Store) -> Result<Self, BuildError> {
        let cache_dir = store.root().join("builds");
        std::fs::create_dir_all(&cache_dir).map_err(BuildError::Io)?;
        
        let index_path = cache_dir.join("index.json");
        let index = if index_path.exists() {
            let data = std::fs::read(&index_path).map_err(BuildError::Io)?;
            serde_json::from_slice(&data).unwrap_or_default()
        } else {
            BuildCacheIndex::default()
        };
        
        Ok(Self {
            store,
            cache_dir,
            index: Arc::new(Mutex::new(index)),
        })
    }

    /// Check if a build result exists in cache.
    pub fn has(&self, key: &str) -> bool {
        self.index.lock().unwrap().entries.contains_key(key)
    }

    /// Get a cached build result.
    pub fn get(&self, key: &str) -> Option<BuildResult> {
        let mut index = self.index.lock().unwrap();
        if let Some(entry) = index.entries.get_mut(key) {
            entry.last_accessed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            entry.access_count += 1;
            Some(entry.result.clone())
        } else {
            None
        }
    }

    /// Store a build result in cache.
    pub fn put(&self, key: &str, entry: BuildCacheEntry) -> Result<(), BuildError> {
        let mut index = self.index.lock().unwrap();
        index.entries.insert(key.to_string(), entry);
        self.persist_index(&index)
    }

    /// Remove a build result from cache.
    pub fn remove(&self, key: &str) -> Result<(), BuildError> {
        let mut index = self.index.lock().unwrap();
        index.entries.remove(key);
        self.persist_index(&index)
    }

    /// List all cached build keys.
    pub fn list(&self) -> Vec<String> {
        self.index.lock().unwrap().entries.keys().cloned().collect()
    }

    /// Get cache statistics.
    pub fn stats(&self) -> BuildCacheStats {
        let index = self.index.lock().unwrap();
        let total_size: u64 = index.entries.values().map(|e| e.size_bytes).sum();
        BuildCacheStats {
            entry_count: index.entries.len(),
            total_size_bytes: total_size,
        }
    }

    /// Evict least recently used entries to stay within size limit.
    pub fn evict_lru(&self, max_size_bytes: u64) -> Result<u64, BuildError> {
        let mut index = self.index.lock().unwrap();
        let mut total_size: u64 = index.entries.values().map(|e| e.size_bytes).sum();
        
        if total_size <= max_size_bytes {
            return Ok(0);
        }
        
        // Sort by last_accessed ascending (oldest first)
        let mut sorted: Vec<_> = index.entries.iter()
            .map(|(k, v)| (k.clone(), v.last_accessed, v.size_bytes))
            .collect();
        sorted.sort_by_key(|(_, last_accessed, _)| *last_accessed);
        
        let mut evicted = 0u64;
        for (key, _, size) in sorted {
            if total_size <= max_size_bytes {
                break;
            }
            index.entries.remove(&key);
            total_size -= size;
            evicted += size;
        }
        
        self.persist_index(&index)?;
        Ok(evicted)
    }

    fn persist_index(&self, index: &BuildCacheIndex) -> Result<(), BuildError> {
        let index_path = self.cache_dir.join("index.json");
        let data = serde_json::to_vec_pretty(index).map_err(BuildError::Serialization)?;
        std::fs::write(&index_path, data).map_err(BuildError::Io)
    }
}

/// Build cache statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildCacheStats {
    pub entry_count: usize,
    pub total_size_bytes: u64,
}

/// Build execution context.
pub struct BuildContext {
    /// Package being built
    pub package_name: PackageName,
    /// Package version
    pub version: Version,
    /// Package integrity
    pub integrity: Integrity,
    /// Package manifest
    pub manifest: PackageManifest,
    /// Build directory (where source is extracted)
    pub build_dir: PathBuf,
    /// Output directory (where artifacts are placed)
    pub output_dir: PathBuf,
    /// Build key
    pub build_key: BuildKey,
    /// Sandbox policy
    pub sandbox_policy: SandboxPolicy,
    /// Build timeout
    pub timeout: Duration,
    /// Environment variables to set
    pub env: BTreeMap<String, String>,
}

/// Build script configuration from package.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildScriptConfig {
    /// The script to run (e.g., "node-gyp rebuild")
    pub command: String,
    /// Arguments to the command
    pub args: Vec<String>,
    /// Working directory relative to package root
    pub working_dir: Option<String>,
    /// Environment variables
    pub env: BTreeMap<String, String>,
}

/// Build errors.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Store error: {0}")]
    Store(#[from] jsm_store::StoreError),
    #[error("Sandbox error: {0}")]
    Sandbox(#[from] SandboxError),
    #[error("Build failed with exit code {code}: {message}")]
    BuildFailed { code: i32, message: String },
    #[error("Build timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("Missing build script: {0}")]
    MissingScript(String),
    #[error("Invalid build configuration: {0}")]
    InvalidConfig(String),
    #[error("Cache error: {0}")]
    Cache(String),
}

/// Compute the script hash for a package's build scripts.
pub fn compute_script_hash(manifest: &PackageManifest) -> String {
    let mut hasher = Sha256::new();
    
    // Hash all relevant scripts
    if let Some(pkg_json) = &manifest.package_json {
        let scripts = pkg_json.get("scripts").and_then(|s| s.as_object());
        if let Some(scripts) = scripts {
            // Sort keys for deterministic hashing
            let mut script_names: Vec<_> = scripts.keys().collect();
            script_names.sort();
            
            for name in script_names {
                if let Some(script) = scripts.get(name).and_then(|s| s.as_str()) {
                    hasher.update(name.as_bytes());
                    hasher.update(b":");
                    hasher.update(script.as_bytes());
                    hasher.update(b"\n");
                }
            }
        }
    }
    
    hex::encode(hasher.finalize())
}

/// Compute toolchain fingerprint for the current environment.
pub fn compute_toolchain_fingerprint() -> String {
    let mut parts = Vec::new();
    
    // Try to get compiler versions
    if let Ok(output) = Command::new("cc").arg("--version").output() {
        if let Ok(version) = String::from_utf8(output.stdout) {
            parts.push(version.lines().next().unwrap_or("").to_string());
        }
    }
    
    if let Ok(output) = Command::new("c++").arg("--version").output() {
        if let Ok(version) = String::from_utf8(output.stdout) {
            parts.push(version.lines().next().unwrap_or("").to_string());
        }
    }
    
    // Node-gyp version
    if let Ok(output) = Command::new("node-gyp").arg("--version").output() {
        if let Ok(version) = String::from_utf8(output.stdout) {
            parts.push(format!("node-gyp:{}", version.trim()));
        }
    }
    
    // Python version (used by node-gyp)
    if let Ok(output) = Command::new("python3").arg("--version").output() {
        if let Ok(version) = String::from_utf8(output.stdout) {
            parts.push(version.trim().to_string());
        }
    }
    
    let combined = parts.join("|");
    let hash = Sha256::digest(combined.as_bytes());
    hex::encode(hash)
}

/// Detect Node ABI version for the current Node.js installation.
pub fn detect_node_abi() -> String {
    // Try to get from node directly
    if let Ok(output) = Command::new("node").args(["-p", "process.versions.modules"]).output() {
        if let Ok(version) = String::from_utf8(output.stdout) {
            return version.trim().to_string();
        }
    }
    
    // Fallback: try to infer from node version
    if let Ok(output) = Command::new("node").arg("--version").output() {
        if let Ok(version) = String::from_utf8(output.stdout) {
            let v = version.trim().trim_start_matches('v');
            // Simple mapping - in reality this would use a proper ABI table
            return format!("abi-{}", v);
        }
    }
    
    "unknown".to_string()
}

/// Parallel build scheduler with topological ordering.
pub struct BuildScheduler {
    /// Maximum concurrent builds
    max_concurrency: usize,
    /// Semaphore for limiting concurrency
    semaphore: Arc<Semaphore>,
    /// Build cache
    cache: Arc<BuildCache>,
}

impl BuildScheduler {
    /// Create a new build scheduler.
    pub fn new(max_concurrency: usize, cache: Arc<BuildCache>) -> Self {
        Self {
            max_concurrency,
            semaphore: Arc::new(Semaphore::new(max_concurrency)),
            cache,
        }
    }

    /// Build multiple packages in topological order.
    /// `packages` is a map of package name -> (package manifest, build context, dependencies).
    /// Dependencies must be built before dependents.
    pub async fn build_all(
        &self,
        packages: Vec<BuildTask>,
    ) -> Result<Vec<BuildResult>, BuildError> {
        // Build dependency graph
        let mut graph = BuildGraph::new();
        for task in &packages {
            graph.add_node(task.name.clone());
            for dep in &task.dependencies {
                graph.add_edge(dep.clone(), task.name.clone());
            }
        }
        
        // Get topological order
        let order = graph.topological_sort()
            .map_err(|e| BuildError::InvalidConfig(format!("Cyclic dependency: {}", e)))?;
        
        // Build in topological order
        let mut results = Vec::new();
        let mut built = BTreeSet::new();
        
        for name in order {
            let task = packages.iter().find(|t| t.name == name)
                .ok_or_else(|| BuildError::InvalidConfig(format!("Missing task for {}", name)))?;
            
            // Wait for all dependencies to be built
            for dep in &task.dependencies {
                if !built.contains(dep) {
                    return Err(BuildError::InvalidConfig(format!("Dependency {} not built", dep)));
                }
            }
            
            // Acquire semaphore permit
            let permit = self.semaphore.acquire().await
                .map_err(|_| BuildError::InvalidConfig("Semaphore closed".to_string()))?;
            
            let result = self.build_one(task).await?;
            built.insert(task.name.clone());
            results.push(result);
            
            drop(permit);
        }
        
        Ok(results)
    }

    /// Build a single package.
    async fn build_one(&self, task: &BuildTask) -> Result<BuildResult, BuildError> {
        // Check cache first
        let cache_key = task.build_key.cache_key();
        if let Some(cached) = self.cache.get(&cache_key) {
            info!("Cache hit for {}@{}", task.name, task.version);
            return Ok(cached);
        }
        
        info!("Building {}@{}", task.name, task.version);
        let start = SystemTime::now();
        
        // Execute build in sandbox
        let result = self.execute_build(&task).await?;
        
        let duration = start.elapsed().unwrap_or(Duration::from_secs(0)).as_millis() as u64;
        
        let build_result = BuildResult {
            build_key: task.build_key.clone(),
            outputs: result.outputs,
            duration_ms: duration,
            built_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            command: result.command,
            stdout: result.stdout,
            stderr: result.stderr,
            success: result.success,
            exit_code: result.exit_code,
        };
        
        // Store in cache if successful
        if build_result.success {
            let entry = BuildCacheEntry {
                build_key: task.build_key.clone(),
                result: build_result.clone(),
                size_bytes: 0, // Would calculate actual size
                last_accessed: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                access_count: 0,
            };
            self.cache.put(&task.build_key.cache_key(), entry)?;
        }
        
        Ok(build_result)
    }

    /// Execute the actual build command in a sandbox.
    async fn execute_build(&self, task: &BuildTask) -> Result<BuildExecutionResult, BuildError> {
        let script = &task.script;
        
        // Prepare build directory
        std::fs::create_dir_all(&task.build_dir).map_err(BuildError::Io)?;
        std::fs::create_dir_all(&task.output_dir).map_err(BuildError::Io)?;
        
        // Set up environment
        let mut env = task.env.clone();
        env.insert("NPM_PACKAGE_NAME".to_string(), task.name.to_string());
        env.insert("NPM_PACKAGE_VERSION".to_string(), task.version.to_string());
        env.insert("NPM_CONFIG_PREFIX".to_string(), task.output_dir.to_string_lossy().to_string());
        
        // Create sandbox
        let sandbox = Sandbox::new(&task.sandbox_policy)
            .map_err(BuildError::Sandbox)?;
        
        // Execute in sandbox
        let output = sandbox.execute(
            &task.build_dir,
            &script.command,
            &script.args,
            &env,
            task.timeout,
        ).await?;
        
        // Collect outputs
        let outputs = collect_build_outputs(&task.output_dir)?;
        
        Ok(BuildExecutionResult {
            outputs,
            command: format!("{} {}", script.command, script.args.join(" ")),
            stdout: output.stdout,
            stderr: output.stderr,
            success: output.success,
            exit_code: output.exit_code,
        })
    }
}

/// Build task for a single package.
#[derive(Debug, Clone)]
pub struct BuildTask {
    pub name: PackageName,
    pub version: Version,
    pub integrity: Integrity,
    pub build_key: BuildKey,
    pub build_dir: PathBuf,
    pub output_dir: PathBuf,
    pub script: BuildScriptConfig,
    pub sandbox_policy: SandboxPolicy,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
    pub dependencies: Vec<PackageName>,
}

/// Result of executing a build command.
#[derive(Debug)]
struct BuildExecutionResult {
    outputs: BTreeMap<String, String>,
    command: String,
    stdout: String,
    stderr: String,
    success: bool,
    exit_code: Option<i32>,
}

/// Simple dependency graph for topological sorting.
struct BuildGraph {
    nodes: BTreeMap<PackageName, BTreeSet<PackageName>>,
}

impl BuildGraph {
    fn new() -> Self {
        Self { nodes: BTreeMap::new() }
    }
    
    fn add_node(&mut self, name: PackageName) {
        self.nodes.entry(name).or_default();
    }
    
    fn add_edge(&mut self, from: PackageName, to: PackageName) {
        self.nodes.entry(from.clone()).or_default().insert(to.clone());
        self.nodes.entry(to).or_default();
    }
    
    fn topological_sort(&self) -> Result<Vec<PackageName>, String> {
        let mut in_degree: BTreeMap<PackageName, usize> = BTreeMap::new();
        let mut adj: BTreeMap<PackageName, Vec<PackageName>> = BTreeMap::new();
        
        // Initialize
        for (node, deps) in &self.nodes {
            in_degree.insert(node.clone(), 0);
            adj.entry(node.clone()).or_default();
        }
        
        // Calculate in-degrees
        for (node, deps) in &self.nodes {
            for dep in deps {
                *in_degree.get_mut(dep).unwrap() += 1;
                adj.entry(node.clone()).or_default().push(dep.clone());
            }
        }
        
        // Kahn's algorithm
        let mut queue: Vec<PackageName> = in_degree.iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(n, _)| n.clone())
            .collect();
        
        let mut result = Vec::new();
        
        while let Some(node) = queue.pop() {
            result.push(node.clone());
            
            if let Some(neighbors) = adj.get(&node) {
                for neighbor in neighbors {
                    let deg = in_degree.get_mut(neighbor).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(neighbor.clone());
                    }
                }
            }
        }
        
        if result.len() != self.nodes.len() {
            return Err("Cyclic dependency detected".to_string());
        }
        
        Ok(result)
    }
}

/// Collect build outputs from output directory.
fn collect_build_outputs(output_dir: &Path) -> Result<BTreeMap<String, String>, BuildError> {
    let mut outputs = BTreeMap::new();
    
    fn walk_dir(dir: &Path, base: &Path, outputs: &mut BTreeMap<String, String>) -> Result<(), BuildError> {
        for entry in std::fs::read_dir(dir).map_err(BuildError::Io)? {
            let entry = entry.map_err(BuildError::Io)?;
            let path = entry.path();
            let rel_path = path.strip_prefix(base)
                .map_err(|_| BuildError::InvalidConfig("Path not under base".to_string()))?
                .to_string_lossy()
                .to_string();
            
            if path.is_dir() {
                walk_dir(&path, base, outputs)?;
            } else {
                let hash = hash_file(&path)?;
                outputs.insert(rel_path, hash);
            }
        }
        Ok(())
    }
    
    walk_dir(output_dir, output_dir, &mut outputs)?;
    Ok(outputs)
}

/// Hash a file with SHA256.
fn hash_file(path: &Path) -> Result<String, BuildError> {
    let mut file = std::fs::File::open(path).map_err(BuildError::Io)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(BuildError::Io)?;
    Ok(hex::encode(hasher.finalize()))
}

/// Prebuilt binary resolution.
pub struct PrebuiltResolver;

impl PrebuiltResolver {
    /// Try to resolve a prebuilt binary for a package.
    /// Returns the build key if found, None otherwise.
    pub fn resolve(
        package_name: &PackageName,
        version: &Version,
        integrity: &Integrity,
        node_abi: &str,
        platform: &BuildPlatform,
    ) -> Option<BuildKey> {
        // Check known prebuild patterns
        // This would query npm registry for prebuild metadata
        // For now, return None to force local build
        None
    }
    
    /// Check if a package has prebuilt binaries available.
    pub fn has_prebuilt(package_name: &PackageName, version: &Version) -> bool {
        // Would check registry for prebuild metadata
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsm_core::{PackageName, Version, Integrity};
    
    #[test]
    fn test_build_key_computation() {
        let integrity = Integrity::new("sha512-abc123".to_string()).unwrap();
        let platform = BuildPlatform {
            os: "linux".to_string(),
            cpu: "x64".to_string(),
            libc: "glibc".to_string(),
        };
        
        let key = BuildKey::compute(
            &integrity,
            "script-hash",
            "108",
            platform,
            "toolchain-fp",
            BTreeMap::new(),
            BTreeSet::new(),
            &SandboxPolicy::default(),
        );
        
        assert_eq!(key.package_integrity, "sha512-abc123");
        assert_eq!(key.node_abi, "108");
        assert!(!key.cache_key().is_empty());
    }
    
    #[test]
    fn test_build_graph_topological_sort() {
        let mut graph = BuildGraph::new();
        let a = PackageName::new("a").unwrap();
        let b = PackageName::new("b").unwrap();
        let c = PackageName::new("c").unwrap();
        
        graph.add_node(a.clone());
        graph.add_node(b.clone());
        graph.add_node(c.clone());
        graph.add_edge(a.clone(), b.clone());
        graph.add_edge(b.clone(), c.clone());
        
        let order = graph.topological_sort().unwrap();
        assert_eq!(order.len(), 3);
        // a must come before b, b before c
        let a_idx = order.iter().position(|n| *n == a).unwrap();
        let b_idx = order.iter().position(|n| *n == b).unwrap();
        let c_idx = order.iter().position(|n| *n == c).unwrap();
        assert!(a_idx < b_idx);
        assert!(b_idx < c_idx);
    }
    
    #[test]
    fn test_compute_script_hash() {
        use jsm_store::PackageManifest;
        use serde_json::json;
        
        let manifest = PackageManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            integrity: "sha512-abc".to_string(),
            entries: vec![],
            package_json: Some(json!({
                "scripts": {
                    "install": "node-gyp rebuild",
                    "prepare": "npm run build"
                }
            })),
        };
        
        let hash = compute_script_hash(&manifest);
        assert!(!hash.is_empty());
    }
}