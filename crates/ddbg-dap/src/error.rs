use thiserror::Error;

/// Errors produced while framing/parsing DAP messages.
#[derive(Debug, Error)]
pub enum CodecError {
    #[error("invalid DAP header")]
    InvalidHeader,
    #[error("DAP header missing Content-Length")]
    MissingContentLength,
    #[error("DAP header too long")]
    HeaderTooLong,
    #[error("invalid DAP JSON: {0}")]
    Json(#[source] serde_json::Error),
}

/// Errors produced by the DAP client.
#[derive(Debug, Error)]
pub enum DapError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("failed to spawn adapter `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    /// The adapter answered with `success: false`.
    #[error("{command} failed: {message}")]
    Adapter { command: String, message: String },
    #[error("unexpected response body for {command}: {source}")]
    Body {
        command: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("debug adapter connection closed")]
    ChannelClosed,
    #[error("request {0} timed out")]
    Timeout(String),
}

pub type Result<T, E = DapError> = std::result::Result<T, E>;
