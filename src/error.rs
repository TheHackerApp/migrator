use sqlx::migrate::MigrateError;
use std::{
    fmt::{Display, Formatter},
    io,
};

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

/// Possible errors that can occur while running migrations
#[derive(Debug)]
pub enum Error {
    /// Error while executing migrations
    Execute(sqlx::Error),
    /// Error while reading from migration source
    Source(io::Error),
    /// The migration was previously applied but is missing in the resolved migrations
    MissingPreviouslyAppliedVersion(i64),
    /// The migration was previously applied but has been modified
    VersionMismatch(i64),
    /// The specified migration could not be found in the resolved migrations
    UnknownVersion(i64),
    /// The target migration is newer than the latest migration
    VersionTooNew { target: i64, latest: i64 },
    /// The migration is partially applied
    Dirty(i64),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Execute(_) => write!(f, "error while executing migrations"),
            Self::Source(_) => write!(f, "error while reading from migration source"),
            Self::MissingPreviouslyAppliedVersion(version) => write!(f, "migration {version} was previously applied, but is missing in the resolved migrations"),
            Self::VersionMismatch(version) => write!(f, "migration {version} was previously applied, but has been modified"),
            Self::UnknownVersion(version) => write!(f, "migration {version} does not exist in the resolved migrations"),
            Self::VersionTooNew {target, latest} => write!(f, "migration {target} is newer than the latest applied migration {latest}"),
            Self::Dirty(version) => write!(f, "migration {version} is partially applied"), 
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Execute(e) => Some(e),
            Self::Source(e) => Some(e),
            _ => None,
        }
    }
}

impl From<MigrateError> for Error {
    fn from(e: MigrateError) -> Self {
        match e {
            MigrateError::Execute(e) => Self::Execute(e),
            MigrateError::VersionMissing(v) => Self::MissingPreviouslyAppliedVersion(v),
            MigrateError::VersionMismatch(v) => Self::VersionMismatch(v),
            MigrateError::Dirty(v) => Self::Dirty(v),
            // We'll never encounter the other errors as we implement the same functionality ourselves
            _ => unreachable!(),
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Self::Execute(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Source(e)
    }
}
