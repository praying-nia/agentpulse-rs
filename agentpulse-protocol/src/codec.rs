//! JSON codec entry points and protocol-version dispatch.

use serde_json::Value;

use crate::{
    ProtocolError, ProtocolMessage, V1_PROTOCOL_VERSION,
    v1::{EnvelopeDto, decode_envelope, encode_envelope},
};

/// Encodes a validated semantic message in the canonical JSON v1 envelope.
pub fn encode_json(message: &ProtocolMessage) -> Result<Vec<u8>, ProtocolError> {
    let envelope = encode_envelope(message)?;
    serde_json::to_vec(&envelope).map_err(|source| ProtocolError::JsonEncode { source })
}

/// Decodes one strict JSON v1 envelope into validated Core domain values.
pub fn decode_json(input: &[u8]) -> Result<ProtocolMessage, ProtocolError> {
    let value: Value =
        serde_json::from_slice(input).map_err(|source| ProtocolError::JsonDecode { source })?;
    let received = value
        .get("protocol_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| ProtocolError::InvalidWireValue {
            field: "protocol_version",
            reason: "expected an unsigned JSON integer".to_owned(),
        })?;

    if received != u64::from(V1_PROTOCOL_VERSION) {
        return Err(ProtocolError::UnsupportedProtocolVersion {
            received,
            supported: V1_PROTOCOL_VERSION,
        });
    }

    let envelope: EnvelopeDto =
        serde_json::from_value(value).map_err(|source| ProtocolError::JsonDecode { source })?;
    decode_envelope(envelope)
}
