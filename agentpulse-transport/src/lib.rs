//! Bounded transport primitives used by concrete AgentPulse adapters.
//!
//! The initial transport is an unencrypted WebSocket listener that enforces a
//! loopback-only bind address. Authentication and non-loopback networking are
//! deliberately outside this crate's current contract.

mod loopback_websocket;

pub use loopback_websocket::{
    DEFAULT_MAX_MESSAGE_BYTES, LoopbackWebSocket, LoopbackWebSocketConfig, LoopbackWebSocketError,
    LoopbackWebSocketListener, TransportRead,
};
