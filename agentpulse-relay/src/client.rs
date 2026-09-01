//! Outbound Host connector and deployment health probe.

use std::{
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};
use time::OffsetDateTime;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    RelayEndpoint, RelayError, RelayTunnelStats,
    crypto::{client_proof, decode_32, host_authentication_key, host_proof},
    framing::{read_frame, write_frame},
    protocol::{
        EndpointMessage, RelayErrorCode, RelayMessage, RouteRegistration, decode_relay,
        encode_endpoint,
    },
    tunnel::pump,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_WAIT_TIMEOUT: Duration = Duration::from_secs(20);
const TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const TUNNEL_STALLED_TIMEOUT: Duration = Duration::from_secs(10);

/// Validated Host-side settings for one Relay registration cycle.
#[derive(Clone)]
pub struct RelayHostConnectionConfig {
    endpoint: RelayEndpoint,
    host_id: String,
    host_authentication_key: Arc<Zeroizing<[u8; 32]>>,
    local_native_address: SocketAddr,
}

impl RelayHostConnectionConfig {
    /// Builds Host settings from the one-time enrollment Token.
    pub fn new(
        endpoint: RelayEndpoint,
        host_id: impl Into<String>,
        enrollment_token: &str,
        local_native_address: SocketAddr,
    ) -> Result<Self, RelayError> {
        if enrollment_token.trim().is_empty()
            || enrollment_token.trim() != enrollment_token
            || enrollment_token.len() > 128
        {
            return Err(RelayError::invalid(
                "enrollment_token",
                "must be nonblank, unpadded by whitespace, and at most 128 bytes",
            ));
        }
        Self::from_key(
            endpoint,
            host_id,
            *host_authentication_key(enrollment_token),
            local_native_address,
        )
    }

    /// Builds Host settings from an already derived 32-byte proof key.
    pub fn from_key(
        endpoint: RelayEndpoint,
        host_id: impl Into<String>,
        authentication_key: [u8; 32],
        local_native_address: SocketAddr,
    ) -> Result<Self, RelayError> {
        let host_id = host_id.into();
        let parsed = Uuid::parse_str(&host_id)
            .map_err(|error| RelayError::invalid("host_id", error.to_string()))?;
        if parsed.get_version_num() != 7 {
            return Err(RelayError::invalid("host_id", "must be UUIDv7"));
        }
        if local_native_address.port() == 0 {
            return Err(RelayError::invalid(
                "local_native_address",
                "port must be non-zero",
            ));
        }
        Ok(Self {
            endpoint,
            host_id,
            host_authentication_key: Arc::new(Zeroizing::new(authentication_key)),
            local_native_address,
        })
    }

    /// Returns the configured public endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &RelayEndpoint {
        &self.endpoint
    }
}

impl std::fmt::Debug for RelayHostConnectionConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelayHostConnectionConfig")
            .field("endpoint", &self.endpoint)
            .field("host_id", &self.host_id)
            .field("local_native_address", &self.local_native_address)
            .finish_non_exhaustive()
    }
}

/// Opens one authenticated Host registration and serves at most one client tunnel.
pub fn connect_host_once(
    config: &RelayHostConnectionConfig,
    routes: &[RouteRegistration],
    stop: &AtomicBool,
) -> Result<RelayTunnelStats, RelayError> {
    connect_host_once_with_route_check(config, routes, stop, || true)
}

/// Opens one Host registration and abandons it when the route snapshot changes.
///
/// The callback runs after registration and after every Relay heartbeat. Returning
/// `false` closes the waiting slot so a caller can immediately register fresh
/// device credentials. Inner Native authorization still enforces revocation on
/// every connection independently of this bounded refresh.
pub fn connect_host_once_with_route_check(
    config: &RelayHostConnectionConfig,
    routes: &[RouteRegistration],
    stop: &AtomicBool,
    routes_are_current: impl Fn() -> bool,
) -> Result<RelayTunnelStats, RelayError> {
    if routes.is_empty() {
        return Err(RelayError::invalid(
            "routes",
            "at least one paired device is required",
        ));
    }
    let mut routes = routes.to_vec();
    routes.sort_by(|left, right| left.route_id.cmp(&right.route_id));
    let mut stream = connect_tls(&config.endpoint)?;
    let (connection_id, nonce, expires_at) = read_challenge(&mut stream)?;
    let proof = host_proof(
        &config.host_authentication_key,
        &connection_id,
        &nonce,
        expires_at,
        &config.host_id,
        &routes,
    )?;
    write_endpoint(
        &mut stream,
        &EndpointMessage::HostHello {
            host_id: config.host_id.clone(),
            routes,
            proof,
        },
    )?;
    stream
        .sock
        .set_read_timeout(Some(HOST_WAIT_TIMEOUT))
        .map_err(|source| RelayError::io("set Relay Host wait timeout", source))?;

    loop {
        match read_relay(&mut stream)? {
            RelayMessage::HostWaiting {
                connection_id: waiting_id,
            } if waiting_id == connection_id => {
                if !routes_are_current() {
                    return Err(RelayError::RoutesChanged);
                }
            }
            RelayMessage::Ping { ping_id } => {
                if !routes_are_current() {
                    return Err(RelayError::RoutesChanged);
                }
                write_endpoint(&mut stream, &EndpointMessage::Pong { ping_id })?;
            }
            RelayMessage::TunnelReady { .. } => break,
            RelayMessage::Error { code, message, .. } => {
                return Err(map_remote_error(code, message));
            }
            _ => {
                return Err(RelayError::Protocol {
                    message: "unexpected message while waiting for a client".to_owned(),
                });
            }
        }
    }

    let mut local = TcpStream::connect_timeout(&config.local_native_address, CONNECT_TIMEOUT)
        .map_err(|source| RelayError::io("connect local Native endpoint", source))?;
    local
        .set_nonblocking(true)
        .map_err(|source| RelayError::io("configure local Native tunnel", source))?;
    stream
        .sock
        .set_nonblocking(true)
        .map_err(|source| RelayError::io("configure public Relay tunnel", source))?;
    pump(
        &mut local,
        &mut stream,
        stop,
        TUNNEL_IDLE_TIMEOUT,
        TUNNEL_STALLED_TIMEOUT,
    )
}

/// Opens public TLS and validates that a Relay v1 challenge is served.
pub fn probe(endpoint: &RelayEndpoint) -> Result<(), RelayError> {
    let mut stream = connect_tls(endpoint)?;
    let _ = read_challenge(&mut stream)?;
    Ok(())
}

/// Builds the Android-equivalent Client Hello for fixtures and independent clients.
pub fn build_client_hello(
    authentication_key: &[u8; 32],
    route_id: &str,
    challenge: &RelayMessage,
) -> Result<EndpointMessage, RelayError> {
    let RelayMessage::Challenge {
        connection_id,
        nonce,
        expires_at_unix_seconds,
    } = challenge
    else {
        return Err(RelayError::invalid(
            "challenge",
            "message is not a challenge",
        ));
    };
    let nonce = decode_32("nonce", nonce)?;
    let proof = client_proof(
        authentication_key,
        connection_id,
        &nonce,
        *expires_at_unix_seconds,
        route_id,
    )?;
    Ok(EndpointMessage::ClientHello {
        route_id: route_id.to_owned(),
        proof,
    })
}

fn connect_tls(
    endpoint: &RelayEndpoint,
) -> Result<StreamOwned<ClientConnection, TcpStream>, RelayError> {
    let addresses = endpoint
        .authority()
        .to_socket_addrs()
        .map_err(|source| RelayError::io("resolve public Relay endpoint", source))?
        .collect::<Vec<_>>();
    let mut last_error = None;
    let mut tcp = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        RelayError::io(
            "connect public Relay endpoint",
            last_error.unwrap_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "DNS returned no addresses")
            }),
        )
    })?;
    tcp.set_read_timeout(Some(CONTROL_TIMEOUT))
        .map_err(|source| RelayError::io("set Relay read timeout", source))?;
    tcp.set_write_timeout(Some(CONTROL_TIMEOUT))
        .map_err(|source| RelayError::io("set Relay write timeout", source))?;
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let client = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = ServerName::try_from(endpoint.host().to_owned())
        .map_err(|error| RelayError::invalid("server_name", error.to_string()))?;
    let connection =
        ClientConnection::new(Arc::new(client), server_name).map_err(|error| RelayError::Tls {
            message: error.to_string(),
        })?;
    Ok(StreamOwned::new(connection, tcp))
}

fn read_challenge(
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
) -> Result<(String, [u8; 32], i64), RelayError> {
    let message = read_relay(stream)?;
    let RelayMessage::Challenge {
        connection_id,
        nonce,
        expires_at_unix_seconds,
    } = message
    else {
        return Err(RelayError::Protocol {
            message: "Relay did not begin with a challenge".to_owned(),
        });
    };
    if expires_at_unix_seconds <= OffsetDateTime::now_utc().unix_timestamp() {
        return Err(RelayError::Timeout {
            operation: "authenticate the Relay challenge",
        });
    }
    let nonce = URL_SAFE_NO_PAD.decode(nonce)?;
    let nonce = nonce
        .try_into()
        .map_err(|_| RelayError::invalid("nonce", "must contain 32 bytes"))?;
    Ok((connection_id, nonce, expires_at_unix_seconds))
}

fn read_relay(
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
) -> Result<RelayMessage, RelayError> {
    decode_relay(&read_frame(stream)?)
}

fn write_endpoint(
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
    message: &EndpointMessage,
) -> Result<(), RelayError> {
    write_frame(stream, &encode_endpoint(message)?)
}

fn map_remote_error(code: RelayErrorCode, message: String) -> RelayError {
    match code {
        RelayErrorCode::AuthenticationFailed => RelayError::Authentication,
        RelayErrorCode::HostUnavailable => RelayError::HostUnavailable,
        RelayErrorCode::HostBusy => RelayError::HostBusy,
        _ => RelayError::Protocol { message },
    }
}
