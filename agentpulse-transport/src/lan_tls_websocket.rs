//! Authenticated TLS WebSocket transport for explicitly selected LAN addresses.

use std::{
    fmt, io,
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};

use rustls::{
    ServerConfig, ServerConnection, StreamOwned,
    pki_types::{CertificateDer, PrivateKeyDer},
};
use tungstenite::{
    Message, WebSocket, accept_hdr_with_config,
    handshake::server::{Request, Response},
    http::{HeaderValue, StatusCode, header::SEC_WEBSOCKET_PROTOCOL},
    protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
};

use crate::loopback_websocket::{
    configure_stream, map_websocket_error_value, rejection, truncate_close_reason,
    validate_outgoing_size,
};
use crate::{DEFAULT_MAX_MESSAGE_BYTES, LoopbackWebSocketError, TransportRead};

const CLIENT_ID_HEADER: &str = "x-agentpulse-client-id";

/// Authorizes one device credential at upgrade time and during a live connection.
pub trait BearerTokenAuthorizer: Send + Sync + fmt::Debug {
    /// Returns whether this client and bearer token remain valid.
    fn authorize(&self, client_id: &str, bearer_token: &str) -> bool;
}

/// DER-encoded server certificate chain and private key.
#[derive(Clone)]
pub struct TlsServerIdentity {
    certificate_chain_der: Vec<Vec<u8>>,
    private_key_der: Vec<u8>,
}

impl TlsServerIdentity {
    /// Creates a TLS identity. The key and chain are parsed again when binding.
    pub fn from_der(
        certificate_chain_der: Vec<Vec<u8>>,
        private_key_der: Vec<u8>,
    ) -> Result<Self, LoopbackWebSocketError> {
        if certificate_chain_der.is_empty() {
            return Err(LoopbackWebSocketError::TlsIdentity {
                message: "certificate chain is empty".to_owned(),
            });
        }
        if private_key_der.is_empty() {
            return Err(LoopbackWebSocketError::TlsIdentity {
                message: "private key is empty".to_owned(),
            });
        }
        Ok(Self {
            certificate_chain_der,
            private_key_der,
        })
    }

    fn server_config(&self) -> Result<ServerConfig, LoopbackWebSocketError> {
        let certificates = self
            .certificate_chain_der
            .iter()
            .cloned()
            .map(CertificateDer::from)
            .collect::<Vec<_>>();
        let private_key =
            PrivateKeyDer::try_from(self.private_key_der.clone()).map_err(|error| {
                LoopbackWebSocketError::TlsIdentity {
                    message: error.to_string(),
                }
            })?;
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .map_err(|error| LoopbackWebSocketError::TlsIdentity {
                message: error.to_string(),
            })
    }
}

impl fmt::Debug for TlsServerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsServerIdentity")
            .field("certificate_count", &self.certificate_chain_der.len())
            .finish_non_exhaustive()
    }
}

/// Configuration for one bounded loopback or private-network TLS WebSocket listener.
#[derive(Clone)]
pub struct TlsWebSocketConfig {
    bind_address: SocketAddr,
    path: String,
    subprotocol: String,
    identity: TlsServerIdentity,
    authorizer: Option<Arc<dyn BearerTokenAuthorizer>>,
    handshake_timeout: Duration,
    io_poll_interval: Duration,
    max_message_bytes: usize,
}

impl TlsWebSocketConfig {
    /// Creates a TLS listener configuration without HTTP bearer authorization.
    ///
    /// Anonymous TLS mode is intended for a short-lived, fingerprint-pinned
    /// pairing endpoint. Long-lived data endpoints must add an authorizer.
    pub fn new(
        bind_address: SocketAddr,
        path: impl Into<String>,
        subprotocol: impl Into<String>,
        identity: TlsServerIdentity,
    ) -> Result<Self, LoopbackWebSocketError> {
        let config = Self {
            bind_address,
            path: path.into(),
            subprotocol: subprotocol.into(),
            identity,
            authorizer: None,
            handshake_timeout: Duration::from_secs(5),
            io_poll_interval: Duration::from_millis(100),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        };
        config.validate()?;
        Ok(config)
    }

    /// Requires a device-specific bearer token for every upgrade.
    #[must_use]
    pub fn with_bearer_authorizer(mut self, authorizer: Arc<dyn BearerTokenAuthorizer>) -> Self {
        self.authorizer = Some(authorizer);
        self
    }

    /// Overrides the TLS and WebSocket handshake deadline.
    #[must_use]
    pub const fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Overrides the blocking encrypted-stream poll interval.
    #[must_use]
    pub const fn with_io_poll_interval(mut self, interval: Duration) -> Self {
        self.io_poll_interval = interval;
        self
    }

    /// Overrides the complete WebSocket message size limit.
    #[must_use]
    pub const fn with_max_message_bytes(mut self, maximum: usize) -> Self {
        self.max_message_bytes = maximum;
        self
    }

    /// Returns the selected LAN address.
    #[must_use]
    pub const fn bind_address(&self) -> SocketAddr {
        self.bind_address
    }

    /// Returns whether HTTP bearer authorization is enabled.
    #[must_use]
    pub const fn requires_authorization(&self) -> bool {
        self.authorizer.is_some()
    }

    fn validate(&self) -> Result<(), LoopbackWebSocketError> {
        if !is_private_endpoint(self.bind_address.ip()) {
            return Err(LoopbackWebSocketError::NonPrivateAddress {
                address: self.bind_address,
            });
        }
        if !self.path.starts_with('/') || self.path.contains(['?', '#']) {
            return Err(LoopbackWebSocketError::InvalidPath {
                path: self.path.clone(),
            });
        }
        if self.subprotocol.trim().is_empty()
            || self
                .subprotocol
                .contains(|character: char| character.is_ascii_whitespace() || character == ',')
        {
            return Err(LoopbackWebSocketError::InvalidSubprotocol {
                subprotocol: self.subprotocol.clone(),
            });
        }
        if self.handshake_timeout.is_zero() {
            return Err(LoopbackWebSocketError::InvalidLimit {
                field: "handshake timeout",
            });
        }
        if self.io_poll_interval.is_zero() {
            return Err(LoopbackWebSocketError::InvalidLimit {
                field: "I/O poll interval",
            });
        }
        if self.max_message_bytes == 0 {
            return Err(LoopbackWebSocketError::InvalidLimit {
                field: "maximum message bytes",
            });
        }
        Ok(())
    }
}

impl fmt::Debug for TlsWebSocketConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsWebSocketConfig")
            .field("bind_address", &self.bind_address)
            .field("path", &self.path)
            .field("subprotocol", &self.subprotocol)
            .field("requires_authorization", &self.authorizer.is_some())
            .field("handshake_timeout", &self.handshake_timeout)
            .field("io_poll_interval", &self.io_poll_interval)
            .field("max_message_bytes", &self.max_message_bytes)
            .finish_non_exhaustive()
    }
}

/// A bound TLS WebSocket listener on one explicit private-network address.
pub struct TlsWebSocketListener {
    listener: TcpListener,
    config: TlsWebSocketConfig,
    server_config: Arc<ServerConfig>,
    local_address: SocketAddr,
}

impl TlsWebSocketListener {
    /// Binds and validates one TLS LAN listener.
    pub fn bind(config: TlsWebSocketConfig) -> Result<Self, LoopbackWebSocketError> {
        config.validate()?;
        let server_config = Arc::new(config.identity.server_config()?);
        let listener = TcpListener::bind(config.bind_address).map_err(|source| {
            LoopbackWebSocketError::Io {
                operation: "bind TLS LAN listener",
                source,
            }
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|source| LoopbackWebSocketError::Io {
                operation: "configure TLS LAN listener",
                source,
            })?;
        let local_address = listener
            .local_addr()
            .map_err(|source| LoopbackWebSocketError::Io {
                operation: "read TLS LAN listener address",
                source,
            })?;
        Ok(Self {
            listener,
            config,
            server_config,
            local_address,
        })
    }

    /// Returns the actual bound address.
    #[must_use]
    pub const fn local_address(&self) -> SocketAddr {
        self.local_address
    }

    /// Accepts, authorizes, and upgrades one pending TLS connection.
    #[allow(clippy::result_large_err)]
    pub fn try_accept(&self) -> Result<Option<TlsWebSocket>, LoopbackWebSocketError> {
        let (tcp, peer_address) = match self.listener.accept() {
            Ok(accepted) => accepted,
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(source) => {
                return Err(LoopbackWebSocketError::Io {
                    operation: "accept TLS LAN connection",
                    source,
                });
            }
        };
        configure_stream(&tcp, self.config.handshake_timeout)?;
        let connection =
            ServerConnection::new(Arc::clone(&self.server_config)).map_err(|error| {
                LoopbackWebSocketError::Tls {
                    message: error.to_string(),
                }
            })?;
        let stream = StreamOwned::new(connection, tcp);
        let authenticated = Arc::new(Mutex::new(None::<(String, String)>));
        let capture = Arc::clone(&authenticated);
        let path = self.config.path.clone();
        let subprotocol = self.config.subprotocol.clone();
        let authorizer = self.config.authorizer.clone();
        let callback = move |request: &Request, mut response: Response| {
            validate_upgrade(request, &path, &subprotocol)?;
            let credentials = if let Some(authorizer) = authorizer.as_ref() {
                let client_id = header(request, CLIENT_ID_HEADER).ok_or_else(|| {
                    rejection(StatusCode::UNAUTHORIZED, "device authorization required")
                })?;
                let bearer = header(request, "authorization")
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        rejection(StatusCode::UNAUTHORIZED, "device authorization required")
                    })?;
                if !authorizer.authorize(client_id, bearer) {
                    return Err(rejection(
                        StatusCode::UNAUTHORIZED,
                        "device authorization failed",
                    ));
                }
                Some((client_id.to_owned(), bearer.to_owned()))
            } else {
                None
            };
            if let Ok(mut slot) = capture.lock() {
                *slot = credentials;
            }
            if let Ok(value) = HeaderValue::from_str(&subprotocol) {
                let _ = response.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, value);
            }
            Ok(response)
        };
        let websocket_config = WebSocketConfig::default()
            .read_buffer_size(16 * 1024)
            .write_buffer_size(0)
            .max_write_buffer_size(self.config.max_message_bytes.saturating_mul(2))
            .max_message_size(Some(self.config.max_message_bytes))
            .max_frame_size(Some(self.config.max_message_bytes));
        let mut socket =
            accept_hdr_with_config(stream, callback, Some(websocket_config)).map_err(|error| {
                LoopbackWebSocketError::Handshake {
                    message: error.to_string(),
                }
            })?;
        configure_stream(&socket.get_mut().sock, self.config.io_poll_interval)?;
        let credentials = authenticated.lock().ok().and_then(|slot| slot.clone());
        Ok(Some(TlsWebSocket {
            socket,
            peer_address,
            max_message_bytes: self.config.max_message_bytes,
            credentials,
            authorizer: self.config.authorizer.clone(),
        }))
    }
}

impl fmt::Debug for TlsWebSocketListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsWebSocketListener")
            .field("local_address", &self.local_address)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// One accepted bounded TLS WebSocket connection.
pub struct TlsWebSocket {
    socket: WebSocket<StreamOwned<ServerConnection, TcpStream>>,
    peer_address: SocketAddr,
    max_message_bytes: usize,
    credentials: Option<(String, String)>,
    authorizer: Option<Arc<dyn BearerTokenAuthorizer>>,
}

impl TlsWebSocket {
    /// Returns the connected peer address.
    #[must_use]
    pub const fn peer_address(&self) -> SocketAddr {
        self.peer_address
    }

    /// Returns the upgrade-authenticated client identity, when required.
    #[must_use]
    pub fn authenticated_client_id(&self) -> Option<&str> {
        self.credentials.as_ref().map(|value| value.0.as_str())
    }

    /// Rechecks the credential so revocation closes an already active client.
    #[must_use]
    pub fn remains_authorized(&self) -> bool {
        match (&self.authorizer, &self.credentials) {
            (Some(authorizer), Some((client_id, token))) => authorizer.authorize(client_id, token),
            (None, None) => true,
            _ => false,
        }
    }

    /// Reads one application or control outcome.
    pub fn read(&mut self) -> Result<TransportRead, LoopbackWebSocketError> {
        match self.socket.read() {
            Ok(Message::Text(text)) => Ok(TransportRead::Text(text.to_string())),
            Ok(Message::Binary(_)) => Err(LoopbackWebSocketError::BinaryFrame),
            Ok(Message::Ping(_)) => {
                self.socket
                    .flush()
                    .map_err(|error| map_websocket_error_value("flush automatic pong", error))?;
                Ok(TransportRead::Control)
            }
            Ok(Message::Pong(_)) => Ok(TransportRead::Pong),
            Ok(Message::Close(_)) => Ok(TransportRead::Closed),
            Ok(Message::Frame(_)) => Ok(TransportRead::Control),
            Err(tungstenite::Error::Io(source))
                if matches!(
                    source.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(TransportRead::Timeout)
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(TransportRead::Closed)
            }
            Err(error) => Err(map_websocket_error_value(
                "read TLS WebSocket message",
                error,
            )),
        }
    }

    /// Sends one complete bounded text message.
    pub fn send_text(&mut self, text: String) -> Result<(), LoopbackWebSocketError> {
        validate_outgoing_size(text.len(), self.max_message_bytes)?;
        self.socket
            .send(Message::text(text))
            .map_err(|error| map_websocket_error_value("send TLS WebSocket text", error))
    }

    /// Sends one Ping frame.
    pub fn send_ping(&mut self) -> Result<(), LoopbackWebSocketError> {
        self.socket
            .send(Message::Ping(Vec::new().into()))
            .map_err(|error| map_websocket_error_value("send TLS WebSocket ping", error))
    }

    /// Attempts a bounded protocol close.
    pub fn close(
        &mut self,
        code: CloseCode,
        reason: impl Into<String>,
    ) -> Result<(), LoopbackWebSocketError> {
        let reason = truncate_close_reason(reason.into());
        match self.socket.close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })) {
            Ok(())
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(())
            }
            Err(error) => Err(map_websocket_error_value("close TLS WebSocket", error)),
        }
    }
}

impl fmt::Debug for TlsWebSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsWebSocket")
            .field("peer_address", &self.peer_address)
            .field("authenticated_client_id", &self.authenticated_client_id())
            .field("max_message_bytes", &self.max_message_bytes)
            .finish_non_exhaustive()
    }
}

#[allow(clippy::result_large_err)]
fn validate_upgrade(
    request: &Request,
    expected_path: &str,
    expected_subprotocol: &str,
) -> Result<(), tungstenite::handshake::server::ErrorResponse> {
    if request.uri().path() != expected_path || request.uri().query().is_some() {
        return Err(rejection(StatusCode::NOT_FOUND, "unknown WebSocket path"));
    }
    let offered = request
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == expected_subprotocol)
        });
    if !offered {
        return Err(rejection(
            StatusCode::BAD_REQUEST,
            "required WebSocket subprotocol was not offered",
        ));
    }
    Ok(())
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
}

fn is_private_endpoint(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        IpAddr::V6(address) => {
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn endpoint_boundary_accepts_loopback_and_private_but_rejects_public() {
        for address in [Ipv4Addr::UNSPECIFIED, Ipv4Addr::new(8, 8, 8, 8)] {
            assert!(!is_private_endpoint(IpAddr::V4(address)));
        }
        assert!(is_private_endpoint(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_private_endpoint(IpAddr::V4(Ipv4Addr::new(
            192, 168, 1, 20
        ))));
        assert!(is_private_endpoint(IpAddr::V4(Ipv4Addr::new(
            169, 254, 1, 4
        ))));
    }
}
