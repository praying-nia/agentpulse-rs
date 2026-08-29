//! Observable Provider health and counters.

use std::sync::{Arc, Mutex, MutexGuard};

/// Current liveness of the managed Codex Provider connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CodexProviderHealth {
    /// No managed App Server is running.
    Stopped,
    /// Process startup and protocol initialization are in progress.
    Starting,
    /// All configured threads are subscribed and the live reader is active.
    Running,
    /// The Provider encountered a terminal startup or live-stream failure.
    Failed,
}

/// A point-in-time, thread-safe Codex Provider status view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexProviderSnapshot {
    health: CodexProviderHealth,
    last_error: Option<String>,
    validated_frames: u64,
    mapped_events: u64,
    validated_unmapped_frames: u64,
    rejected_server_requests: u64,
    channel_delivery_failures: u64,
}

impl CodexProviderSnapshot {
    /// Returns current Provider liveness.
    #[must_use]
    pub const fn health(&self) -> CodexProviderHealth {
        self.health
    }

    /// Returns the latest terminal failure, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Returns the number of schema-valid inbound protocol frames.
    #[must_use]
    pub const fn validated_frames(&self) -> u64 {
        self.validated_frames
    }

    /// Returns the number of normalized events committed to the Bridge.
    #[must_use]
    pub const fn mapped_events(&self) -> u64 {
        self.mapped_events
    }

    /// Returns the number of valid frames intentionally outside the mapping boundary.
    #[must_use]
    pub const fn validated_unmapped_frames(&self) -> u64 {
        self.validated_unmapped_frames
    }

    /// Returns the number of server requests rejected by the read-only client.
    #[must_use]
    pub const fn rejected_server_requests(&self) -> u64 {
        self.rejected_server_requests
    }

    /// Returns committed events whose downstream Channel fan-out was partial.
    #[must_use]
    pub const fn channel_delivery_failures(&self) -> u64 {
        self.channel_delivery_failures
    }
}

#[derive(Debug)]
pub(crate) struct StatusRecord {
    pub(crate) health: CodexProviderHealth,
    pub(crate) last_error: Option<String>,
    pub(crate) validated_frames: u64,
    pub(crate) mapped_events: u64,
    pub(crate) validated_unmapped_frames: u64,
    pub(crate) rejected_server_requests: u64,
    pub(crate) channel_delivery_failures: u64,
}

impl Default for StatusRecord {
    fn default() -> Self {
        Self {
            health: CodexProviderHealth::Stopped,
            last_error: None,
            validated_frames: 0,
            mapped_events: 0,
            validated_unmapped_frames: 0,
            rejected_server_requests: 0,
            channel_delivery_failures: 0,
        }
    }
}

pub(crate) type SharedStatus = Arc<Mutex<StatusRecord>>;

pub(crate) fn lock_status(status: &SharedStatus) -> MutexGuard<'_, StatusRecord> {
    status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn snapshot(status: &SharedStatus) -> CodexProviderSnapshot {
    let status = lock_status(status);
    CodexProviderSnapshot {
        health: status.health,
        last_error: status.last_error.clone(),
        validated_frames: status.validated_frames,
        mapped_events: status.mapped_events,
        validated_unmapped_frames: status.validated_unmapped_frames,
        rejected_server_requests: status.rejected_server_requests,
        channel_delivery_failures: status.channel_delivery_failures,
    }
}
