use std::error::Error;

use serde::{Deserialize, Serialize};

use crate::redact;
use thiserror::Error;

/// Stable machine-readable error family from `SPECS.md` Appendix C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    #[serde(rename = "JSM_E_GENERAL")]
    General,
    #[serde(rename = "JSM_E_USAGE")]
    Usage,
    #[serde(rename = "JSM_E_RESOLVE")]
    Resolve,
    #[serde(rename = "JSM_E_NETWORK")]
    Network,
    #[serde(rename = "JSM_E_INTEGRITY")]
    Integrity,
    #[serde(rename = "JSM_E_SECURITY")]
    Security,
    #[serde(rename = "JSM_E_LOCKFILE")]
    Lockfile,
    #[serde(rename = "JSM_E_SCRIPT")]
    Script,
    #[serde(rename = "JSM_E_STORE")]
    Store,
    #[serde(rename = "JSM_E_INTERRUPTED")]
    Interrupted,
    #[serde(rename = "JSM_E_AUDIT")]
    Audit,
}

impl ErrorCode {
    pub const ALL: [Self; 11] = [
        Self::General,
        Self::Usage,
        Self::Resolve,
        Self::Network,
        Self::Integrity,
        Self::Security,
        Self::Lockfile,
        Self::Script,
        Self::Store,
        Self::Interrupted,
        Self::Audit,
    ];

    /// Map a failure family to the CLI exit code specified in Appendix C.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::General => 1,
            Self::Usage => 2,
            Self::Resolve => 3,
            Self::Network => 4,
            Self::Integrity | Self::Security => 5,
            Self::Lockfile => 6,
            Self::Script => 7,
            Self::Store => 8,
            Self::Interrupted => 9,
            Self::Audit => 10,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::General => "JSM_E_GENERAL",
            Self::Usage => "JSM_E_USAGE",
            Self::Resolve => "JSM_E_RESOLVE",
            Self::Network => "JSM_E_NETWORK",
            Self::Integrity => "JSM_E_INTEGRITY",
            Self::Security => "JSM_E_SECURITY",
            Self::Lockfile => "JSM_E_LOCKFILE",
            Self::Script => "JSM_E_SCRIPT",
            Self::Store => "JSM_E_STORE",
            Self::Interrupted => "JSM_E_INTERRUPTED",
            Self::Audit => "JSM_E_AUDIT",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A cloneable error-chain node suitable for carrying external cause messages.
#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct ErrorCause {
    message: String,
    #[source]
    source: Option<Box<ErrorCause>>,
}

impl ErrorCause {
    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn from_error(error: &(dyn Error + 'static)) -> Self {
        let source = error
            .source()
            .map(|cause| Box::new(Self::from_error(cause)));
        Self {
            message: redact(&error.to_string()),
            source,
        }
    }
}

/// Structured failure shared by libraries; CLI rendering is owned by `jsm-cli`.
#[derive(Debug, Clone, Error)]
#[error("{code}: {message}")]
pub struct JsmError {
    code: ErrorCode,
    message: String,
    help: Option<String>,
    #[source]
    cause: Option<ErrorCause>,
}

impl JsmError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: redact(&message.into()),
            help: None,
            cause: None,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(redact(&help.into()));
        self
    }

    pub fn with_cause(mut self, cause: ErrorCause) -> Self {
        self.cause = Some(cause);
        self
    }

    pub fn caused_by<E>(code: ErrorCode, message: impl Into<String>, cause: &E) -> Self
    where
        E: Error + 'static,
    {
        Self::new(code, message).with_cause(ErrorCause::from_error(cause))
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn help(&self) -> Option<&str> {
        self.help.as_deref()
    }

    pub fn cause_node(&self) -> Option<&ErrorCause> {
        self.cause.as_ref()
    }

    pub fn exit_code(&self) -> u8 {
        self.code.exit_code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_taxonomy_snapshot_matches_documented_codes_and_exit_statuses() {
        let actual = ErrorCode::ALL
            .iter()
            .map(|code| format!("{}={}", code.as_str(), code.exit_code()))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        assert_eq!(
            actual,
            include_str!("../tests/snapshots/error_taxonomy.snap").replace("\r\n", "\n")
        );
    }

    #[test]
    fn errors_keep_help_and_cause_chain() {
        let io_error = std::io::Error::other("disk unavailable");
        let error = JsmError::caused_by(ErrorCode::Store, "cannot write package", &io_error)
            .with_help("check free space");
        assert_eq!(error.exit_code(), 8);
        assert_eq!(error.help(), Some("check free space"));
        assert_eq!(error.source().unwrap().to_string(), "disk unavailable");

        let sensitive = JsmError::new(ErrorCode::Network, "access_token=must-not-leak")
            .with_help("set password=also-secret");
        assert!(!sensitive.to_string().contains("must-not-leak"));
        assert!(!sensitive.help().unwrap().contains("also-secret"));
    }
}
