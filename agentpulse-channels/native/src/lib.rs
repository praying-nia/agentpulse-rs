//! Complete local Native Channel for AgentPulse.
//!
//! The Channel serves one explicitly handshaken client over a loopback-only
//! WebSocket, exposes current Provider/Session discovery, establishes an exact
//! Session baseline cursor, and streams normalized JSON v2 domain messages.

mod config;
mod error;
mod port;
mod protocol;
mod runtime;
mod state;
mod status;

use std::sync::{Arc, Mutex};

use agentpulse_core::{ChannelCapabilities, ChannelDescriptor, ChannelKind, NonEmptyText};

pub use config::{DEFAULT_OUTBOUND_CAPACITY, NativeChannelConfig};
pub use error::{
    NativeChannelBuildError, NativeChannelPortError, NativeChannelSourceError, NativeProtocolError,
};
pub use port::NativeChannelPort;
pub use protocol::{
    NativeClientMessage, NativeDeliveryContext, NativeErrorCode, NativeEventRoute,
    NativeServerMessage, NativeSubscriptionStatus, NativeUnsubscriptionStatus,
    decode_client_message, decode_server_message, encode_client_message, encode_server_message,
};
pub use runtime::NativeChannelSource;
pub use status::{NativeChannelHealth, NativeChannelSnapshot};

use state::DeliveryState;
use status::{SharedStatus, lock_status};

/// Native Transport control protocol version implemented by this crate.
pub const NATIVE_TRANSPORT_VERSION: u16 = 3;

/// Exact HTTP path accepted by the local Native WebSocket endpoint.
pub const NATIVE_WEBSOCKET_PATH: &str = "/agentpulse/native/v3";

/// Required RFC 6455 WebSocket subprotocol token.
pub const NATIVE_WEBSOCKET_SUBPROTOCOL: &str = "agentpulse.native.v3";

/// Thread-safe monitoring handle for one built Native Channel.
#[derive(Clone)]
pub struct NativeChannelHandle {
    status: SharedStatus,
}

impl NativeChannelHandle {
    /// Returns an atomic point-in-time health and counter snapshot.
    #[must_use]
    pub fn snapshot(&self) -> NativeChannelSnapshot {
        lock_status(&self.status).clone()
    }
}

/// Paired Port, Source, and monitoring handle produced by the factory.
pub struct NativeChannelParts {
    port: NativeChannelPort,
    source: NativeChannelSource,
    handle: NativeChannelHandle,
}

impl NativeChannelParts {
    /// Borrows the monitoring handle before RuntimeHost registration.
    #[must_use]
    pub const fn handle(&self) -> &NativeChannelHandle {
        &self.handle
    }

    /// Splits the complete Channel into RuntimeHost registration parts and a handle.
    #[must_use]
    pub fn into_parts(self) -> (NativeChannelPort, NativeChannelSource, NativeChannelHandle) {
        (self.port, self.source, self.handle)
    }
}

/// Factory for the local Native Channel.
pub struct NativeChannel;

impl NativeChannel {
    /// Validates configuration and constructs one paired Native Channel Adapter.
    pub fn build(
        config: NativeChannelConfig,
    ) -> Result<NativeChannelParts, NativeChannelBuildError> {
        config.validate()?;
        let descriptor = ChannelDescriptor::new(
            config.channel_id,
            ChannelKind::new("native")?,
            NonEmptyText::new("Native Local")?,
            ChannelCapabilities::NOTIFICATION
                | ChannelCapabilities::SESSION_VIEW
                | ChannelCapabilities::REALTIME_SYNC
                | ChannelCapabilities::APPROVAL
                | ChannelCapabilities::FORM_INPUT
                | ChannelCapabilities::TEXT_INPUT
                | ChannelCapabilities::REMOTE_COMMAND,
        )
        .with_version(NonEmptyText::new(env!("CARGO_PKG_VERSION"))?);
        let status = Arc::new(Mutex::new(NativeChannelSnapshot::default()));
        let state = Arc::new(Mutex::new(DeliveryState::new(
            config.outbound_capacity,
            Arc::clone(&status),
        )));
        let port = NativeChannelPort::new(descriptor.clone(), Arc::clone(&state));
        let source = NativeChannelSource::new(config, descriptor, state, Arc::clone(&status));
        let handle = NativeChannelHandle { status };
        Ok(NativeChannelParts {
            port,
            source,
            handle,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        net::{IpAddr, Ipv4Addr, SocketAddr},
    };

    use agentpulse_bridge::ChannelPort;
    use agentpulse_core::ChannelId;

    use super::*;

    #[test]
    fn factory_declares_the_exact_interaction_contract() -> Result<(), Box<dyn Error>> {
        let parts = NativeChannel::build(NativeChannelConfig::new(ChannelId::new()))?;
        let descriptor = parts.port.descriptor();
        assert_eq!(descriptor.kind().as_str(), "native");
        assert_eq!(
            descriptor.capabilities(),
            ChannelCapabilities::NOTIFICATION
                | ChannelCapabilities::SESSION_VIEW
                | ChannelCapabilities::REALTIME_SYNC
                | ChannelCapabilities::APPROVAL
                | ChannelCapabilities::FORM_INPUT
                | ChannelCapabilities::TEXT_INPUT
                | ChannelCapabilities::REMOTE_COMMAND
        );
        Ok(())
    }

    #[test]
    fn factory_rejects_non_loopback_binding() {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let result = NativeChannel::build(
            NativeChannelConfig::new(ChannelId::new()).with_bind_address(address),
        );
        assert!(matches!(
            result,
            Err(NativeChannelBuildError::NonLoopbackAddress { .. })
        ));
    }
}
