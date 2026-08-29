//! Runtime-neutral multi-endpoint Provider and Channel orchestration for AgentPulse.
//!
//! The ports define synchronous handoff boundaries only. A successful call
//! means that the receiving adapter accepted the message; it does not imply
//! completion of provider-, network-, or platform-specific I/O. Adapters may
//! hand messages to their own queues or asynchronous tasks without exposing a
//! runtime dependency through this crate. The Bridge owns heterogeneous
//! endpoints and routes only across explicit Session-to-Channel subscriptions.

mod bridge;
mod port;
mod registry;

pub use bridge::{
    Bridge, ChannelActionError, ChannelDeliveryError, ChannelDeliveryKind, ChannelDeliveryResult,
    EndpointRegistrationError, ProviderEventError, ProviderEventOutcome, ProviderEventReport,
    ProviderHandoffKind, SubscribeOutcome, SubscriptionError, UnsubscribeOutcome,
};
pub use port::{ChannelActionSink, ChannelPort, ProviderEventSink, ProviderPort};
