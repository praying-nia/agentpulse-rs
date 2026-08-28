//! Versioned, channel-neutral wire protocol for AgentPulse.
//!
//! The public API exchanges validated [`agentpulse_core`] values. Private wire
//! DTOs isolate the stable JSON contract from the in-memory domain model.
//!
//! # Example
//!
//! ```
//! use agentpulse_core::{
//!     NonEmptyText, ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind,
//! };
//! use agentpulse_protocol::{ProtocolMessage, decode_json, encode_json};
//!
//! let descriptor = ProviderDescriptor::new(
//!     ProviderId::new(),
//!     ProviderKind::new("codex")?,
//!     NonEmptyText::new("Codex Provider")?,
//!     ProviderCapabilities::SESSION_STATE,
//! );
//! let message = ProtocolMessage::ProviderDescriptor(descriptor);
//! let json = encode_json(&message)?;
//!
//! assert_eq!(decode_json(&json)?, message);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod codec;
mod error;
mod message;
mod v1;

pub use codec::{decode_json, encode_json};
pub use error::ProtocolError;
pub use message::ProtocolMessage;

/// The protocol version implemented by the initial JSON codec.
pub const V1_PROTOCOL_VERSION: u16 = 1;
