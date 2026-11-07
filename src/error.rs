//! Typed failures shared across library boundaries.

use std::path::PathBuf;

/// Recoverable application failure with safe, credential-free context.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation failed.
    #[error("{operation} failed for {path}: {source}")]
    Io {
        /// The attempted operation.
        operation: &'static str,
        /// The affected path, never a credential value.
        path: PathBuf,
        /// Original operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// A setting failed parsing or validation.
    #[error("invalid configuration at {path}, key {key}: {reason}")]
    Config {
        /// Configuration source.
        path: PathBuf,
        /// Setting name.
        key: String,
        /// Explanation without the rejected value.
        reason: String,
    },
}

/// Standard application result.
pub type Result<T> = std::result::Result<T, Error>;

/// Attach filesystem context without exposing file contents.
pub fn io(operation: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        operation,
        path: path.into(),
        source,
    }
}
