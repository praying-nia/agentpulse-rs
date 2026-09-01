//! Optional authenticated Relay for public-network AgentPulse Native links.
//!
//! The Relay authenticates an outbound Host registration and one Android route,
//! then switches to a fixed-buffer opaque byte tunnel. The existing Host-issued
//! TLS certificate and Native bearer authorization run inside that tunnel, so
//! the Relay never receives Session/Event plaintext or the Native bearer Token.

mod client;
mod config;
mod crypto;
mod endpoint;
mod error;
mod framing;
mod protocol;
mod server;
mod tunnel;

use std::time::Duration;

pub use client::{
    RelayHostConnectionConfig, build_client_hello, connect_host_once,
    connect_host_once_with_route_check, probe,
};
pub use config::{
    CertificateStatus, RELAY_CONFIG_SCHEMA_VERSION, RelayServerConfig, new_server_config,
};
pub use crypto::{
    RelayRouteCredential, derive_route, device_root_from_token, host_authentication_key,
};
pub use endpoint::RelayEndpoint;
pub use error::RelayError;
pub use protocol::{
    EndpointMessage, MAX_CONTROL_BYTES, MAX_ROUTES, RELAY_PROTOCOL_VERSION, RelayErrorCode,
    RelayMessage, RouteRegistration, decode_endpoint, decode_relay, encode_endpoint, encode_relay,
};
pub use server::RelayServer;

/// Nonsecret byte and duration counters for one completed opaque tunnel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelayTunnelStats {
    /// Bytes sent from the Android side toward the Host.
    pub to_host_bytes: u64,
    /// Bytes sent from the Host side toward Android.
    pub to_client_bytes: u64,
    /// Total tunnel lifetime.
    pub elapsed: Duration,
}
