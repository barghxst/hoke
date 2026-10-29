use crate::container::CodecError;
use std::{fmt, io};

#[derive(Debug)]
pub enum JournalError {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Codec(CodecError),
    Corruption(&'static str),
    Limit(&'static str),
    Poisoned,
}
impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Codec(error) => error.fmt(f),
            Self::Corruption(reason) => write!(f, "corrupt container: {reason}"),
            Self::Limit(reason) => write!(f, "container limit: {reason}"),
            Self::Poisoned => {
                f.write_str("container outcome unknown; reopen before further operations")
            }
        }
    }
}
impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Codec(error) => Some(error),
            _ => None,
        }
    }
}
impl From<CodecError> for JournalError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JournalRecovery {
    pub discarded_tail_bytes: u64,
    pub records_scanned: u64,
}
