//! Protocol encoding, decoding, and validation failures.

use agentpulse_core::DomainError;
use thiserror::Error;

/// An error produced while converting between JSON v1 and the domain model.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProtocolError {
    /// A validated message could not be serialized as JSON.
    #[error("failed to encode protocol JSON: {source}")]
    JsonEncode {
        /// The underlying JSON serialization error.
        #[source]
        source: serde_json::Error,
    },

    /// Input was not valid JSON v1 structure.
    #[error("failed to decode protocol JSON: {source}")]
    JsonDecode {
        /// The underlying JSON deserialization error.
        #[source]
        source: serde_json::Error,
    },

    /// The envelope selected a protocol version unsupported by this codec.
    #[error("unsupported protocol version {received}; supported version is {supported}")]
    UnsupportedProtocolVersion {
        /// The version found in the input envelope.
        received: u64,
        /// The version implemented by this codec.
        supported: u16,
    },

    /// A scalar or collection violated the JSON v1 wire contract.
    #[error("invalid protocol field {field}: {reason}")]
    InvalidWireValue {
        /// The stable semantic field name.
        field: &'static str,
        /// A human-readable validation reason.
        reason: String,
    },

    /// Decoded data violated a Core domain invariant.
    #[error("decoded protocol value violates the domain model: {source}")]
    Domain {
        /// The domain validation failure.
        #[source]
        source: DomainError,
    },

    /// A future non-exhaustive Core variant has no JSON v1 representation.
    #[error("{type_name} contains a variant unsupported by protocol v1")]
    UnsupportedDomainVariant {
        /// The Core enum that cannot be represented.
        type_name: &'static str,
    },
}

impl From<DomainError> for ProtocolError {
    fn from(source: DomainError) -> Self {
        Self::Domain { source }
    }
}
