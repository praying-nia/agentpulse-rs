//! Native Channel configuration, handoff, protocol, and lifecycle errors.

use std::{io, net::SocketAddr};

use agentpulse_bridge::ChannelActionIngressError;
use agentpulse_core::{DomainError, SessionId};
use agentpulse_protocol::ProtocolError;
use agentpulse_transport::LoopbackWebSocketError;
use thiserror::Error;

/// Failure while validating or constructing a Native Channel.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NativeChannelBuildError {
    /// A Core descriptor value was invalid.
    #[error("invalid Native Channel descriptor: {0}")]
    Domain(#[from] DomainError),
    /// A timeout, capacity, or frame limit was zero or inconsistent.
    #[error("invalid Native Channel setting {field}: {reason}")]
    InvalidSetting {
        /// The invalid setting.
        field: &'static str,
        /// The validation failure.
        reason: &'static str,
    },
    /// The requested listener address was not loopback.
    #[error("Native Channel address {address} is not loopback")]
    NonLoopbackAddress {
        /// The rejected address.
        address: SocketAddr,
    },
    /// The authenticated endpoint was not bound to a private LAN address.
    #[error("Native Channel LAN address {address} is not private or link-local")]
    NonPrivateLanAddress {
        /// The rejected address.
        address: SocketAddr,
    },
}

/// Failure while a Channel Port accepts one Bridge delivery.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NativeChannelPortError {
    /// No handshaken client is available for an active subscription delivery.
    #[error("no active Native client is available")]
    NoActiveClient,
    /// The Bridge delivered a Session that the active client did not subscribe to.
    #[error("Native client is not subscribed to session {session_id}")]
    SessionNotSubscribed {
        /// The unexpected Session.
        session_id: SessionId,
    },
    /// The Channel received a future event route it cannot encode.
    #[error("Native Channel received an unsupported future event route")]
    UnsupportedEventRoute,
    /// Domain JSON encoding failed.
    #[error("failed to encode Native domain delivery: {0}")]
    Protocol(#[from] NativeProtocolError),
    /// The bounded client output queue overflowed.
    #[error("Native client output queue reached its {capacity}-frame limit")]
    QueueFull {
        /// The configured maximum queued frames.
        capacity: usize,
    },
}

/// Failure while starting, running, or stopping the Native Channel Source.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NativeChannelSourceError {
    /// The Source was started twice without a successful stop.
    #[error("Native Channel Source is already running")]
    AlreadyRunning,
    /// A previously validated configuration could not be materialized.
    #[error("Native Channel configuration failed: {0}")]
    Build(#[from] NativeChannelBuildError),
    /// The loopback WebSocket listener or connection failed.
    #[error("Native Channel transport failed: {0}")]
    Transport(#[from] LoopbackWebSocketError),
    /// A Native control message was invalid.
    #[error("Native Channel protocol failed: {0}")]
    Protocol(#[from] NativeProtocolError),
    /// RuntimeHost rejected a Channel-owned operation.
    #[error("Native Channel RuntimeHost operation failed: {0}")]
    Runtime(#[from] ChannelActionIngressError),
    /// A worker thread could not be created.
    #[error("failed to spawn Native Channel worker: {source}")]
    WorkerSpawn {
        /// The underlying thread creation failure.
        #[source]
        source: io::Error,
    },
    /// The Source worker terminated unexpectedly.
    #[error("Native Channel worker failed: {message}")]
    WorkerFailed {
        /// A bounded worker diagnostic.
        message: String,
    },
    /// The Source worker did not stop within the configured deadline.
    #[error("Native Channel worker did not stop before the deadline")]
    ShutdownTimeout,
    /// The Source worker panicked.
    #[error("Native Channel worker panicked")]
    WorkerPanicked,
}

/// Strict Native Transport v1 encoding or decoding failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NativeProtocolError {
    /// JSON syntax or strict DTO decoding failed.
    #[error("invalid Native Transport JSON: {source}")]
    Json {
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// A different Native Transport version was received.
    #[error("unsupported Native Transport version {received}; supported version is {supported}")]
    UnsupportedVersion {
        /// The received version.
        received: u64,
        /// The supported version.
        supported: u16,
    },
    /// A control field violated the Native contract.
    #[error("invalid Native Transport field {field}: {reason}")]
    InvalidField {
        /// The stable field name.
        field: &'static str,
        /// A human-readable validation failure.
        reason: String,
    },
    /// A nested AgentPulse JSON v1 domain envelope was invalid.
    #[error("invalid nested AgentPulse domain message: {source}")]
    DomainProtocol {
        /// The existing domain protocol failure.
        #[source]
        source: ProtocolError,
    },
    /// A nested domain type was invalid for its delivery context.
    #[error("domain message {actual} is invalid for Native delivery context {context}")]
    InvalidDomainContext {
        /// The Native delivery context.
        context: &'static str,
        /// The nested domain message type.
        actual: &'static str,
    },
}

impl From<ProtocolError> for NativeProtocolError {
    fn from(source: ProtocolError) -> Self {
        Self::DomainProtocol { source }
    }
}
