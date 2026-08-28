//! Minimal runtime-neutral Bridge orchestration.

use std::{collections::BTreeMap, error::Error, fmt};

use agentpulse_core::{
    AgentCommand, AgentEvent, AgentEventPayload, ApplyOutcome, CapabilityRouteError,
    CapabilityRouter, ChannelCapabilities, ChannelDescriptor, ChannelId, InteractionId,
    InteractionResponse, ProviderDescriptor, ProviderId, ReduceError, SessionAggregate,
    SessionAggregateConfig, SessionId,
};

use crate::{ChannelActionSink, ChannelPort, ProviderEventSink, ProviderPort};

/// The result of accepting one Provider Event into the Bridge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProviderEventOutcome {
    /// The Event advanced the Session Aggregate and was accepted by the Channel.
    Applied,
    /// The Event was an exact retry of the current Aggregate cursor.
    AlreadyApplied,
}

/// Identifies the Channel handoff that rejected an applied Event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ChannelDeliveryKind {
    /// Delivery of the normalized Event and its centralized route.
    Event,
    /// Delivery of the latest complete Session view.
    Session,
}

impl fmt::Display for ChannelDeliveryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Event => formatter.write_str("event"),
            Self::Session => formatter.write_str("session view"),
        }
    }
}

/// Identifies the Provider handoff that rejected a validated Channel Action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProviderHandoffKind {
    /// Handoff of a correlated Interaction Response.
    InteractionResponse,
    /// Handoff of an Agent Command.
    Command,
}

impl fmt::Display for ProviderHandoffKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InteractionResponse => formatter.write_str("interaction response"),
            Self::Command => formatter.write_str("command"),
        }
    }
}

/// An error raised while processing a Provider Event through the Bridge.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProviderEventError<E> {
    /// The publishing adapter is not the Provider registered with this Bridge.
    SourceProviderMismatch {
        /// The registered Provider.
        expected: ProviderId,
        /// The Provider named by the publication.
        actual: ProviderId,
    },

    /// An Event for an unknown Session was not a valid initial Event.
    SessionNotStarted {
        /// The unknown Session.
        session_id: SessionId,
    },

    /// Centralized endpoint or capability validation rejected the Event.
    CapabilityRoute(CapabilityRouteError),

    /// The Session Reducer rejected the Event.
    Reduce(ReduceError),

    /// The Channel rejected an Event or Session-view handoff.
    ChannelHandoff {
        /// The selected Channel.
        channel_id: ChannelId,
        /// The handoff that failed.
        delivery: ChannelDeliveryKind,
        /// The adapter-defined handoff error.
        source: E,
    },
}

impl<E: Error> fmt::Display for ProviderEventError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceProviderMismatch { expected, actual } => write!(
                formatter,
                "event source Provider mismatch: expected {expected}, got {actual}"
            ),
            Self::SessionNotStarted { session_id } => write!(
                formatter,
                "session {session_id} has not been started by a sequence-one SessionStarted event"
            ),
            Self::CapabilityRoute(source) => source.fmt(formatter),
            Self::Reduce(source) => source.fmt(formatter),
            Self::ChannelHandoff {
                channel_id,
                delivery,
                source,
            } => write!(
                formatter,
                "Channel {channel_id} rejected {delivery} handoff: {source}"
            ),
        }
    }
}

impl<E: Error + 'static> Error for ProviderEventError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CapabilityRoute(source) => Some(source),
            Self::Reduce(source) => Some(source),
            Self::ChannelHandoff { source, .. } => Some(source),
            Self::SourceProviderMismatch { .. } | Self::SessionNotStarted { .. } => None,
        }
    }
}

impl<E> From<CapabilityRouteError> for ProviderEventError<E> {
    fn from(source: CapabilityRouteError) -> Self {
        Self::CapabilityRoute(source)
    }
}

impl<E> From<ReduceError> for ProviderEventError<E> {
    fn from(source: ReduceError) -> Self {
        Self::Reduce(source)
    }
}

/// An error raised while processing a Channel Action through the Bridge.
#[derive(Debug)]
#[non_exhaustive]
pub enum ChannelActionError<E> {
    /// The submitting adapter is not the Channel registered with this Bridge.
    SourceChannelMismatch {
        /// The registered Channel.
        expected: ChannelId,
        /// The Channel named by the submission.
        actual: ChannelId,
    },

    /// The Action targets a Session unknown to this Bridge.
    SessionNotFound {
        /// The unknown Session.
        session_id: SessionId,
    },

    /// An Interaction Response does not name a currently pending request.
    InteractionNotPending {
        /// The owning Session.
        session_id: SessionId,
        /// The missing pending Interaction.
        interaction_id: InteractionId,
    },

    /// Centralized endpoint, correlation, or capability validation rejected the Action.
    CapabilityRoute(CapabilityRouteError),

    /// The Provider rejected a validated Action handoff.
    ProviderHandoff {
        /// The selected Provider.
        provider_id: ProviderId,
        /// The handoff that failed.
        handoff: ProviderHandoffKind,
        /// The adapter-defined handoff error.
        source: E,
    },
}

impl<E: Error> fmt::Display for ChannelActionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceChannelMismatch { expected, actual } => write!(
                formatter,
                "action source Channel mismatch: expected {expected}, got {actual}"
            ),
            Self::SessionNotFound { session_id } => {
                write!(
                    formatter,
                    "session {session_id} is not registered with the Bridge"
                )
            }
            Self::InteractionNotPending {
                session_id,
                interaction_id,
            } => write!(
                formatter,
                "interaction {interaction_id} is not pending for session {session_id}"
            ),
            Self::CapabilityRoute(source) => source.fmt(formatter),
            Self::ProviderHandoff {
                provider_id,
                handoff,
                source,
            } => write!(
                formatter,
                "Provider {provider_id} rejected {handoff} handoff: {source}"
            ),
        }
    }
}

impl<E: Error + 'static> Error for ChannelActionError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CapabilityRoute(source) => Some(source),
            Self::ProviderHandoff { source, .. } => Some(source),
            Self::SourceChannelMismatch { .. }
            | Self::SessionNotFound { .. }
            | Self::InteractionNotPending { .. } => None,
        }
    }
}

impl<E> From<CapabilityRouteError> for ChannelActionError<E> {
    fn from(source: CapabilityRouteError) -> Self {
        Self::CapabilityRoute(source)
    }
}

/// A synchronous, in-memory Provider-to-Channel orchestration loop.
///
/// One Bridge instance owns one Provider Port and one Channel Port while
/// maintaining any number of Session Aggregates for that route. Descriptor
/// snapshots and capabilities are fixed when the Bridge is constructed.
pub struct Bridge<P, C> {
    provider: P,
    channel: C,
    provider_descriptor: ProviderDescriptor,
    channel_descriptor: ChannelDescriptor,
    session_config: SessionAggregateConfig,
    sessions: BTreeMap<SessionId, SessionAggregate>,
}

impl<P, C> Bridge<P, C>
where
    P: ProviderPort,
    C: ChannelPort,
{
    /// Registers one Provider and one Channel using default Aggregate settings.
    #[must_use]
    pub fn new(provider: P, channel: C) -> Self {
        Self::with_session_config(provider, channel, SessionAggregateConfig::default())
    }

    /// Registers one Provider and one Channel using explicit Aggregate settings.
    #[must_use]
    pub fn with_session_config(
        provider: P,
        channel: C,
        session_config: SessionAggregateConfig,
    ) -> Self {
        let provider_descriptor = provider.descriptor().clone();
        let channel_descriptor = channel.descriptor().clone();
        Self {
            provider,
            channel,
            provider_descriptor,
            channel_descriptor,
            session_config,
            sessions: BTreeMap::new(),
        }
    }

    /// Borrows the registered Provider Descriptor snapshot.
    #[must_use]
    pub const fn provider_descriptor(&self) -> &ProviderDescriptor {
        &self.provider_descriptor
    }

    /// Borrows the registered Channel Descriptor snapshot.
    #[must_use]
    pub const fn channel_descriptor(&self) -> &ChannelDescriptor {
        &self.channel_descriptor
    }

    /// Returns the Aggregate settings used for newly observed Sessions.
    #[must_use]
    pub const fn session_config(&self) -> SessionAggregateConfig {
        self.session_config
    }

    /// Borrows the current Aggregate for a Session.
    #[must_use]
    pub fn session_aggregate(&self, session_id: SessionId) -> Option<&SessionAggregate> {
        self.sessions.get(&session_id)
    }

    /// Iterates over current Session Aggregates in stable Session-ID order.
    pub fn session_aggregates(&self) -> impl ExactSizeIterator<Item = &SessionAggregate> {
        self.sessions.values()
    }

    /// Borrows the registered Provider Port.
    #[must_use]
    pub const fn provider(&self) -> &P {
        &self.provider
    }

    /// Borrows the registered Channel Port.
    #[must_use]
    pub const fn channel(&self) -> &C {
        &self.channel
    }

    /// Consumes the Bridge and returns its registered Ports.
    #[must_use]
    pub fn into_ports(self) -> (P, C) {
        (self.provider, self.channel)
    }

    /// Processes one normalized Event from the registered Provider.
    ///
    /// Successful reduction is retained before Channel handoff. Therefore a
    /// Channel error does not roll back the current Aggregate.
    pub fn handle_provider_event(
        &mut self,
        provider_id: ProviderId,
        event: AgentEvent,
    ) -> Result<ProviderEventOutcome, ProviderEventError<C::Error>> {
        if provider_id != self.provider_descriptor.id() {
            return Err(ProviderEventError::SourceProviderMismatch {
                expected: self.provider_descriptor.id(),
                actual: provider_id,
            });
        }

        let session_id = event.session_id();
        let route = if let Some(aggregate) = self.sessions.get(&session_id) {
            CapabilityRouter::channel_event_route(
                &self.provider_descriptor,
                &self.channel_descriptor,
                aggregate.session(),
                &event,
            )?
        } else {
            let AgentEventPayload::SessionStarted(session) = event.payload() else {
                return Err(ProviderEventError::SessionNotStarted { session_id });
            };
            CapabilityRouter::channel_event_route(
                &self.provider_descriptor,
                &self.channel_descriptor,
                session,
                &event,
            )?
        };

        let session_view = if let Some(aggregate) = self.sessions.get_mut(&session_id) {
            let outcome = aggregate.apply(event.clone())?;
            if matches!(outcome, ApplyOutcome::AlreadyApplied) {
                return Ok(ProviderEventOutcome::AlreadyApplied);
            }
            session_view_for_event(&self.channel_descriptor, &event)
                .then(|| aggregate.session().clone())
        } else {
            let aggregate = SessionAggregate::from_initial_event_with_config(
                event.clone(),
                self.session_config,
            )?;
            let session_view = session_view_for_event(&self.channel_descriptor, &event)
                .then(|| aggregate.session().clone());
            let _ = self.sessions.insert(session_id, aggregate);
            session_view
        };

        self.channel.deliver_event(event, route).map_err(|source| {
            ProviderEventError::ChannelHandoff {
                channel_id: self.channel_descriptor.id(),
                delivery: ChannelDeliveryKind::Event,
                source,
            }
        })?;

        if let Some(session) = session_view {
            self.channel.deliver_session(session).map_err(|source| {
                ProviderEventError::ChannelHandoff {
                    channel_id: self.channel_descriptor.id(),
                    delivery: ChannelDeliveryKind::Session,
                    source,
                }
            })?;
        }

        Ok(ProviderEventOutcome::Applied)
    }

    /// Validates and hands one Interaction Response to the registered Provider.
    pub fn handle_interaction_response(
        &mut self,
        channel_id: ChannelId,
        response: InteractionResponse,
    ) -> Result<(), ChannelActionError<P::Error>> {
        self.validate_source_channel(channel_id)?;

        let session_id = response.session_id();
        let aggregate = self
            .sessions
            .get(&session_id)
            .ok_or(ChannelActionError::SessionNotFound { session_id })?;
        let interaction_id = response.request_id();
        let request = aggregate.pending_interaction(interaction_id).ok_or(
            ChannelActionError::InteractionNotPending {
                session_id,
                interaction_id,
            },
        )?;

        CapabilityRouter::validate_interaction_response(
            &self.provider_descriptor,
            &self.channel_descriptor,
            aggregate.session(),
            request,
            &response,
        )?;

        self.provider
            .accept_interaction_response(response)
            .map_err(|source| ChannelActionError::ProviderHandoff {
                provider_id: self.provider_descriptor.id(),
                handoff: ProviderHandoffKind::InteractionResponse,
                source,
            })
    }

    /// Validates and hands one Agent Command to the registered Provider.
    pub fn handle_command(
        &mut self,
        channel_id: ChannelId,
        command: AgentCommand,
    ) -> Result<(), ChannelActionError<P::Error>> {
        self.validate_source_channel(channel_id)?;

        let session_id = command.session_id();
        let aggregate = self
            .sessions
            .get(&session_id)
            .ok_or(ChannelActionError::SessionNotFound { session_id })?;

        CapabilityRouter::validate_command(
            &self.provider_descriptor,
            &self.channel_descriptor,
            aggregate.session(),
            &command,
        )?;

        self.provider.accept_command(command).map_err(|source| {
            ChannelActionError::ProviderHandoff {
                provider_id: self.provider_descriptor.id(),
                handoff: ProviderHandoffKind::Command,
                source,
            }
        })
    }

    fn validate_source_channel(
        &self,
        channel_id: ChannelId,
    ) -> Result<(), ChannelActionError<P::Error>> {
        if channel_id != self.channel_descriptor.id() {
            return Err(ChannelActionError::SourceChannelMismatch {
                expected: self.channel_descriptor.id(),
                actual: channel_id,
            });
        }
        Ok(())
    }
}

impl<P, C> ProviderEventSink for Bridge<P, C>
where
    P: ProviderPort,
    C: ChannelPort,
{
    type Error = ProviderEventError<C::Error>;

    fn publish_event(
        &mut self,
        provider_id: ProviderId,
        event: AgentEvent,
    ) -> Result<(), Self::Error> {
        let _ = self.handle_provider_event(provider_id, event)?;
        Ok(())
    }
}

impl<P, C> ChannelActionSink for Bridge<P, C>
where
    P: ProviderPort,
    C: ChannelPort,
{
    type Error = ChannelActionError<P::Error>;

    fn submit_interaction_response(
        &mut self,
        channel_id: ChannelId,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        self.handle_interaction_response(channel_id, response)
    }

    fn submit_command(
        &mut self,
        channel_id: ChannelId,
        command: AgentCommand,
    ) -> Result<(), Self::Error> {
        self.handle_command(channel_id, command)
    }
}

fn session_view_for_event(channel: &ChannelDescriptor, event: &AgentEvent) -> bool {
    channel
        .capabilities()
        .contains(ChannelCapabilities::SESSION_VIEW)
        && matches!(
            event.payload(),
            AgentEventPayload::SessionStarted(_)
                | AgentEventPayload::StateChanged(_)
                | AgentEventPayload::ConnectionChanged(_)
                | AgentEventPayload::SessionEnded(_)
        )
}
