//! Private JSON v1 DTOs and domain conversions.

mod convert;
mod dto;
mod scalar;

pub(crate) use convert::{decode_envelope, encode_envelope};
pub(crate) use dto::EnvelopeDto;
