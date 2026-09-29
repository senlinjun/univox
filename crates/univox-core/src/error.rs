//! Unified error classification (FEATURES.md §12).

use thiserror::Error;

/// Unified error across all platform drivers. The raw platform error is
/// preserved in [`Error::Platform`].
#[derive(Debug, Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("permission denied: {missing}")]
    Permission { missing: String },
    #[error("rate limited (retry after {retry_after:?})")]
    RateLimited { retry_after: std::time::Duration },
    #[error("server flood protection: {0}")]
    Flood(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("audio error: {0}")]
    Audio(String),
    #[error("file transfer error: {0}")]
    FileTransfer(String),
    #[error("timeout")]
    Timeout,
    #[error("session closed")]
    Closed,
    #[error("unsupported by this platform/driver: {0}")]
    Unsupported(String),
    /// Raw platform error, preserved verbatim.
    #[error("platform error: {platform} {code}: {message}")]
    Platform {
        platform: crate::platform::Platform,
        code: i32,
        message: String,
    },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Wrap any displayable error into [`Error::Other`].
    pub fn other(e: impl std::fmt::Display) -> Self {
        Error::Other(e.to_string())
    }

    /// Server error id if this is a [`Error::Platform`].
    pub fn server_id(&self) -> Option<i32> {
        match self {
            Error::Platform { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn is_server_err(&self, id: i32) -> bool {
        self.server_id() == Some(id)
    }

    /// Retryable errors: network hiccups, timeouts, rate limits, floods.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Network(_) | Error::Timeout | Error::RateLimited { .. } | Error::Flood(_)
        )
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Network(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(Error::Timeout.is_retryable());
        assert!(Error::RateLimited {
            retry_after: std::time::Duration::from_secs(1)
        }
        .is_retryable());
        assert!(!Error::Auth("bad password".into()).is_retryable());
        assert!(!Error::Permission {
            missing: "b_admin".into()
        }
        .is_retryable());
        assert_eq!(
            Error::Platform {
                platform: crate::platform::Platform::Ts3,
                code: 520,
                message: "invalid login".into()
            }
            .server_id(),
            Some(520)
        );
    }
}
