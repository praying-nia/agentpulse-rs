//! Native Channel health and bounded monitoring state.

use std::{net::SocketAddr, sync::MutexGuard};

/// Current Native Channel health.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeChannelHealth {
    /// The Source is not running.
    #[default]
    Stopped,
    /// The loopback listener is accepting a client.
    Listening,
    /// One client completed the application handshake.
    Connected,
    /// The listener or worker terminated and requires explicit restart.
    Failed,
}

/// Atomic point-in-time Native Channel monitoring data.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeChannelSnapshot {
    /// Current lifecycle and connection health.
    pub health: NativeChannelHealth,
    /// Actual bound loopback address while the Source is running.
    pub local_address: Option<SocketAddr>,
    /// Self-declared active client UUIDv7, when connected.
    pub client_id: Option<String>,
    /// Number of successful application handshakes.
    pub connections: u64,
    /// Number of completed discovery snapshots.
    pub discoveries: u64,
    /// Number of Session subscriptions established.
    pub subscriptions: u64,
    /// Number of domain frames accepted by the output queue.
    pub domain_frames: u64,
    /// Number of text frames written to clients.
    pub frames_sent: u64,
    /// Number of text frames received from clients.
    pub frames_received: u64,
    /// Number of client connections ended for any reason.
    pub disconnects: u64,
    /// Last bounded connection or worker diagnostic.
    pub last_error: Option<String>,
}

pub(crate) type SharedStatus = std::sync::Arc<std::sync::Mutex<NativeChannelSnapshot>>;

pub(crate) fn lock_status(status: &SharedStatus) -> MutexGuard<'_, NativeChannelSnapshot> {
    match status.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
