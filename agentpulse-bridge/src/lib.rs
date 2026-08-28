//! Runtime-neutral Provider and Channel orchestration for AgentPulse.
//!
//! The ports define synchronous handoff boundaries only. A successful call
//! means that the receiving adapter accepted the message; it does not imply
//! completion of provider-, network-, or platform-specific I/O. Adapters may
//! hand messages to their own queues or asynchronous tasks without exposing a
//! runtime dependency through this crate.

mod bridge;
mod port;

pub use bridge::{
    Bridge, ChannelActionError, ChannelDeliveryKind, ProviderEventError, ProviderEventOutcome,
    ProviderHandoffKind,
};
pub use port::{ChannelActionSink, ChannelPort, ProviderEventSink, ProviderPort};
