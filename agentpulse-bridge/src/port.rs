//! Independent Provider and Channel port contracts.

use std::error::Error;

use agentpulse_core::{
    AgentCommand, AgentEvent, AgentSession, ChannelDescriptor, ChannelEventRoute, ChannelId,
    InteractionResponse, ProviderDescriptor, ProviderId,
};

use crate::{ChannelSessionBaseline, ChannelSessionSync};

/// Adapter boundary for sending validated user actions toward an AI agent.
///
/// Implementations must treat successful calls as message acceptance rather
/// than a guarantee that provider-specific I/O has completed.
pub trait ProviderPort: Send {
    /// The adapter-specific handoff error.
    type Error: Error + Send + Sync + 'static;

    /// Returns this configured Provider instance and its declared capabilities.
    fn descriptor(&self) -> &ProviderDescriptor;

    /// Accepts a centrally validated Interaction Response.
    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), Self::Error>;

    /// Accepts a centrally validated Agent Command.
    fn accept_command(&mut self, command: AgentCommand) -> Result<(), Self::Error>;
}

/// Bridge-facing publication boundary used by a Provider adapter.
///
/// The explicit Provider identifier lets the Bridge resolve the registered
/// endpoint and reject events for a Session owned by another Provider.
pub trait ProviderEventSink: Send {
    /// The event handoff error.
    type Error: Error + Send + Sync + 'static;

    /// Publishes one normalized Agent Event from a configured Provider.
    fn publish_event(
        &mut self,
        provider_id: ProviderId,
        event: AgentEvent,
    ) -> Result<(), Self::Error>;
}

/// Adapter boundary for delivering normalized state and events to users.
///
/// Capability decisions are supplied by Core through [`ChannelEventRoute`]; a
/// Channel must not reconstruct Provider-to-Channel executability rules.
pub trait ChannelPort: Send {
    /// The adapter-specific handoff error.
    type Error: Error + Send + Sync + 'static;

    /// Returns this configured Channel instance and its declared capabilities.
    fn descriptor(&self) -> &ChannelDescriptor;

    /// Accepts one normalized Event and its centralized Channel route.
    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), Self::Error>;

    /// Accepts a complete current Session view.
    fn deliver_session(&mut self, session: AgentSession) -> Result<(), Self::Error>;

    /// Accepts one atomic subscription baseline containing the Session and all
    /// interactions pending at the baseline cursor.
    fn deliver_session_baseline(
        &mut self,
        baseline: ChannelSessionBaseline,
    ) -> Result<(), Self::Error> {
        self.deliver_session(baseline.into_session())
    }

    /// Accepts one bounded historical page and an optional final live baseline.
    fn deliver_session_sync(&mut self, sync: ChannelSessionSync) -> Result<(), Self::Error> {
        for routed in sync.events() {
            self.deliver_event(routed.event().clone(), routed.route())?;
        }
        if let Some(baseline) = sync.baseline().cloned() {
            self.deliver_session_baseline(baseline)?;
        }
        Ok(())
    }
}

/// Bridge-facing submission boundary used by a Channel adapter.
///
/// The Bridge must require an active Session subscription and revalidate every
/// submitted action before forwarding it to the Session's owning Provider,
/// even when an earlier delivery was marked interactive.
pub trait ChannelActionSink: Send {
    /// The action handoff error.
    type Error: Error + Send + Sync + 'static;

    /// Submits an Interaction Response from a configured Channel.
    fn submit_interaction_response(
        &mut self,
        channel_id: ChannelId,
        response: InteractionResponse,
    ) -> Result<(), Self::Error>;

    /// Submits an Agent Command from a configured Channel.
    fn submit_command(
        &mut self,
        channel_id: ChannelId,
        command: AgentCommand,
    ) -> Result<(), Self::Error>;
}
