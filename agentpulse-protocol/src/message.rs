//! Transport-independent protocol messages.

use agentpulse_core::{
    AgentCommand, AgentEvent, AgentSession, ChannelDescriptor, InteractionRequest,
    InteractionResponse, ProviderDescriptor,
};

/// A validated semantic message carried by a versioned protocol envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProtocolMessage {
    /// Describes one configured Provider endpoint.
    ProviderDescriptor(ProviderDescriptor),
    /// Describes one configured Channel endpoint.
    ChannelDescriptor(ChannelDescriptor),
    /// Carries a complete observed Agent Session snapshot.
    AgentSession(AgentSession),
    /// Carries one normalized, ordered Agent event.
    AgentEvent(AgentEvent),
    /// Carries one current Provider-originated interaction request.
    InteractionRequest(InteractionRequest),
    /// Carries a Channel-originated interaction response.
    InteractionResponse(InteractionResponse),
    /// Carries a Channel-originated command toward a Provider.
    AgentCommand(AgentCommand),
}
