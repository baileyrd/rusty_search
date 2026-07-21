use thiserror::Error;

/// The result type returned by every [`crate::SearchBackend`] operation.
pub type Result<T> = std::result::Result<T, SearchError>;

/// Errors that can occur while talking to a search backend.
///
/// Backends map their own error types onto these variants so callers can
/// handle failures generically, regardless of which engine is plugged in.
#[derive(Debug, Error)]
pub enum SearchError {
    #[error("index `{0}` not found")]
    IndexNotFound(String),

    #[error("index `{0}` already exists")]
    IndexAlreadyExists(String),

    #[error("document `{0}` not found in index `{1}`")]
    DocumentNotFound(String, String),

    #[error("invalid schema: {0}")]
    InvalidSchema(String),

    #[error("invalid query: {0}")]
    InvalidQuery(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Catch-all for backend-specific failures (I/O, network, the engine's
    /// own error type, etc). Backends should prefer the typed variants above
    /// when the failure maps cleanly onto one.
    #[error("backend error: {0}")]
    Backend(#[from] anyhow::Error),
}
