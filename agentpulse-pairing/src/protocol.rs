//! Strict Pairing v1 URI and WebSocket messages.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use time::OffsetDateTime;

use crate::PairingError;

/// Pairing protocol version.
pub const PAIRING_PROTOCOL_VERSION: u16 = 1;
/// Pairing WebSocket path.
pub const PAIRING_WEBSOCKET_PATH: &str = "/agentpulse/pair/v1";
/// Pairing WebSocket subprotocol.
pub const PAIRING_WEBSOCKET_SUBPROTOCOL: &str = "agentpulse.pair.v1";
const PAIRING_URI_PREFIX: &str = "agentpulse://pair/v1/";

/// Opaque bootstrap bundle carried only by a QR code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingBundle {
    /// Bundle schema version.
    pub pairing_version: u16,
    /// UUIDv7 one-time pairing identity.
    pub pairing_id: String,
    /// Stable UUIDv7 Host identity.
    pub host_id: String,
    /// User-facing Host name.
    pub host_name: String,
    /// Stable TLS DNS name.
    pub server_name: String,
    /// Current private-network address.
    pub address: String,
    /// Ephemeral pairing port.
    pub port: u16,
    /// Lowercase SHA-256 of the current leaf certificate DER.
    pub leaf_sha256: String,
    /// Single-use 256-bit bootstrap secret.
    pub bootstrap_token: String,
    /// Public Relay authority used for QR-only bootstrap.
    pub relay_endpoint: String,
    /// UTC Unix expiry in seconds.
    pub expires_at_unix_seconds: i64,
}

impl PairingBundle {
    /// Encodes the canonical struct field order as an AgentPulse URI.
    pub fn to_uri(&self) -> Result<String, PairingError> {
        validate_bundle(self)?;
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?);
        Ok(format!("{PAIRING_URI_PREFIX}{encoded}"))
    }
}

/// Device request sent over the pinned pairing socket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingRequest {
    /// Matching one-time pairing ID.
    pub pairing_id: String,
    /// Matching bootstrap token.
    pub bootstrap_token: String,
    /// Stable Android installation UUIDv7.
    pub client_id: String,
    /// User-facing device name.
    pub display_name: String,
    /// Optional client build version.
    pub version: Option<String>,
}

/// Stable Pairing v1 failure codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PairingErrorCode {
    /// A field or message was invalid.
    InvalidRequest,
    /// The pairing ID or token did not match.
    InvalidCredential,
    /// The pairing session expired.
    Expired,
    /// The pairing session was already consumed.
    Used,
    /// The Host user denied the device.
    Denied,
    /// The device store is full.
    Capacity,
    /// An internal operation failed safely.
    Internal,
}

/// Server-originated Pairing v1 messages.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PairingServerMessage {
    /// The Host is waiting for explicit local confirmation.
    Pending {
        /// Matching client identity.
        client_id: String,
        /// Device name shown in the terminal.
        display_name: String,
    },
    /// A persistent device credential was issued.
    Succeeded {
        /// Stable Host identity.
        host_id: String,
        /// User-facing Host name.
        host_name: String,
        /// Base64 DER local CA trust anchor.
        ca_certificate_der: String,
        /// Stable TLS DNS name.
        server_name: String,
        /// Current Native endpoint address.
        native_address: String,
        /// Current Native endpoint port.
        native_port: u16,
        /// Per-device 256-bit bearer token.
        access_token: String,
        /// Supported Native Transport version.
        native_transport_version: u16,
        /// Supported domain protocol versions.
        domain_protocol_versions: Vec<u16>,
    },
    /// A terminal or recoverable pairing failure.
    Error {
        /// Stable programmatic code.
        code: PairingErrorCode,
        /// Bounded user-facing diagnostic.
        message: String,
        /// Whether another request may be sent on this session.
        recoverable: bool,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    pairing_version: u64,
    message: T,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RequestDto {
    PairRequest {
        pairing_id: String,
        bootstrap_token: String,
        client_id: String,
        display_name: String,
        version: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::enum_variant_names)]
enum ServerDto {
    PairingPending {
        client_id: String,
        display_name: String,
    },
    PairingSucceeded {
        host_id: String,
        host_name: String,
        ca_certificate_der: String,
        server_name: String,
        native_address: String,
        native_port: u16,
        access_token: String,
        native_transport_version: u16,
        domain_protocol_versions: Vec<u16>,
    },
    PairingError {
        code: ErrorCodeDto,
        message: String,
        recoverable: bool,
    },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCodeDto {
    InvalidRequest,
    InvalidCredential,
    Expired,
    Used,
    Denied,
    Capacity,
    Internal,
}

/// Decodes a strict pairing URI.
pub fn decode_pairing_uri(uri: &str) -> Result<PairingBundle, PairingError> {
    let encoded =
        uri.strip_prefix(PAIRING_URI_PREFIX)
            .ok_or_else(|| PairingError::InvalidField {
                field: "pairing_uri",
                reason: "unsupported URI scheme or version".to_owned(),
            })?;
    let bundle: PairingBundle = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?;
    validate_bundle(&bundle)?;
    if bundle.expires_at_unix_seconds <= OffsetDateTime::now_utc().unix_timestamp() {
        return Err(invalid(
            "expires_at_unix_seconds",
            "pairing session expired",
        ));
    }
    Ok(bundle)
}

/// Encodes one strict device request.
pub fn encode_pairing_request(request: &PairingRequest) -> Result<Vec<u8>, PairingError> {
    validate_request(request)?;
    Ok(serde_json::to_vec(&Envelope {
        pairing_version: u64::from(PAIRING_PROTOCOL_VERSION),
        message: RequestDto::PairRequest {
            pairing_id: request.pairing_id.clone(),
            bootstrap_token: request.bootstrap_token.clone(),
            client_id: request.client_id.clone(),
            display_name: request.display_name.clone(),
            version: request.version.clone(),
        },
    })?)
}

/// Decodes one strict device request.
pub fn decode_pairing_request(input: &[u8]) -> Result<PairingRequest, PairingError> {
    let envelope: Envelope<RequestDto> = serde_json::from_slice(input)?;
    validate_version(envelope.pairing_version)?;
    let RequestDto::PairRequest {
        pairing_id,
        bootstrap_token,
        client_id,
        display_name,
        version,
    } = envelope.message;
    let request = PairingRequest {
        pairing_id,
        bootstrap_token,
        client_id,
        display_name,
        version,
    };
    validate_request(&request)?;
    Ok(request)
}

/// Encodes one strict server message.
pub fn encode_server_message(message: &PairingServerMessage) -> Result<Vec<u8>, PairingError> {
    validate_server_message(message)?;
    let message = match message {
        PairingServerMessage::Pending {
            client_id,
            display_name,
        } => ServerDto::PairingPending {
            client_id: client_id.clone(),
            display_name: display_name.clone(),
        },
        PairingServerMessage::Succeeded {
            host_id,
            host_name,
            ca_certificate_der,
            server_name,
            native_address,
            native_port,
            access_token,
            native_transport_version,
            domain_protocol_versions,
        } => ServerDto::PairingSucceeded {
            host_id: host_id.clone(),
            host_name: host_name.clone(),
            ca_certificate_der: ca_certificate_der.clone(),
            server_name: server_name.clone(),
            native_address: native_address.clone(),
            native_port: *native_port,
            access_token: access_token.clone(),
            native_transport_version: *native_transport_version,
            domain_protocol_versions: domain_protocol_versions.clone(),
        },
        PairingServerMessage::Error {
            code,
            message,
            recoverable,
        } => ServerDto::PairingError {
            code: ErrorCodeDto::from(*code),
            message: message.clone(),
            recoverable: *recoverable,
        },
    };
    Ok(serde_json::to_vec(&Envelope {
        pairing_version: u64::from(PAIRING_PROTOCOL_VERSION),
        message,
    })?)
}

/// Decodes one strict server message.
pub fn decode_server_message(input: &[u8]) -> Result<PairingServerMessage, PairingError> {
    let envelope: Envelope<ServerDto> = serde_json::from_slice(input)?;
    validate_version(envelope.pairing_version)?;
    let message = match envelope.message {
        ServerDto::PairingPending {
            client_id,
            display_name,
        } => PairingServerMessage::Pending {
            client_id,
            display_name,
        },
        ServerDto::PairingSucceeded {
            host_id,
            host_name,
            ca_certificate_der,
            server_name,
            native_address,
            native_port,
            access_token,
            native_transport_version,
            domain_protocol_versions,
        } => PairingServerMessage::Succeeded {
            host_id,
            host_name,
            ca_certificate_der,
            server_name,
            native_address,
            native_port,
            access_token,
            native_transport_version,
            domain_protocol_versions,
        },
        ServerDto::PairingError {
            code,
            message,
            recoverable,
        } => PairingServerMessage::Error {
            code: code.into(),
            message,
            recoverable,
        },
    };
    validate_server_message(&message)?;
    Ok(message)
}

fn validate_bundle(bundle: &PairingBundle) -> Result<(), PairingError> {
    if bundle.pairing_version != PAIRING_PROTOCOL_VERSION {
        return Err(invalid("pairing_version", "unsupported version"));
    }
    uuid_v7("pairing_id", &bundle.pairing_id)?;
    uuid_v7("host_id", &bundle.host_id)?;
    nonblank("host_name", &bundle.host_name, 80)?;
    nonblank("server_name", &bundle.server_name, 253)?;
    nonblank("address", &bundle.address, 64)?;
    if bundle.leaf_sha256.len() != 64
        || !bundle
            .leaf_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid(
            "leaf_sha256",
            "must be a lowercase SHA-256 fingerprint",
        ));
    }
    nonblank("bootstrap_token", &bundle.bootstrap_token, 128)?;
    validate_relay_endpoint(&bundle.relay_endpoint)?;
    if bundle.port == 0 {
        return Err(invalid("port", "must be non-zero"));
    }
    if bundle.expires_at_unix_seconds <= 0 {
        return Err(invalid("expires_at_unix_seconds", "must be positive"));
    }
    Ok(())
}

fn validate_relay_endpoint(endpoint: &str) -> Result<(), PairingError> {
    if endpoint.trim() != endpoint
        || endpoint.contains("//")
        || endpoint.chars().any(|character| "/?#@".contains(character))
    {
        return Err(invalid(
            "relay_endpoint",
            "must be a canonical DNS host:port authority",
        ));
    }
    let Some((host, port)) = endpoint.rsplit_once(':') else {
        return Err(invalid("relay_endpoint", "must contain one port"));
    };
    if host.contains(':')
        || host.len() > 253
        || !host.contains('.')
        || host != host.to_ascii_lowercase()
        || !host.is_ascii()
        || host.parse::<IpAddr>().is_ok()
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return Err(invalid(
            "relay_endpoint",
            "must use a canonical public ASCII DNS name",
        ));
    }
    if port.parse::<u16>().ok().is_none_or(|port| port == 0) {
        return Err(invalid("relay_endpoint", "port must be 1..65535"));
    }
    Ok(())
}

fn validate_request(request: &PairingRequest) -> Result<(), PairingError> {
    uuid_v7("pairing_id", &request.pairing_id)?;
    uuid_v7("client_id", &request.client_id)?;
    nonblank("bootstrap_token", &request.bootstrap_token, 128)?;
    nonblank("display_name", &request.display_name, 80)?;
    if let Some(version) = &request.version {
        nonblank("version", version, 64)?;
    }
    Ok(())
}

fn validate_server_message(message: &PairingServerMessage) -> Result<(), PairingError> {
    match message {
        PairingServerMessage::Pending {
            client_id,
            display_name,
        } => {
            uuid_v7("client_id", client_id)?;
            nonblank("display_name", display_name, 80)
        }
        PairingServerMessage::Succeeded {
            host_id,
            host_name,
            ca_certificate_der,
            server_name,
            native_address,
            native_port,
            access_token,
            native_transport_version,
            domain_protocol_versions,
        } => {
            uuid_v7("host_id", host_id)?;
            nonblank("host_name", host_name, 80)?;
            nonblank("ca_certificate_der", ca_certificate_der, 16 * 1024)?;
            nonblank("server_name", server_name, 253)?;
            nonblank("native_address", native_address, 64)?;
            nonblank("access_token", access_token, 128)?;
            if *native_port == 0 || *native_transport_version == 0 {
                return Err(invalid("endpoint", "port and version must be non-zero"));
            }
            if domain_protocol_versions.is_empty() {
                return Err(invalid(
                    "domain_protocol_versions",
                    "at least one version is required",
                ));
            }
            Ok(())
        }
        PairingServerMessage::Error { message, .. } => nonblank("message", message, 512),
    }
}

fn validate_version(version: u64) -> Result<(), PairingError> {
    if version == u64::from(PAIRING_PROTOCOL_VERSION) {
        Ok(())
    } else {
        Err(invalid("pairing_version", "unsupported version"))
    }
}

fn uuid_v7(field: &'static str, value: &str) -> Result<(), PairingError> {
    let value = uuid::Uuid::parse_str(value).map_err(|error| invalid(field, &error.to_string()))?;
    if value.get_version_num() != 7 {
        return Err(invalid(field, "must be UUIDv7"));
    }
    Ok(())
}

fn nonblank(field: &'static str, value: &str, maximum: usize) -> Result<(), PairingError> {
    if value.trim().is_empty() || value.len() > maximum {
        Err(invalid(field, "must be nonblank and within its size limit"))
    } else {
        Ok(())
    }
}

fn invalid(field: &'static str, reason: &str) -> PairingError {
    PairingError::InvalidField {
        field,
        reason: reason.to_owned(),
    }
}

impl From<PairingErrorCode> for ErrorCodeDto {
    fn from(value: PairingErrorCode) -> Self {
        match value {
            PairingErrorCode::InvalidRequest => Self::InvalidRequest,
            PairingErrorCode::InvalidCredential => Self::InvalidCredential,
            PairingErrorCode::Expired => Self::Expired,
            PairingErrorCode::Used => Self::Used,
            PairingErrorCode::Denied => Self::Denied,
            PairingErrorCode::Capacity => Self::Capacity,
            PairingErrorCode::Internal => Self::Internal,
        }
    }
}

impl From<ErrorCodeDto> for PairingErrorCode {
    fn from(value: ErrorCodeDto) -> Self {
        match value {
            ErrorCodeDto::InvalidRequest => Self::InvalidRequest,
            ErrorCodeDto::InvalidCredential => Self::InvalidCredential,
            ErrorCodeDto::Expired => Self::Expired,
            ErrorCodeDto::Used => Self::Used,
            ErrorCodeDto::Denied => Self::Denied,
            ErrorCodeDto::Capacity => Self::Capacity,
            ErrorCodeDto::Internal => Self::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    const PAIRING_ID: &str = "0198f142-5a00-7000-8000-000000000001";
    const HOST_ID: &str = "0198f142-5a00-7000-8000-000000000002";
    const CLIENT_ID: &str = "0198f142-5a00-7000-8000-000000000003";

    fn bundle() -> PairingBundle {
        PairingBundle {
            pairing_version: PAIRING_PROTOCOL_VERSION,
            pairing_id: PAIRING_ID.to_owned(),
            host_id: HOST_ID.to_owned(),
            host_name: "Studio Host".to_owned(),
            server_name: format!("{HOST_ID}.agentpulse.local"),
            address: "192.168.50.4".to_owned(),
            port: 49_321,
            leaf_sha256: "ab".repeat(32),
            bootstrap_token: "bootstrap-secret".to_owned(),
            relay_endpoint: "relay.example.com:2333".to_owned(),
            expires_at_unix_seconds: 4_102_444_800,
        }
    }

    #[test]
    fn pairing_uri_round_trips_canonical_bundle() -> TestResult {
        let expected = bundle();
        let uri = expected.to_uri()?;
        assert_eq!(decode_pairing_uri(&uri)?, expected);
        Ok(())
    }

    #[test]
    fn pairing_uri_rejects_unknown_fields_and_bad_fingerprint() -> TestResult {
        let json = serde_json::json!({
            "pairing_version": 1,
            "pairing_id": PAIRING_ID,
            "host_id": HOST_ID,
            "host_name": "Studio Host",
            "server_name": "host.agentpulse.local",
            "address": "192.168.50.4",
            "port": 49321,
            "leaf_sha256": "AB",
            "bootstrap_token": "secret",
            "expires_at_unix_seconds": 4102444800_i64,
            "unexpected": true,
        });
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json)?);
        assert!(decode_pairing_uri(&format!("{PAIRING_URI_PREFIX}{encoded}")).is_err());

        let mut invalid = bundle();
        invalid.leaf_sha256 = "AB".repeat(32);
        assert!(invalid.to_uri().is_err());

        let mut invalid = bundle();
        invalid.relay_endpoint = "https://relay.example.com:2333".to_owned();
        assert!(invalid.to_uri().is_err());
        Ok(())
    }

    #[test]
    fn request_and_success_messages_round_trip_strictly() -> TestResult {
        let request = PairingRequest {
            pairing_id: PAIRING_ID.to_owned(),
            bootstrap_token: "bootstrap-secret".to_owned(),
            client_id: CLIENT_ID.to_owned(),
            display_name: "Pixel".to_owned(),
            version: Some("0.1.0".to_owned()),
        };
        let encoded = encode_pairing_request(&request)?;
        assert_eq!(decode_pairing_request(&encoded)?, request);

        let success = PairingServerMessage::Succeeded {
            host_id: HOST_ID.to_owned(),
            host_name: "Studio Host".to_owned(),
            ca_certificate_der: "base64-ca".to_owned(),
            server_name: "host.agentpulse.local".to_owned(),
            native_address: "192.168.50.4".to_owned(),
            native_port: 49_320,
            access_token: "device-secret".to_owned(),
            native_transport_version: 1,
            domain_protocol_versions: vec![1],
        };
        let encoded = encode_server_message(&success)?;
        assert_eq!(decode_server_message(&encoded)?, success);

        let with_unknown = br#"{"pairing_version":1,"message":{"type":"pair_request","pairing_id":"0198f142-5a00-7000-8000-000000000001","bootstrap_token":"secret","client_id":"0198f142-5a00-7000-8000-000000000003","display_name":"Pixel","extra":true}}"#;
        assert!(decode_pairing_request(with_unknown).is_err());
        Ok(())
    }
}
