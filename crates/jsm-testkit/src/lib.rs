//! Hermetic fixtures and helpers for repository and end-to-end tests.

mod fixture;
mod harness;
mod install;
mod registry;

pub use fixture::{FixtureArtifact, FixturePackage, validate_relative_archive_path};
pub use harness::{
    CliHarness, DeterministicClock, FileSnapshot, FrozenClock, InMemoryMetrics, SeededRng,
    TempProject, TempStore, filesystem_snapshot, system_time_unix_ms,
};
pub use install::install_fixture_from_registry;
pub use registry::{FakeRegistry, RegistryBehavior};
