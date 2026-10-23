use std::{error::Error, fmt::Display};

#[derive(Debug)]
pub enum WalError {
    BadIdentifier,
    UnsupportedVersion(u16),
    Corrupted,
    SmallEncodeBuffer,
    UnknownType((u32, u8)),
}

impl Display for WalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalError::BadIdentifier => write!(f, "Identifier of Write-Ahead-Log not recognized."),
            WalError::UnsupportedVersion(v) => {
                write!(f, "The version '{v}' of Write-Ahead-Log is not supported")
            }
            WalError::Corrupted => write!(f, "The Write-Ahead-Log file is corrupted"),
            WalError::UnknownType((_, t)) => write!(f, "An entry with type {t} cannot be parsed"),
            WalError::SmallEncodeBuffer => write!(f, "Encode buffer too small"),
        }
    }
}

impl Error for WalError {}
