//! Public Codex Provider error types.

use std::{path::PathBuf, time::Duration};

use agentpulse_core::DomainError;
use thiserror::Error;

/// An error raised while validating configuration or building a Codex Provider.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CodexProviderBuildError {
    /// No Codex thread was configured.
    #[error("at least one Codex thread ID is required")]
    EmptyThreadList,
    /// A configured Codex thread ID was repeated.
    #[error("Codex thread ID {thread_id} is configured more than once")]
    DuplicateThreadId {
        /// The duplicate thread ID.
        thread_id: String,
    },
    /// A configured Codex thread ID was not a UUIDv7.
    #[error("invalid Codex thread ID {thread_id}: {source}")]
    InvalidThreadId {
        /// The invalid thread ID.
        thread_id: String,
        /// The identifier validation failure.
        #[source]
        source: DomainError,
    },
    /// The process working directory could not be resolved.
    #[error("failed to resolve the current directory: {message}")]
    CurrentDirectory {
        /// The operating-system error text.
        message: String,
    },
    /// The runtime root cannot be represented in the App Server URI.
    #[error("runtime root is not valid UTF-8: {path:?}")]
    NonUtf8RuntimeRoot {
        /// The invalid path.
        path: PathBuf,
    },
    /// The resulting Unix socket path exceeds the portable length limit.
    #[error("Codex App Server socket path is too long ({length} bytes; maximum {maximum})")]
    SocketPathTooLong {
        /// The encoded socket path length.
        length: usize,
        /// The enforced portable maximum.
        maximum: usize,
    },
    /// A timeout was configured as zero.
    #[error("{field} must be greater than zero")]
    ZeroTimeout {
        /// The invalid timeout field.
        field: &'static str,
    },
    /// The Codex executable path was empty.
    #[error("Codex executable path must not be empty")]
    EmptyExecutable,
    /// A Core domain value needed by the Provider could not be constructed.
    #[error("failed to construct Codex Provider domain metadata: {0}")]
    Domain(#[from] DomainError),
    /// The bundled official protocol schema could not be compiled.
    #[error("failed to compile bundled Codex App Server schema: {message}")]
    Schema {
        /// The schema compiler error.
        message: String,
    },
}

/// An error returned when a write action is sent to the read-only Provider.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum CodexProviderPortError {
    /// Interaction responses are outside the Provider capability boundary.
    #[error("the Codex Provider is read-only and cannot accept interaction responses")]
    ReadOnlyInteractionResponse,
    /// Agent commands are outside the Provider capability boundary.
    #[error("the Codex Provider is read-only and cannot accept agent commands")]
    ReadOnlyCommand,
}

/// A lifecycle or live-stream failure raised by the Codex Provider Source.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CodexProviderSourceError {
    /// The same Source instance was started twice without a completed stop.
    #[error("Codex Provider Source is already started")]
    AlreadyStarted,
    /// Managed Unix App Server operation is unavailable on this platform.
    #[error("managed Codex App Server Unix sockets are unsupported on this platform")]
    UnsupportedPlatform,
    /// The Codex version probe failed.
    #[error("failed to query Codex version: {message}")]
    VersionProbe {
        /// The process or output failure.
        message: String,
    },
    /// The installed Codex version is neither verified nor newer than the schema baseline.
    #[error("unsupported Codex version {actual}; expected {expected}")]
    VersionMismatch {
        /// The accepted version policy.
        expected: &'static str,
        /// The observed version output.
        actual: String,
    },
    /// The Provider-owned runtime directory already exists.
    #[error("Codex runtime path is already occupied: {path:?}")]
    RuntimePathOccupied {
        /// The path the Provider refused to overwrite.
        path: PathBuf,
    },
    /// A filesystem or process operation failed.
    #[error("Codex runtime {operation} failed: {message}")]
    Runtime {
        /// The failed operation.
        operation: &'static str,
        /// The operating-system failure.
        message: String,
    },
    /// App Server readiness exceeded the configured deadline.
    #[error("Codex App Server did not become ready within {timeout:?}")]
    StartupTimeout {
        /// The configured deadline.
        timeout: Duration,
    },
    /// App Server exited before a usable connection was established.
    #[error("Codex App Server exited during startup ({status}): {stderr}")]
    ProcessExited {
        /// The process exit status.
        status: String,
        /// A bounded stderr diagnostic.
        stderr: String,
    },
    /// The Unix WebSocket transport failed.
    #[error("Codex App Server transport failed: {message}")]
    Transport {
        /// The transport failure without raw protocol payloads.
        message: String,
    },
    /// Strict App Server protocol validation or correlation failed.
    #[error("Codex App Server protocol failed: {message}")]
    Protocol {
        /// The protocol failure without raw protocol payloads.
        message: String,
    },
    /// One or more explicitly configured threads could not be resumed.
    #[error("failed to resume configured Codex threads: {failures}")]
    ThreadResume {
        /// The ordered per-thread failure summary.
        failures: String,
    },
    /// A normalized event could not be handed to RuntimeHost.
    #[error("Codex event ingress failed: {message}")]
    EventIngress {
        /// The Bridge ingress failure.
        message: String,
    },
    /// The live reader thread panicked.
    #[error("Codex Provider worker terminated unexpectedly")]
    WorkerPanicked,
    /// Source shutdown could not release every owned resource.
    #[error("Codex Provider shutdown failed: {message}")]
    Shutdown {
        /// The ordered cleanup failure summary.
        message: String,
    },
}

impl CodexProviderSourceError {
    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol {
            message: message.into(),
        }
    }

    pub(crate) fn transport(message: impl Into<String>) -> Self {
        Self::Transport {
            message: message.into(),
        }
    }

    pub(crate) fn runtime(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Runtime {
            operation,
            message: error.to_string(),
        }
    }
}
