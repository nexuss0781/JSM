use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use jsm_core::{Integrity, MetricSink};
use rand::{RngCore, SeedableRng, rngs::StdRng};
use walkdir::WalkDir;

/// Deterministic wall-clock interface for tests.
pub trait DeterministicClock: Send + Sync {
    fn unix_time_ms(&self) -> u64;
}

/// A fixed clock that never advances.
#[derive(Debug, Clone, Copy)]
pub struct FrozenClock {
    unix_time_ms: u64,
}

impl FrozenClock {
    pub fn new(unix_time_ms: u64) -> Self {
        Self { unix_time_ms }
    }

    pub fn at_epoch() -> Self {
        Self::new(0)
    }
}

impl DeterministicClock for FrozenClock {
    fn unix_time_ms(&self) -> u64 {
        self.unix_time_ms
    }
}

/// An explicitly seeded RNG suitable for reproducible property and fixture tests.
pub struct SeededRng(StdRng);

impl SeededRng {
    pub fn new(seed: u64) -> Self {
        Self(StdRng::seed_from_u64(seed))
    }
}

impl RngCore for SeededRng {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        self.0.try_fill_bytes(dest)
    }
}

/// Per-file snapshot data used in deterministic tree assertions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileSnapshot {
    pub relative_path: String,
    pub size: u64,
    pub sha512: Option<String>,
    pub symlink_target: Option<String>,
}

pub fn filesystem_snapshot(root: impl AsRef<Path>) -> io::Result<Vec<FileSnapshot>> {
    let root = root.as_ref();
    let mut snapshot = Vec::new();
    for entry in WalkDir::new(root).follow_links(false).min_depth(1) {
        let entry = entry.map_err(io::Error::other)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            continue;
        }
        let relative_path = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_string_lossy()
            .replace('\\', "/");
        if metadata.file_type().is_symlink() {
            snapshot.push(FileSnapshot {
                relative_path,
                size: 0,
                sha512: None,
                symlink_target: Some(fs::read_link(path)?.to_string_lossy().into_owned()),
            });
        } else {
            let bytes = fs::read(path)?;
            snapshot.push(FileSnapshot {
                relative_path,
                size: metadata.len(),
                sha512: Some(Integrity::sha512(&bytes).to_string()),
                symlink_target: None,
            });
        }
    }
    snapshot.sort();
    Ok(snapshot)
}

/// An automatically cleaned temporary project directory.
pub struct TempProject {
    root: tempfile::TempDir,
}

impl TempProject {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            root: tempfile::tempdir()?,
        })
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    pub fn write_package_json(&self, content: &str) -> io::Result<PathBuf> {
        let path = self.path().join("package.json");
        fs::write(&path, content)?;
        Ok(path)
    }
}

/// An automatically cleaned temporary store directory.
pub struct TempStore {
    root: tempfile::TempDir,
}

impl TempStore {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            root: tempfile::tempdir()?,
        })
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }
}

/// Minimal command harness with an isolated working directory.
pub struct CliHarness {
    executable: PathBuf,
    working_directory: PathBuf,
}

impl CliHarness {
    pub fn new(executable: impl Into<PathBuf>, working_directory: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            working_directory: working_directory.into(),
        }
    }

    pub fn run<I, S>(&self, arguments: I) -> io::Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        Command::new(&self.executable)
            .args(arguments)
            .current_dir(&self.working_directory)
            .output()
    }
}

/// Simple thread-safe recorder implementing the core metrics interface.
#[derive(Default)]
pub struct InMemoryMetrics {
    counters: Mutex<BTreeMap<&'static str, u64>>,
    observations: Mutex<BTreeMap<&'static str, Vec<f64>>>,
}

impl InMemoryMetrics {
    pub fn counter(&self, name: &'static str) -> u64 {
        *self
            .counters
            .lock()
            .expect("metrics lock")
            .get(name)
            .unwrap_or(&0)
    }

    pub fn observations(&self, name: &'static str) -> Vec<f64> {
        self.observations
            .lock()
            .expect("metrics lock")
            .get(name)
            .cloned()
            .unwrap_or_default()
    }
}

impl MetricSink for InMemoryMetrics {
    fn increment_counter(&self, name: &'static str, amount: u64) {
        *self
            .counters
            .lock()
            .expect("metrics lock")
            .entry(name)
            .or_default() += amount;
    }

    fn observe(&self, name: &'static str, value: f64) {
        self.observations
            .lock()
            .expect("metrics lock")
            .entry(name)
            .or_default()
            .push(value);
    }
}

/// A production clock helper for benchmark timestamp metadata.
pub fn system_time_unix_ms() -> io::Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    Ok(duration.as_millis().min(u128::from(u64::MAX)) as u64)
}
