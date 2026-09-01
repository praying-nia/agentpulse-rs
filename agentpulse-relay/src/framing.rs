//! Bounded length-prefixed Relay control framing.

use std::io::{Read, Write};

use crate::{RelayError, protocol::MAX_CONTROL_BYTES};

pub(crate) fn write_frame(stream: &mut impl Write, payload: &[u8]) -> Result<(), RelayError> {
    if payload.is_empty() || payload.len() > MAX_CONTROL_BYTES {
        return Err(RelayError::invalid(
            "control_frame",
            "payload must be between 1 byte and 16 KiB",
        ));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| RelayError::invalid("control_frame", "payload length overflow"))?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(|source| RelayError::io("write control-frame length", source))?;
    stream
        .write_all(payload)
        .map_err(|source| RelayError::io("write control-frame payload", source))?;
    stream
        .flush()
        .map_err(|source| RelayError::io("flush control frame", source))
}

pub(crate) fn read_frame(stream: &mut impl Read) -> Result<Vec<u8>, RelayError> {
    let mut length = [0_u8; 4];
    stream
        .read_exact(&mut length)
        .map_err(|source| RelayError::io("read control-frame length", source))?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|_| RelayError::invalid("control_frame", "payload length overflow"))?;
    if length == 0 || length > MAX_CONTROL_BYTES {
        return Err(RelayError::invalid(
            "control_frame",
            "payload must be between 1 byte and 16 KiB",
        ));
    }
    let mut payload = vec![0_u8; length];
    stream
        .read_exact(&mut payload)
        .map_err(|source| RelayError::io("read control-frame payload", source))?;
    Ok(payload)
}
