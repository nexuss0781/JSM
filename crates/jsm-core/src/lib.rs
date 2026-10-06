//! Shared, validated vocabulary for `jsm` crates.
//!
//! Serde representations in this Phase 0 scaffold are provisional. They do not
//! freeze lockfile, store, CLI JSON, or external compatibility contracts.

mod error;
mod metrics;
mod redact;
mod types;

pub use error::{ErrorCause, ErrorCode, JsmError};
pub use metrics::{MetricSink, NoopMetrics};
pub use redact::redact;
pub use types::{
    DependencySpec, DistTag, Integrity, Libc, NodeAbi, PackageId, PackageName, Platform,
    PlatformToken, Range, Spec, ValidationError, Version,
};
