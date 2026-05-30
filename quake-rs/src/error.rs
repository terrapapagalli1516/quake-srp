//! The crate-wide error type.
//!
//! The original C engine called `Sys_Error(...)` (which `longjmp`s back to the
//! host loop or aborts) on any malformed input. We return a structured error
//! instead so loaders compose and callers can recover.

use std::fmt;

#[derive(Debug)]
pub enum QError {
    /// An underlying I/O failure (file open/read/seek).
    Io(std::io::Error),
    /// Ran off the end of the buffer while decoding `context`.
    Truncated {
        context: &'static str,
        need: usize,
        have: usize,
    },
    /// A magic / identification field did not match the expected value.
    BadMagic {
        context: &'static str,
        found: [u8; 4],
        expected: &'static str,
    },
    /// A structurally invalid value (bad count, version, offset, …).
    Invalid(String),
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, QError>;

impl QError {
    /// Convenience constructor for [`QError::Invalid`].
    pub fn invalid(msg: impl Into<String>) -> Self {
        QError::Invalid(msg.into())
    }
}

impl fmt::Display for QError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QError::Io(e) => write!(f, "io error: {e}"),
            QError::Truncated {
                context,
                need,
                have,
            } => write!(
                f,
                "truncated while reading {context}: need {need} byte(s), {have} available"
            ),
            QError::BadMagic {
                context,
                found,
                expected,
            } => write!(
                f,
                "bad magic in {context}: found {found:02x?}, expected {expected:?}"
            ),
            QError::Invalid(msg) => write!(f, "invalid data: {msg}"),
        }
    }
}

impl std::error::Error for QError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            QError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for QError {
    fn from(e: std::io::Error) -> Self {
        QError::Io(e)
    }
}
