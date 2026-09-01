//! One-shot fingerprint-pinned pairing server.

use std::{
    net::SocketAddr,
    thread,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use tungstenite::protocol::frame::coding::CloseCode;
use uuid::Uuid;
use zeroize::Zeroizing;

use agentpulse_transport::{TlsWebSocket, TlsWebSocketConfig, TlsWebSocketListener, TransportRead};

use crate::{
    HostCredentialStore, PairingBundle, PairingError, PairingErrorCode, PairingRequest,
    PairingServerMessage, decode_pairing_request, encode_server_message,
    protocol::{PAIRING_PROTOCOL_VERSION, PAIRING_WEBSOCKET_PATH, PAIRING_WEBSOCKET_SUBPROTOCOL},
};

const SESSION_LIFETIME: Duration = Duration::from_secs(120);
const MAX_ATTEMPTS: usize = 5;
const MAX_PAIRING_MESSAGE_BYTES: usize = 16 * 1024;

/// Successful pairing details safe to print after the token has been delivered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingOutcome {
    /// Paired client identity.
    pub client_id: String,
    /// Approved device name.
    pub display_name: String,
}

/// One bound, single-use Pairing v1 session.
pub struct PairingSession {
    listener: TlsWebSocketListener,
    store: HostCredentialStore,
    bundle: PairingBundle,
    pairing_uri: String,
    native_address: SocketAddr,
    native_transport_version: u16,
    domain_protocol_versions: Vec<u16>,
    expires_at: Instant,
}

impl PairingSession {
    /// Binds a two-minute pairing endpoint for one QR bootstrap.
    pub fn bind(
        store: HostCredentialStore,
        bind_address: SocketAddr,
        native_address: SocketAddr,
        relay_endpoint: String,
        native_transport_version: u16,
        domain_protocol_versions: Vec<u16>,
    ) -> Result<Self, PairingError> {
        if !bind_address.ip().is_loopback() {
            return Err(PairingError::InvalidField {
                field: "bind_address",
                reason: "QR pairing listeners must be loopback-only".to_owned(),
            });
        }
        if domain_protocol_versions.is_empty() || native_transport_version == 0 {
            return Err(PairingError::InvalidField {
                field: "supported_versions",
                reason: "versions must be non-empty and non-zero".to_owned(),
            });
        }
        let identity = store.load_identity()?;
        let config = TlsWebSocketConfig::new(
            bind_address,
            PAIRING_WEBSOCKET_PATH,
            PAIRING_WEBSOCKET_SUBPROTOCOL,
            identity.tls_identity()?,
        )?
        .with_handshake_timeout(Duration::from_secs(5))
        .with_io_poll_interval(Duration::from_millis(100))
        .with_max_message_bytes(MAX_PAIRING_MESSAGE_BYTES);
        let listener = TlsWebSocketListener::bind(config)?;
        let local_address = listener.local_address();
        let mut secret = Zeroizing::new([0_u8; 32]);
        rand::rng().fill_bytes(secret.as_mut());
        let bootstrap_token = URL_SAFE_NO_PAD.encode(secret.as_ref());
        let leaf_sha256 = identity.leaf_sha256();
        let bundle = PairingBundle {
            pairing_version: PAIRING_PROTOCOL_VERSION,
            pairing_id: Uuid::now_v7().to_string(),
            host_id: identity.host_id,
            host_name: identity.host_name,
            server_name: identity.server_name,
            address: local_address.ip().to_string(),
            port: local_address.port(),
            leaf_sha256,
            bootstrap_token,
            relay_endpoint,
            expires_at_unix_seconds: OffsetDateTime::now_utc().unix_timestamp()
                + i64::try_from(SESSION_LIFETIME.as_secs()).unwrap_or(120),
        };
        let pairing_uri = bundle.to_uri()?;
        Ok(Self {
            listener,
            store,
            bundle,
            pairing_uri,
            native_address,
            native_transport_version,
            domain_protocol_versions,
            expires_at: Instant::now() + SESSION_LIFETIME,
        })
    }

    /// Returns the opaque URI encoded by the terminal QR code.
    #[must_use]
    pub fn pairing_uri(&self) -> &str {
        &self.pairing_uri
    }

    /// Returns the pairing bundle without reparsing the URI.
    #[must_use]
    pub const fn bundle(&self) -> &PairingBundle {
        &self.bundle
    }

    /// Returns the private listener address used as the Relay tunnel target.
    #[must_use]
    pub fn local_address(&self) -> SocketAddr {
        self.listener.local_address()
    }

    /// Serves until one locally approved credential is issued or the session ends.
    pub fn serve(
        self,
        approve: impl Fn(&PairingRequest) -> bool,
    ) -> Result<PairingOutcome, PairingError> {
        let mut attempts = 0_usize;
        while Instant::now() < self.expires_at {
            let mut socket = match self.listener.try_accept() {
                Ok(Some(socket)) => socket,
                Ok(None) => {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(agentpulse_transport::LoopbackWebSocketError::Handshake { .. }) => {
                    attempts += 1;
                    if attempts >= MAX_ATTEMPTS {
                        return Err(PairingError::AttemptLimit);
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let request = match read_request(&mut socket, self.expires_at) {
                Ok(request) => request,
                Err(error) => {
                    attempts += 1;
                    let _ = send(
                        &mut socket,
                        &PairingServerMessage::Error {
                            code: PairingErrorCode::InvalidRequest,
                            message: error.to_string(),
                            recoverable: attempts < MAX_ATTEMPTS,
                        },
                    );
                    let _ = socket.close(CloseCode::Protocol, "invalid pairing request");
                    if attempts >= MAX_ATTEMPTS {
                        return Err(PairingError::AttemptLimit);
                    }
                    continue;
                }
            };
            if !credential_matches(&request.pairing_id, &self.bundle.pairing_id)
                || !credential_matches(&request.bootstrap_token, &self.bundle.bootstrap_token)
            {
                attempts += 1;
                let _ = send(
                    &mut socket,
                    &PairingServerMessage::Error {
                        code: PairingErrorCode::InvalidCredential,
                        message: "pairing credential is invalid".to_owned(),
                        recoverable: attempts < MAX_ATTEMPTS,
                    },
                );
                let _ = socket.close(CloseCode::Policy, "invalid pairing credential");
                if attempts >= MAX_ATTEMPTS {
                    return Err(PairingError::AttemptLimit);
                }
                continue;
            }
            send(
                &mut socket,
                &PairingServerMessage::Pending {
                    client_id: request.client_id.clone(),
                    display_name: request.display_name.clone(),
                },
            )?;
            if !approve(&request) {
                send(
                    &mut socket,
                    &PairingServerMessage::Error {
                        code: PairingErrorCode::Denied,
                        message: "the Host user denied this device".to_owned(),
                        recoverable: false,
                    },
                )?;
                let _ = socket.close(CloseCode::Policy, "pairing denied");
                return Err(PairingError::Denied);
            }
            let token = match self.store.issue_device(
                &request.client_id,
                &request.display_name,
                request.version.as_deref(),
            ) {
                Ok(token) => token,
                Err(PairingError::DeviceCapacity { .. }) => {
                    send(
                        &mut socket,
                        &PairingServerMessage::Error {
                            code: PairingErrorCode::Capacity,
                            message: "the Host has reached its paired-device limit".to_owned(),
                            recoverable: false,
                        },
                    )?;
                    return Err(PairingError::DeviceCapacity { capacity: 16 });
                }
                Err(error) => return Err(error),
            };
            let identity = self.store.load_identity()?;
            let ca_certificate_der = identity.ca_certificate_base64();
            send(
                &mut socket,
                &PairingServerMessage::Succeeded {
                    host_id: identity.host_id,
                    host_name: identity.host_name,
                    ca_certificate_der,
                    server_name: identity.server_name,
                    native_address: self.native_address.ip().to_string(),
                    native_port: self.native_address.port(),
                    access_token: token,
                    native_transport_version: self.native_transport_version,
                    domain_protocol_versions: self.domain_protocol_versions,
                },
            )?;
            let _ = socket.close(CloseCode::Normal, "pairing completed");
            return Ok(PairingOutcome {
                client_id: request.client_id,
                display_name: request.display_name,
            });
        }
        Err(PairingError::Expired)
    }
}

fn read_request(
    socket: &mut TlsWebSocket,
    expires_at: Instant,
) -> Result<PairingRequest, PairingError> {
    loop {
        if Instant::now() >= expires_at {
            return Err(PairingError::Expired);
        }
        match socket.read()? {
            TransportRead::Text(text) => return decode_pairing_request(text.as_bytes()),
            TransportRead::Pong | TransportRead::Control | TransportRead::Timeout => {}
            TransportRead::Closed => {
                return Err(PairingError::InvalidField {
                    field: "pairing_socket",
                    reason: "client disconnected before requesting pairing".to_owned(),
                });
            }
            _ => {
                return Err(PairingError::InvalidField {
                    field: "pairing_socket",
                    reason: "unsupported future transport outcome".to_owned(),
                });
            }
        }
    }
}

fn send(socket: &mut TlsWebSocket, message: &PairingServerMessage) -> Result<(), PairingError> {
    let bytes = encode_server_message(message)?;
    let text = String::from_utf8(bytes).map_err(|error| PairingError::InvalidField {
        field: "encoded_pairing_message",
        reason: error.to_string(),
    })?;
    socket.send_text(text)?;
    Ok(())
}

fn credential_matches(supplied: &str, expected: &str) -> bool {
    let supplied = Sha256::digest(supplied.as_bytes());
    let expected = Sha256::digest(expected.as_bytes());
    bool::from(supplied.as_slice().ct_eq(expected.as_slice()))
}
