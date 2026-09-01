//! Validated Native Channel configuration.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use agentpulse_core::ChannelId;
use agentpulse_transport::{
    BearerTokenAuthorizer, DEFAULT_MAX_MESSAGE_BYTES, LoopbackWebSocketConfig, TlsServerIdentity,
    TlsWebSocketConfig,
};
use std::sync::Arc;

use crate::{NATIVE_WEBSOCKET_PATH, NATIVE_WEBSOCKET_SUBPROTOCOL, NativeChannelBuildError};

/// Default maximum number of unsent frames retained per active client.
pub const DEFAULT_OUTBOUND_CAPACITY: usize = 256;

/// Configuration for one local read-only Native Channel.
#[derive(Clone, Debug)]
pub(crate) enum NativeTransportConfig {
    Loopback,
    AuthenticatedLan {
        identity: TlsServerIdentity,
        authorizer: Arc<dyn BearerTokenAuthorizer>,
    },
}

/// Configuration for one local or authenticated-LAN read-only Native Channel.
#[derive(Clone, Debug)]
pub struct NativeChannelConfig {
    pub(crate) channel_id: ChannelId,
    pub(crate) bind_address: SocketAddr,
    pub(crate) transport: NativeTransportConfig,
    pub(crate) handshake_timeout: Duration,
    pub(crate) io_poll_interval: Duration,
    pub(crate) ping_interval: Duration,
    pub(crate) idle_timeout: Duration,
    pub(crate) shutdown_timeout: Duration,
    pub(crate) max_frame_bytes: usize,
    pub(crate) outbound_capacity: usize,
}

impl NativeChannelConfig {
    /// Creates a configuration bound to an ephemeral IPv4 loopback port.
    #[must_use]
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            bind_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            transport: NativeTransportConfig::Loopback,
            handshake_timeout: Duration::from_secs(5),
            io_poll_interval: Duration::from_millis(100),
            ping_interval: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(45),
            shutdown_timeout: Duration::from_secs(5),
            max_frame_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            outbound_capacity: DEFAULT_OUTBOUND_CAPACITY,
        }
    }

    /// Overrides the loopback bind address; port zero requests an ephemeral port.
    #[must_use]
    pub fn with_bind_address(mut self, address: SocketAddr) -> Self {
        self.bind_address = address;
        self.transport = NativeTransportConfig::Loopback;
        self
    }

    /// Creates a configuration for a bearer-authenticated TLS loopback or LAN endpoint.
    pub fn authenticated_lan(
        channel_id: ChannelId,
        address: SocketAddr,
        identity: TlsServerIdentity,
        authorizer: Arc<dyn BearerTokenAuthorizer>,
    ) -> Result<Self, NativeChannelBuildError> {
        let config = Self {
            bind_address: address,
            transport: NativeTransportConfig::AuthenticatedLan {
                identity,
                authorizer,
            },
            ..Self::new(channel_id)
        };
        config.validate()?;
        Ok(config)
    }

    /// Overrides the application and WebSocket handshake deadline.
    #[must_use]
    pub const fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Overrides the socket polling interval used for bounded shutdown.
    #[must_use]
    pub const fn with_io_poll_interval(mut self, interval: Duration) -> Self {
        self.io_poll_interval = interval;
        self
    }

    /// Overrides the server Ping interval.
    #[must_use]
    pub const fn with_ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = interval;
        self
    }

    /// Overrides the maximum time without a client frame or Pong.
    #[must_use]
    pub const fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Overrides the Source worker shutdown deadline.
    #[must_use]
    pub const fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Overrides the maximum complete WebSocket text frame size.
    #[must_use]
    pub const fn with_max_frame_bytes(mut self, maximum: usize) -> Self {
        self.max_frame_bytes = maximum;
        self
    }

    /// Overrides the maximum number of queued outgoing frames.
    #[must_use]
    pub const fn with_outbound_capacity(mut self, capacity: usize) -> Self {
        self.outbound_capacity = capacity;
        self
    }

    /// Returns the configured Channel identity.
    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// Returns the requested loopback bind address.
    #[must_use]
    pub const fn bind_address(&self) -> SocketAddr {
        self.bind_address
    }

    /// Returns the complete WebSocket frame limit.
    #[must_use]
    pub const fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }

    /// Returns the bounded outgoing frame capacity.
    #[must_use]
    pub const fn outbound_capacity(&self) -> usize {
        self.outbound_capacity
    }

    pub(crate) fn validate(&self) -> Result<(), NativeChannelBuildError> {
        if matches!(self.transport, NativeTransportConfig::Loopback)
            && !self.bind_address.ip().is_loopback()
        {
            return Err(NativeChannelBuildError::NonLoopbackAddress {
                address: self.bind_address,
            });
        }
        for (field, value) in [
            ("handshake timeout", self.handshake_timeout),
            ("I/O poll interval", self.io_poll_interval),
            ("Ping interval", self.ping_interval),
            ("idle timeout", self.idle_timeout),
            ("shutdown timeout", self.shutdown_timeout),
        ] {
            if value.is_zero() {
                return Err(NativeChannelBuildError::InvalidSetting {
                    field,
                    reason: "must be greater than zero",
                });
            }
        }
        if self.idle_timeout <= self.ping_interval {
            return Err(NativeChannelBuildError::InvalidSetting {
                field: "idle timeout",
                reason: "must be greater than the Ping interval",
            });
        }
        if self.max_frame_bytes == 0 {
            return Err(NativeChannelBuildError::InvalidSetting {
                field: "maximum frame bytes",
                reason: "must be greater than zero",
            });
        }
        if self.outbound_capacity < 3 {
            return Err(NativeChannelBuildError::InvalidSetting {
                field: "outbound capacity",
                reason: "must hold at least three frames",
            });
        }
        Ok(())
    }

    pub(crate) fn transport_config(
        &self,
    ) -> Result<LoopbackWebSocketConfig, NativeChannelBuildError> {
        self.validate()?;
        LoopbackWebSocketConfig::new(
            self.bind_address,
            NATIVE_WEBSOCKET_PATH,
            NATIVE_WEBSOCKET_SUBPROTOCOL,
        )
        .map(|config| {
            config
                .with_handshake_timeout(self.handshake_timeout)
                .with_io_poll_interval(self.io_poll_interval)
                .with_max_message_bytes(self.max_frame_bytes)
        })
        .map_err(|_| NativeChannelBuildError::InvalidSetting {
            field: "loopback WebSocket",
            reason: "transport configuration is invalid",
        })
    }

    pub(crate) fn tls_transport_config(
        &self,
    ) -> Result<Option<TlsWebSocketConfig>, NativeChannelBuildError> {
        let NativeTransportConfig::AuthenticatedLan {
            identity,
            authorizer,
        } = &self.transport
        else {
            return Ok(None);
        };
        self.validate()?;
        TlsWebSocketConfig::new(
            self.bind_address,
            NATIVE_WEBSOCKET_PATH,
            NATIVE_WEBSOCKET_SUBPROTOCOL,
            identity.clone(),
        )
        .map(|config| {
            Some(
                config
                    .with_bearer_authorizer(Arc::clone(authorizer))
                    .with_handshake_timeout(self.handshake_timeout)
                    .with_io_poll_interval(self.io_poll_interval)
                    .with_max_message_bytes(self.max_frame_bytes),
            )
        })
        .map_err(|error| match error {
            agentpulse_transport::LoopbackWebSocketError::NonPrivateAddress { address } => {
                NativeChannelBuildError::NonPrivateLanAddress { address }
            }
            _ => NativeChannelBuildError::InvalidSetting {
                field: "TLS LAN WebSocket",
                reason: "transport configuration is invalid",
            },
        })
    }
}
