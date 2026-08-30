//! Bounded transport primitives used by concrete AgentPulse adapters.
//!
//! Loopback remains the safe default. A separate TLS listener supports explicit
//! private-network binding and application bearer authorization for native
//! clients without weakening that default.

mod lan_tls_websocket;
mod loopback_websocket;

pub use lan_tls_websocket::{
    BearerTokenAuthorizer, TlsServerIdentity, TlsWebSocket, TlsWebSocketConfig,
    TlsWebSocketListener,
};
pub use loopback_websocket::{
    DEFAULT_MAX_MESSAGE_BYTES, LoopbackWebSocket, LoopbackWebSocketConfig, LoopbackWebSocketError,
    LoopbackWebSocketListener, TransportRead,
};

/// Backward-compatible common error for bounded WebSocket transports.
pub type WebSocketTransportError = LoopbackWebSocketError;
