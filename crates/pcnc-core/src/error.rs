use thiserror::Error;

/// Errors the engine reports to API and CLI callers.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The request is malformed (bad name, unknown shared mailbox, …).
    #[error("{code}: {message}")]
    BadRequest { code: &'static str, message: String },
    /// The identity does not exist in the registry.
    #[error("identity not found")]
    NotFound,
    /// The request conflicts with the current state (collision, wrong lifecycle state, …).
    #[error("{code}: {message}")]
    Conflict { code: &'static str, message: String },
    /// A backend (mail server, Nextcloud, registry) failed.
    #[error(transparent)]
    Backend(#[from] anyhow::Error),
}

impl EngineError {
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::BadRequest {
            code,
            message: message.into(),
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            code,
            message: message.into(),
        }
    }

    /// Stable machine-readable code for API responses.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest { code, .. } | Self::Conflict { code, .. } => code,
            Self::NotFound => "not_found",
            Self::Backend(_) => "backend_error",
        }
    }
}
