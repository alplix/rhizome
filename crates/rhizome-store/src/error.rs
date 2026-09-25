use std::fmt;

/// What can go wrong reading or writing the log.
#[derive(Debug)]
pub enum Error {
    /// SQLite reported an error.
    Sqlite(rusqlite::Error),
    /// The database was written by a newer version of Rhizome than this one.
    ///
    /// Opening it anyway could silently corrupt data this version does not
    /// understand, so it is refused.
    TooNew { found: i64, supported: i64 },
}

/// A specialised `Result` for store operations.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "database error: {e}"),
            Error::TooNew { found, supported } => write!(
                f,
                "the log was written by a newer version of Rhizome \
                 (schema {found}, this build understands up to {supported})"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            Error::TooNew { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Sqlite(e)
    }
}
