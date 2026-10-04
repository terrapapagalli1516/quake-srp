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
    Truncated { context: &'static str, need: usize, have: usize },
    /// A magic / identification field did not match the expected value.
    BadMagic { context: &'static str, found: [u8; 4], expected: &'static str },
    /// A structurally invalid value (bad count, version, offset, …).
    Invalid(String),
    /// A QuakeC runtime error: id's `Host_Error ("Program error")`, which
    /// ends the game.
    Program(Box<ProgramError>),
}

/// A QuakeC runtime error — `PR_RunError` (a VM fault, a bad builtin call),
/// or the `error`/`objerror` builtins — which id's host answers with
/// `Host_Error ("Program error")`: the server shuts down, the client
/// disconnects, and the console comes down with what was printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramError {
    /// The QuakeC function that was running (`pr_xfunction`).
    pub function: String,
    /// The error: `PR_RunError`'s message, or the text `error()` was given.
    pub message: String,
    /// Everything id printed to the console on the way to `Host_Error`: for
    /// `PR_RunError` the failing statement, the stack trace and the message;
    /// for `error`/`objerror` their banner and `ED_Print (self)`.
    pub console: String,
}

impl From<ProgramError> for QError {
    fn from(e: ProgramError) -> Self {
        QError::Program(Box::new(e))
    }
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
            QError::Truncated { context, need, have } => {
                write!(f, "truncated while reading {context}: need {need} byte(s), {have} available")
            }
            QError::BadMagic { context, found, expected } => {
                write!(f, "bad magic in {context}: found {found:02x?}, expected {expected:?}")
            }
            QError::Invalid(msg) => write!(f, "invalid data: {msg}"),
            QError::Program(e) => write!(f, "program error in {}(): {}", e.function, e.message),
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
