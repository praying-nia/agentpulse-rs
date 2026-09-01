//! Strict Relay v1 control messages.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{RelayError, crypto::decode_32};

/// Relay control protocol version.
pub const RELAY_PROTOCOL_VERSION: u16 = 1;
/// Maximum complete control-frame JSON payload.
pub const MAX_CONTROL_BYTES: usize = 16 * 1024;
/// Maximum device routes accepted from one Host.
pub const MAX_ROUTES: usize = 16;

/// One device route registered by an authenticated Host.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRegistration {
    /// Opaque 32-byte Base64URL route identifier.
    pub route_id: String,
    /// Domain-separated 32-byte Base64URL HMAC key.
    pub authentication_key: String,
}

impl std::fmt::Debug for RouteRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteRegistration")
            .field("route_id", &self.route_id)
            .field("authentication_key", &"[REDACTED]")
            .finish()
    }
}

/// Endpoint-originated Relay control message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EndpointMessage {
    /// Registers one Host and its currently authorized routes.
    HostHello {
        /// Stable UUIDv7 Host identity configured on the Relay.
        host_id: String,
        /// Ordered, unique device routes.
        routes: Vec<RouteRegistration>,
        /// Challenge-bound HMAC proof.
        proof: String,
    },
    /// Requests one authenticated device route.
    ClientHello {
        /// Opaque route identifier derived from the existing device Token.
        route_id: String,
        /// Challenge-bound HMAC proof.
        proof: String,
    },
    /// Replies to a waiting-registration heartbeat.
    Pong {
        /// Matching UUIDv7 Ping identity.
        ping_id: String,
    },
}

/// Relay-originated control message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelayMessage {
    /// Starts one bounded authentication exchange.
    Challenge {
        /// UUIDv7 identity unique to this outer TLS connection.
        connection_id: String,
        /// Random 32-byte Base64URL nonce.
        nonce: String,
        /// Positive UTC Unix expiry in seconds.
        expires_at_unix_seconds: i64,
    },
    /// Confirms that an authenticated Host is waiting for a client.
    HostWaiting {
        /// This Host outer-connection identity.
        connection_id: String,
    },
    /// Ends control framing and starts the opaque byte tunnel.
    TunnelReady {
        /// UUIDv7 identity of the matched peer outer connection.
        peer_connection_id: String,
    },
    /// Keeps an authenticated Host registration alive.
    Ping {
        /// UUIDv7 heartbeat identity.
        ping_id: String,
    },
    /// Fails closed with a stable public error.
    Error {
        /// Stable error code.
        code: RelayErrorCode,
        /// Bounded nonsecret diagnostic.
        message: String,
        /// Whether a new outer connection may succeed later.
        recoverable: bool,
    },
}

/// Stable Relay v1 error code.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayErrorCode {
    /// TLS completed but control authentication failed.
    AuthenticationFailed,
    /// The challenge expired or the control state was invalid.
    InvalidHandshake,
    /// A strict frame or field was invalid.
    InvalidRequest,
    /// The configured Host is not waiting.
    HostUnavailable,
    /// Another Host registration owns an overlapping route.
    HostBusy,
    /// A bounded capacity or size limit was exceeded.
    ResourceLimit,
    /// An internal operation failed safely.
    Internal,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    relay_version: u64,
    message: T,
}

/// Encodes one endpoint message with the strict Relay v1 envelope.
pub fn encode_endpoint(message: &EndpointMessage) -> Result<Vec<u8>, RelayError> {
    validate_endpoint(message)?;
    encode(message)
}

/// Decodes and validates one endpoint message.
pub fn decode_endpoint(input: &[u8]) -> Result<EndpointMessage, RelayError> {
    let message = decode(input)?;
    validate_endpoint(&message)?;
    Ok(message)
}

/// Encodes one Relay message with the strict Relay v1 envelope.
pub fn encode_relay(message: &RelayMessage) -> Result<Vec<u8>, RelayError> {
    validate_relay(message)?;
    encode(message)
}

/// Decodes and validates one Relay message.
pub fn decode_relay(input: &[u8]) -> Result<RelayMessage, RelayError> {
    let message = decode(input)?;
    validate_relay(&message)?;
    Ok(message)
}

fn encode<T: Serialize + Clone>(message: &T) -> Result<Vec<u8>, RelayError> {
    let bytes = serde_json::to_vec(&Envelope {
        relay_version: u64::from(RELAY_PROTOCOL_VERSION),
        message: message.clone(),
    })?;
    if bytes.len() > MAX_CONTROL_BYTES {
        return Err(RelayError::invalid(
            "control_frame",
            "encoded message exceeds 16 KiB",
        ));
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(input: &[u8]) -> Result<T, RelayError> {
    if input.is_empty() || input.len() > MAX_CONTROL_BYTES {
        return Err(RelayError::invalid(
            "control_frame",
            "message must be between 1 byte and 16 KiB",
        ));
    }
    let envelope = serde_json::from_slice::<Envelope<T>>(input)?;
    if envelope.relay_version != u64::from(RELAY_PROTOCOL_VERSION) {
        return Err(RelayError::invalid(
            "relay_version",
            "unsupported Relay version",
        ));
    }
    Ok(envelope.message)
}

fn validate_endpoint(message: &EndpointMessage) -> Result<(), RelayError> {
    match message {
        EndpointMessage::HostHello {
            host_id,
            routes,
            proof,
        } => {
            uuid_v7("host_id", host_id)?;
            if routes.is_empty() || routes.len() > MAX_ROUTES {
                return Err(RelayError::invalid(
                    "routes",
                    "must contain between 1 and 16 routes",
                ));
            }
            let mut previous: Option<&str> = None;
            for route in routes {
                decode_32("route_id", &route.route_id)?;
                decode_32("authentication_key", &route.authentication_key)?;
                if previous.is_some_and(|value| value >= route.route_id.as_str()) {
                    return Err(RelayError::invalid(
                        "routes",
                        "must be strictly ordered and unique by route_id",
                    ));
                }
                previous = Some(&route.route_id);
            }
            decode_32("proof", proof)?;
        }
        EndpointMessage::ClientHello { route_id, proof } => {
            decode_32("route_id", route_id)?;
            decode_32("proof", proof)?;
        }
        EndpointMessage::Pong { ping_id } => uuid_v7("ping_id", ping_id)?,
    }
    Ok(())
}

fn validate_relay(message: &RelayMessage) -> Result<(), RelayError> {
    match message {
        RelayMessage::Challenge {
            connection_id,
            nonce,
            expires_at_unix_seconds,
        } => {
            uuid_v7("connection_id", connection_id)?;
            decode_32("nonce", nonce)?;
            if *expires_at_unix_seconds <= 0 {
                return Err(RelayError::invalid(
                    "expires_at_unix_seconds",
                    "must be positive",
                ));
            }
        }
        RelayMessage::HostWaiting { connection_id } => uuid_v7("connection_id", connection_id)?,
        RelayMessage::TunnelReady { peer_connection_id } => {
            uuid_v7("peer_connection_id", peer_connection_id)?
        }
        RelayMessage::Ping { ping_id } => uuid_v7("ping_id", ping_id)?,
        RelayMessage::Error { message, .. } => {
            if message.trim().is_empty() || message.len() > 256 {
                return Err(RelayError::invalid(
                    "message",
                    "must be nonblank and at most 256 bytes",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn challenge() -> (RelayMessage, [u8; 32], String, i64) {
    use rand::Rng as _;

    let mut nonce = [0_u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    let connection_id = Uuid::now_v7().to_string();
    let expires_at_unix_seconds = OffsetDateTime::now_utc().unix_timestamp() + 10;
    (
        RelayMessage::Challenge {
            connection_id: connection_id.clone(),
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            expires_at_unix_seconds,
        },
        nonce,
        connection_id,
        expires_at_unix_seconds,
    )
}

fn uuid_v7(field: &'static str, value: &str) -> Result<(), RelayError> {
    let value =
        Uuid::parse_str(value).map_err(|error| RelayError::invalid(field, error.to_string()))?;
    if value.get_version_num() != 7 {
        return Err(RelayError::invalid(field, "must be UUIDv7"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn strict_messages_round_trip_and_reject_unknown_fields() -> Result<(), Box<dyn Error>> {
        let (message, _, _, _) = challenge();
        let bytes = encode_relay(&message)?;
        assert_eq!(decode_relay(&bytes)?, message);
        assert!(decode_relay(br#"{"relay_version":1,"message":{"type":"ping","ping_id":"01890f47-7c00-7000-8000-000000000001","extra":true}}"#).is_err());
        Ok(())
    }
}
