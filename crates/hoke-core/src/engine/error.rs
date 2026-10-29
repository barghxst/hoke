use std::{fmt, io};

#[derive(Debug)]
pub enum Error {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Corruption {
        offset: u64,
        reason: String,
    },
    UnsupportedVersion(u32),
    InvalidInput(String),
    AllocationFailed,
    Poisoned,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Corruption { offset, reason } => {
                write!(f, "corrupt WAL at byte {offset}: {reason}")
            }
            Self::UnsupportedVersion(version) => write!(f, "unsupported WAL version: {version}"),
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
            Self::AllocationFailed => write!(f, "could not allocate a bounded WAL buffer"),
            Self::Poisoned => write!(
                f,
                "WAL write outcome is unknown; close and reopen before writing"
            ),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(super) fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> Error {
    move |source| Error::Io { operation, source }
}
pub(super) fn corrupt(offset: u64, error: impl fmt::Display) -> Error {
    Error::Corruption {
        offset,
        reason: error.to_string(),
    }
}
