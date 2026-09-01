//! Relay failure model.

use std::io;

use thiserror::Error;

/// Failure while configuring, authenticating, or operating a Relay connection.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RelayError {
    /// A configuration or protocol field was invalid.
    #[error("invalid Relay field {field}: {reason}")]
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Bounded diagnostic.
        reason: String,
    },
    /// Strict JSON encoding or decoding failed.
    #[error("invalid Relay JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Base64 data was malformed.
    #[error("invalid Relay base64: {0}")]
    Base64(#[from] base64::DecodeError),
    /// A bounded I/O operation failed.
    #[error("Relay I/O failed while {operation}: {source}")]
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// TLS setup or negotiation failed.
    #[error("Relay TLS failed: {message}")]
    Tls {
        /// TLS diagnostic without credential material.
        message: String,
    },
    /// Authentication failed without revealing which credential differed.
    #[error("Relay authentication failed")]
    Authentication,
    /// The peer violated the Relay state machine.
    #[error("Relay protocol failed: {message}")]
    Protocol {
        /// Protocol diagnostic.
        message: String,
    },
    /// A bounded deadline elapsed.
    #[error("Relay timed out while {operation}")]
    Timeout {
        /// Operation that exceeded its deadline.
        operation: &'static str,
    },
    /// The requested Host is not registered with the Relay.
    #[error("Relay Host is unavailable")]
    HostUnavailable,
    /// Another Host connection already owns an overlapping route or capacity.
    #[error("Relay Host registration is already occupied")]
    HostBusy,
    /// The paired-device route set changed while this Host was waiting.
    #[error("Relay route registration changed")]
    RoutesChanged,
    /// The Relay or Host connector was asked to stop.
    #[error("Relay stopped")]
    Stopped,
}

impl RelayError {
    pub(crate) fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }

    pub(crate) fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidField {
            field,
            reason: reason.into(),
        }
    }
}
