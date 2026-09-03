//! Runtime-neutral multi-endpoint orchestration and lifecycle hosting for AgentPulse.
//!
//! The ports define synchronous handoff boundaries only. A successful call
//! means that the receiving adapter accepted the message; it does not imply
//! completion of provider-, network-, or platform-specific I/O. Adapters may
//! hand messages to their own queues or asynchronous tasks without exposing a
//! runtime dependency through this crate. The Bridge owns heterogeneous
//! endpoints and routes only across explicit Session-to-Channel subscriptions.
//! The Runtime Host pairs each Port with an independently owned Source, controls
//! generation-scoped ingress handles, and preserves Bridge state across explicit
//! start and stop cycles.

mod bridge;
mod port;
mod registry;
mod runtime;

pub use bridge::{
    Bridge, ChannelActionError, ChannelDeliveryError, ChannelDeliveryKind, ChannelDeliveryResult,
    ChannelDiscoverySnapshot, ChannelSessionBaseline, ChannelSessionSync, DiscoveredSession,
    EndpointRegistrationError, ProviderEventError, ProviderEventOutcome, ProviderEventReport,
    ProviderHandoffKind, RoutedAgentEvent, RoutedInteractionRequest, SessionSyncOutcome,
    SubscribeOutcome, SubscriptionError, UnsubscribeOutcome,
};
pub use port::{ChannelActionSink, ChannelPort, ProviderEventSink, ProviderPort};
pub use runtime::{
    AdapterLifecycleError, AdapterLifecyclePhase, AdapterLifecycleResult, AdapterLifecycleState,
    ChannelActionHandle, ChannelActionIngressError, ChannelActionSource, ChannelSubscriptionScope,
    ProviderEventHandle, ProviderEventIngressError, ProviderEventSource, RuntimeAccessError,
    RuntimeEndpointId, RuntimeHost, RuntimeHostState, RuntimeLifecycleError,
    RuntimeLifecycleOperation, RuntimeLifecycleOutcome, RuntimeLifecycleReport,
    RuntimeRegistrationError, RuntimeSubscriptionError,
};
