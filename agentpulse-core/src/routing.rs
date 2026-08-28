//! Centralized Provider-to-Channel capability routing.

use thiserror::Error;

use crate::{
    AgentCommand, AgentEvent, AgentEventPayload, AgentSession, ChannelCapabilities,
    ChannelDescriptor, ChannelId, DomainError, InteractionRequest, InteractionResponse,
    ProviderCapabilities, ProviderDescriptor, ProviderId, SessionId,
};

/// Whether a Channel may expose an interaction as actionable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InteractionRoute {
    /// The request may be shown, but this route cannot accept a response.
    ReadOnly,
    /// The complete Provider-to-Channel route supports a response.
    Interactive,
}

/// The centralized delivery decision attached to an Agent Event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ChannelEventRoute {
    /// The event does not request Channel input.
    ObserveOnly,
    /// The event contains an interaction with the supplied route mode.
    Interaction(InteractionRoute),
}

/// An error raised when a message cannot use a Provider-to-Channel route.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum CapabilityRouteError {
    /// The Session belongs to a different Provider.
    #[error("session Provider mismatch: expected {expected}, got {actual}")]
    ProviderMismatch {
        /// The selected Provider endpoint.
        expected: ProviderId,
        /// The Provider recorded by the Session.
        actual: ProviderId,
    },

    /// A Channel-originated message names a different Channel.
    #[error("source Channel mismatch: expected {expected}, got {actual}")]
    ChannelMismatch {
        /// The selected Channel endpoint.
        expected: ChannelId,
        /// The Channel recorded by the message.
        actual: ChannelId,
    },

    /// A routed object belongs to a different Session.
    #[error("route Session mismatch: expected {expected}, got {actual}")]
    SessionMismatch {
        /// The selected Session.
        expected: SessionId,
        /// The Session recorded by the routed object.
        actual: SessionId,
    },

    /// The Provider did not declare every capability required by the operation.
    #[error(
        "Provider capability route is unavailable: required {required:?}, declared {declared:?}"
    )]
    MissingProviderCapabilities {
        /// The capabilities required by the operation.
        required: ProviderCapabilities,
        /// The capabilities declared by the Provider.
        declared: ProviderCapabilities,
    },

    /// The Channel did not declare every capability required by the operation.
    #[error(
        "Channel capability route is unavailable: required {required:?}, declared {declared:?}"
    )]
    MissingChannelCapabilities {
        /// The capabilities required by the operation.
        required: ChannelCapabilities,
        /// The capabilities declared by the Channel.
        declared: ChannelCapabilities,
    },

    /// An Interaction Response violated its correlated request contract.
    #[error("invalid interaction response: {source}")]
    InvalidInteractionResponse {
        /// The existing domain validation error.
        #[source]
        source: DomainError,
    },
}

/// Stateless centralized capability and correlation policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct CapabilityRouter;

impl CapabilityRouter {
    /// Validates that an Event belongs to the selected Provider and that the
    /// Provider declared the capability required to publish it.
    pub fn validate_provider_event(
        provider: &ProviderDescriptor,
        session: &AgentSession,
        event: &AgentEvent,
    ) -> Result<(), CapabilityRouteError> {
        Self::validate_provider_session(provider, session)?;
        Self::validate_session_id(session.id(), event.session_id())?;

        if let AgentEventPayload::SessionStarted(event_session) = event.payload() {
            Self::validate_provider_session(provider, event_session)?;
        }

        Self::validate_provider_capabilities(provider, event.required_provider_capability())
    }

    /// Decides whether an Interaction may accept a response on the complete
    /// Provider-to-Channel route.
    pub fn interaction_route(
        provider: &ProviderDescriptor,
        channel: &ChannelDescriptor,
        session: &AgentSession,
        request: &InteractionRequest,
    ) -> Result<InteractionRoute, CapabilityRouteError> {
        Self::validate_provider_session(provider, session)?;
        Self::validate_session_id(session.id(), request.session_id())?;
        Self::validate_provider_capabilities(
            provider,
            request.required_provider_request_capability(),
        )?;

        let provider_accepts_response = provider
            .capabilities()
            .contains(request.required_provider_response_capability());
        let channel_collects_response = channel
            .capabilities()
            .contains(request.required_channel_response_capability());

        if provider_accepts_response && channel_collects_response {
            Ok(InteractionRoute::Interactive)
        } else {
            Ok(InteractionRoute::ReadOnly)
        }
    }

    /// Validates a Provider Event and produces the route metadata a Channel
    /// must use instead of recomputing capability rules.
    pub fn channel_event_route(
        provider: &ProviderDescriptor,
        channel: &ChannelDescriptor,
        session: &AgentSession,
        event: &AgentEvent,
    ) -> Result<ChannelEventRoute, CapabilityRouteError> {
        Self::validate_provider_event(provider, session, event)?;

        match event.payload() {
            AgentEventPayload::InteractionRequested(request) => Ok(ChannelEventRoute::Interaction(
                Self::interaction_route(provider, channel, session, request)?,
            )),
            _ => Ok(ChannelEventRoute::ObserveOnly),
        }
    }

    /// Validates a correlated Interaction Response before it is handed to a
    /// Provider.
    pub fn validate_interaction_response(
        provider: &ProviderDescriptor,
        channel: &ChannelDescriptor,
        session: &AgentSession,
        request: &InteractionRequest,
        response: &InteractionResponse,
    ) -> Result<(), CapabilityRouteError> {
        Self::validate_provider_session(provider, session)?;
        Self::validate_session_id(session.id(), request.session_id())?;
        Self::validate_session_id(session.id(), response.session_id())?;
        Self::validate_channel_id(channel.id(), response.channel_id())?;
        request
            .validate_response(response)
            .map_err(|source| CapabilityRouteError::InvalidInteractionResponse { source })?;
        Self::validate_provider_capabilities(
            provider,
            request.required_provider_request_capability(),
        )?;
        Self::validate_provider_capabilities(
            provider,
            request.required_provider_response_capability(),
        )?;
        Self::validate_channel_capabilities(channel, request.required_channel_response_capability())
    }

    /// Validates a remote Agent Command before it is handed to a Provider.
    pub fn validate_command(
        provider: &ProviderDescriptor,
        channel: &ChannelDescriptor,
        session: &AgentSession,
        command: &AgentCommand,
    ) -> Result<(), CapabilityRouteError> {
        Self::validate_provider_session(provider, session)?;
        Self::validate_session_id(session.id(), command.session_id())?;
        Self::validate_channel_id(channel.id(), command.channel_id())?;
        Self::validate_provider_capabilities(provider, command.required_provider_capability())?;
        Self::validate_channel_capabilities(channel, command.required_channel_capabilities())
    }

    fn validate_provider_session(
        provider: &ProviderDescriptor,
        session: &AgentSession,
    ) -> Result<(), CapabilityRouteError> {
        if provider.id() != session.provider_id() {
            return Err(CapabilityRouteError::ProviderMismatch {
                expected: provider.id(),
                actual: session.provider_id(),
            });
        }
        Ok(())
    }

    fn validate_session_id(
        expected: SessionId,
        actual: SessionId,
    ) -> Result<(), CapabilityRouteError> {
        if expected != actual {
            return Err(CapabilityRouteError::SessionMismatch { expected, actual });
        }
        Ok(())
    }

    fn validate_channel_id(
        expected: ChannelId,
        actual: ChannelId,
    ) -> Result<(), CapabilityRouteError> {
        if expected != actual {
            return Err(CapabilityRouteError::ChannelMismatch { expected, actual });
        }
        Ok(())
    }

    fn validate_provider_capabilities(
        provider: &ProviderDescriptor,
        required: ProviderCapabilities,
    ) -> Result<(), CapabilityRouteError> {
        let declared = provider.capabilities();
        if !declared.contains(required) {
            return Err(CapabilityRouteError::MissingProviderCapabilities { required, declared });
        }
        Ok(())
    }

    fn validate_channel_capabilities(
        channel: &ChannelDescriptor,
        required: ChannelCapabilities,
    ) -> Result<(), CapabilityRouteError> {
        let declared = channel.capabilities();
        if !declared.contains(required) {
            return Err(CapabilityRouteError::MissingChannelCapabilities { required, declared });
        }
        Ok(())
    }
}
