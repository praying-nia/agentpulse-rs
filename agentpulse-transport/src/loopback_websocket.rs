//! Loopback-only bounded WebSocket server transport.

use std::{
    fmt, io,
    net::{SocketAddr, TcpListener, TcpStream},
    time::Duration,
};

use thiserror::Error;
use tungstenite::{
    Message, WebSocket, accept_hdr_with_config,
    handshake::server::{ErrorResponse, Request, Response},
    http::{HeaderValue, StatusCode, header::SEC_WEBSOCKET_PROTOCOL},
    protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
};

/// Default maximum size of one complete WebSocket message.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Configuration for one loopback WebSocket listener.
#[derive(Clone, Debug)]
pub struct LoopbackWebSocketConfig {
    bind_address: SocketAddr,
    path: String,
    subprotocol: String,
    handshake_timeout: Duration,
    io_poll_interval: Duration,
    max_message_bytes: usize,
}

impl LoopbackWebSocketConfig {
    /// Creates a loopback configuration with bounded production defaults.
    pub fn new(
        bind_address: SocketAddr,
        path: impl Into<String>,
        subprotocol: impl Into<String>,
    ) -> Result<Self, LoopbackWebSocketError> {
        let config = Self {
            bind_address,
            path: path.into(),
            subprotocol: subprotocol.into(),
            handshake_timeout: Duration::from_secs(5),
            io_poll_interval: Duration::from_millis(100),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        };
        config.validate()?;
        Ok(config)
    }

    /// Overrides the WebSocket handshake deadline.
    #[must_use]
    pub const fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Overrides the blocking socket read/write polling interval.
    #[must_use]
    pub const fn with_io_poll_interval(mut self, interval: Duration) -> Self {
        self.io_poll_interval = interval;
        self
    }

    /// Overrides the complete incoming and outgoing message limit.
    #[must_use]
    pub const fn with_max_message_bytes(mut self, maximum: usize) -> Self {
        self.max_message_bytes = maximum;
        self
    }

    /// Returns the requested loopback bind address.
    #[must_use]
    pub const fn bind_address(&self) -> SocketAddr {
        self.bind_address
    }

    /// Returns the exact accepted HTTP request path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the required WebSocket subprotocol.
    #[must_use]
    pub fn subprotocol(&self) -> &str {
        &self.subprotocol
    }

    /// Returns the maximum size of a complete WebSocket message.
    #[must_use]
    pub const fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    fn validate(&self) -> Result<(), LoopbackWebSocketError> {
        if !self.bind_address.ip().is_loopback() {
            return Err(LoopbackWebSocketError::NonLoopbackAddress {
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

/// A bound nonblocking loopback WebSocket listener.
pub struct LoopbackWebSocketListener {
    listener: TcpListener,
    config: LoopbackWebSocketConfig,
    local_address: SocketAddr,
}

impl LoopbackWebSocketListener {
    /// Binds and validates one loopback-only listener.
    pub fn bind(config: LoopbackWebSocketConfig) -> Result<Self, LoopbackWebSocketError> {
        config.validate()?;
        let listener = TcpListener::bind(config.bind_address).map_err(|source| {
            LoopbackWebSocketError::Io {
                operation: "bind loopback listener",
                source,
            }
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|source| LoopbackWebSocketError::Io {
                operation: "configure loopback listener",
                source,
            })?;
        let local_address = listener
            .local_addr()
            .map_err(|source| LoopbackWebSocketError::Io {
                operation: "read loopback listener address",
                source,
            })?;
        Ok(Self {
            listener,
            config,
            local_address,
        })
    }

    /// Returns the actual bound address, including an assigned ephemeral port.
    #[must_use]
    pub const fn local_address(&self) -> SocketAddr {
        self.local_address
    }

    /// Accepts and upgrades one pending TCP connection, or returns `None`.
    // Tungstenite requires the handshake callback's concrete `ErrorResponse`.
    #[allow(clippy::result_large_err)]
    pub fn try_accept(&self) -> Result<Option<LoopbackWebSocket>, LoopbackWebSocketError> {
        let (stream, peer_address) = match self.listener.accept() {
            Ok(accepted) => accepted,
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(source) => {
                return Err(LoopbackWebSocketError::Io {
                    operation: "accept loopback connection",
                    source,
                });
            }
        };

        if !peer_address.ip().is_loopback() {
            return Err(LoopbackWebSocketError::NonLoopbackPeer {
                address: peer_address,
            });
        }
        configure_stream(&stream, self.config.handshake_timeout)?;
        let path = self.config.path.clone();
        let subprotocol = self.config.subprotocol.clone();
        let callback = move |request: &Request, response: Response| {
            validate_request(request, response, &path, &subprotocol).map(|mut accepted| {
                if let Ok(value) = HeaderValue::from_str(&subprotocol) {
                    let _ = accepted.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, value);
                }
                accepted
            })
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
        configure_stream(socket.get_mut(), self.config.io_poll_interval)?;
        Ok(Some(LoopbackWebSocket {
            socket,
            peer_address,
            max_message_bytes: self.config.max_message_bytes,
        }))
    }
}

impl fmt::Debug for LoopbackWebSocketListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoopbackWebSocketListener")
            .field("local_address", &self.local_address)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// One accepted bounded loopback WebSocket connection.
pub struct LoopbackWebSocket {
    socket: WebSocket<TcpStream>,
    peer_address: SocketAddr,
    max_message_bytes: usize,
}

impl LoopbackWebSocket {
    /// Returns the connected loopback peer address.
    #[must_use]
    pub const fn peer_address(&self) -> SocketAddr {
        self.peer_address
    }

    /// Reads one application or control outcome.
    pub fn read(&mut self) -> Result<TransportRead, LoopbackWebSocketError> {
        match self.socket.read() {
            Ok(Message::Text(text)) => Ok(TransportRead::Text(text.to_string())),
            Ok(Message::Binary(_)) => Err(LoopbackWebSocketError::BinaryFrame),
            Ok(Message::Ping(_)) => {
                self.socket
                    .flush()
                    .map_err(map_websocket_error("flush automatic pong"))?;
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
            Err(error) => Err(map_websocket_error_value("read WebSocket message", error)),
        }
    }

    /// Sends one complete bounded UTF-8 text message.
    pub fn send_text(&mut self, text: String) -> Result<(), LoopbackWebSocketError> {
        validate_outgoing_size(text.len(), self.max_message_bytes)?;
        self.socket
            .send(Message::text(text))
            .map_err(map_websocket_error("send WebSocket text"))
    }

    /// Sends one WebSocket Ping control frame.
    pub fn send_ping(&mut self) -> Result<(), LoopbackWebSocketError> {
        self.socket
            .send(Message::Ping(Vec::new().into()))
            .map_err(map_websocket_error("send WebSocket ping"))
    }

    /// Attempts a bounded protocol close.
    pub fn close(
        &mut self,
        code: CloseCode,
        reason: impl Into<String>,
    ) -> Result<(), LoopbackWebSocketError> {
        let reason = reason.into();
        let truncated = truncate_close_reason(reason);
        match self.socket.close(Some(CloseFrame {
            code,
            reason: truncated.into(),
        })) {
            Ok(())
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(())
            }
            Err(error) => Err(map_websocket_error_value("close WebSocket", error)),
        }
    }
}

impl fmt::Debug for LoopbackWebSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoopbackWebSocket")
            .field("peer_address", &self.peer_address)
            .field("max_message_bytes", &self.max_message_bytes)
            .finish_non_exhaustive()
    }
}

/// One nonterminal or terminal WebSocket read outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TransportRead {
    /// A complete UTF-8 application text message.
    Text(String),
    /// A Pong frame was received.
    Pong,
    /// A control frame was handled internally.
    Control,
    /// No complete frame arrived during the configured poll interval.
    Timeout,
    /// The peer completed or dropped the connection.
    Closed,
}

/// A loopback listener, handshake, framing, or socket failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoopbackWebSocketError {
    /// A non-loopback bind address is outside this transport's boundary.
    #[error("address {address} is not loopback")]
    NonLoopbackAddress {
        /// The rejected bind address.
        address: SocketAddr,
    },
    /// An accepted peer did not originate on a loopback address.
    #[error("peer {address} is not loopback")]
    NonLoopbackPeer {
        /// The rejected peer address.
        address: SocketAddr,
    },
    /// The configured HTTP request path is invalid.
    #[error("invalid WebSocket path {path:?}")]
    InvalidPath {
        /// The rejected path.
        path: String,
    },
    /// The configured WebSocket subprotocol token is invalid.
    #[error("invalid WebSocket subprotocol {subprotocol:?}")]
    InvalidSubprotocol {
        /// The rejected subprotocol.
        subprotocol: String,
    },
    /// A required timeout or size limit was zero.
    #[error("{field} must be greater than zero")]
    InvalidLimit {
        /// The invalid setting.
        field: &'static str,
    },
    /// TCP socket I/O failed.
    #[error("failed to {operation}: {source}")]
    Io {
        /// The socket operation.
        operation: &'static str,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// The HTTP WebSocket upgrade failed or was rejected.
    #[error("WebSocket handshake failed: {message}")]
    Handshake {
        /// A bounded handshake diagnostic.
        message: String,
    },
    /// The peer sent a binary application message.
    #[error("binary WebSocket messages are not supported")]
    BinaryFrame,
    /// One outgoing message exceeded the configured maximum.
    #[error("WebSocket message is {actual} bytes; maximum is {maximum}")]
    MessageTooLarge {
        /// The attempted payload size.
        actual: usize,
        /// The configured maximum.
        maximum: usize,
    },
    /// WebSocket framing or connection processing failed.
    #[error("failed to {operation}: {message}")]
    WebSocket {
        /// The failed operation.
        operation: &'static str,
        /// A bounded protocol diagnostic.
        message: String,
    },
}

fn configure_stream(stream: &TcpStream, timeout: Duration) -> Result<(), LoopbackWebSocketError> {
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|source| LoopbackWebSocketError::Io {
            operation: "configure socket read timeout",
            source,
        })?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|source| LoopbackWebSocketError::Io {
            operation: "configure socket write timeout",
            source,
        })
}

// This signature is fixed by Tungstenite's server handshake callback contract.
#[allow(clippy::result_large_err)]
fn validate_request(
    request: &Request,
    response: Response,
    expected_path: &str,
    expected_subprotocol: &str,
) -> Result<Response, ErrorResponse> {
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
    Ok(response)
}

fn rejection(status: StatusCode, message: &str) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some(message.to_owned()));
    *response.status_mut() = status;
    response
}

fn validate_outgoing_size(actual: usize, maximum: usize) -> Result<(), LoopbackWebSocketError> {
    if actual > maximum {
        Err(LoopbackWebSocketError::MessageTooLarge { actual, maximum })
    } else {
        Ok(())
    }
}

fn map_websocket_error(
    operation: &'static str,
) -> impl FnOnce(tungstenite::Error) -> LoopbackWebSocketError {
    move |error| map_websocket_error_value(operation, error)
}

fn map_websocket_error_value(
    operation: &'static str,
    error: tungstenite::Error,
) -> LoopbackWebSocketError {
    LoopbackWebSocketError::WebSocket {
        operation,
        message: error.to_string(),
    }
}

fn truncate_close_reason(mut reason: String) -> String {
    const MAX_REASON_BYTES: usize = 123;
    if reason.len() <= MAX_REASON_BYTES {
        return reason;
    }
    while reason.len() > MAX_REASON_BYTES {
        let _ = reason.pop();
    }
    reason
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        net::{IpAddr, Ipv4Addr},
    };

    use super::*;

    #[test]
    fn configuration_rejects_non_loopback_addresses_and_invalid_limits()
    -> Result<(), Box<dyn Error>> {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        assert!(matches!(
            LoopbackWebSocketConfig::new(address, "/native", "agentpulse.native.v1"),
            Err(LoopbackWebSocketError::NonLoopbackAddress { .. })
        ));

        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        assert!(LoopbackWebSocketConfig::new(loopback, "native", "protocol").is_err());
        assert!(LoopbackWebSocketConfig::new(loopback, "/native", "bad protocol").is_err());
        let config = LoopbackWebSocketConfig::new(loopback, "/native", "protocol")?;
        let config = config.with_max_message_bytes(0);
        assert!(matches!(
            LoopbackWebSocketListener::bind(config),
            Err(LoopbackWebSocketError::InvalidLimit { .. })
        ));
        Ok(())
    }

    #[test]
    fn close_reason_truncation_preserves_utf8_boundaries() {
        let reason = "界".repeat(100);
        let truncated = truncate_close_reason(reason);
        assert!(truncated.len() <= 123);
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
