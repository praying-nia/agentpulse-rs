//! Pairing failure model.

use std::{io, path::PathBuf};

use thiserror::Error;

/// Failure while storing credentials or serving one pairing session.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PairingError {
    /// A required setting or protocol field was invalid.
    #[error("invalid pairing field {field}: {reason}")]
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Bounded diagnostic.
        reason: String,
    },
    /// Strict JSON parsing or encoding failed.
    #[error("invalid pairing JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Base64 data was malformed.
    #[error("invalid pairing base64: {0}")]
    Base64(#[from] base64::DecodeError),
    /// Certificate generation or parsing failed.
    #[error("pairing certificate operation failed: {0}")]
    Certificate(#[from] rcgen::Error),
    /// The bounded transport failed.
    #[error("pairing transport failed: {0}")]
    Transport(#[from] agentpulse_transport::LoopbackWebSocketError),
    /// A file-system operation failed.
    #[error("failed to {operation} {path}: {source}")]
    Io {
        /// Operation name.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: io::Error,
    },
    /// Initialization would replace an existing identity.
    #[error("AgentPulse credentials already exist at {path}")]
    AlreadyInitialized {
        /// Existing store path.
        path: PathBuf,
    },
    /// An operation requires an initialized store.
    #[error("AgentPulse credentials are not initialized at {path}")]
    NotInitialized {
        /// Missing store path.
        path: PathBuf,
    },
    /// The persisted schema is unsupported or inconsistent.
    #[error("credential store is invalid: {message}")]
    InvalidStore {
        /// Validation diagnostic.
        message: String,
    },
    /// The maximum paired-device count was reached.
    #[error("paired-device capacity of {capacity} has been reached")]
    DeviceCapacity {
        /// Configured capacity.
        capacity: usize,
    },
    /// The requested device does not exist.
    #[error("paired device {client_id} was not found")]
    DeviceNotFound {
        /// Requested client identity.
        client_id: String,
    },
    /// The one-time pairing session expired.
    #[error("pairing session expired")]
    Expired,
    /// The user denied the device.
    #[error("pairing request was denied")]
    Denied,
    /// Too many invalid attempts were made.
    #[error("pairing attempt limit was reached")]
    AttemptLimit,
}

impl PairingError {
    pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}
