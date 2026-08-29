//! Runtime-neutral multi-endpoint Bridge orchestration.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use agentpulse_core::{
    AgentCommand, AgentEvent, AgentEventPayload, AgentSession, ApplyOutcome, CapabilityRouteError,
    CapabilityRouter, ChannelCapabilities, ChannelDescriptor, ChannelId, EventSequence,
    InteractionId, InteractionResponse, ProviderDescriptor, ProviderId, ReduceError,
    SessionAggregate, SessionAggregateConfig, SessionId,
};

use crate::{
    ChannelActionSink, ChannelPort, ProviderEventSink, ProviderPort,
    registry::{BoxAdapterError, RegisteredChannel, RegisteredProvider},
};

/// The result of accepting one Provider Event into the Bridge state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProviderEventOutcome {
    /// The Event advanced the Session Aggregate.
    Applied,
    /// The Event was an exact retry of the current Aggregate cursor.
    AlreadyApplied,
}

/// Identifies one Channel handoff phase.
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

/// A failed Event or Session-view handoff to one Channel.
#[derive(Debug)]
pub struct ChannelDeliveryError {
    channel_id: ChannelId,
    delivery: ChannelDeliveryKind,
    source: BoxAdapterError,
}

impl ChannelDeliveryError {
    fn new(channel_id: ChannelId, delivery: ChannelDeliveryKind, source: BoxAdapterError) -> Self {
        Self {
            channel_id,
            delivery,
            source,
        }
    }

    /// Returns the target Channel.
    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// Returns the handoff phase that failed.
    #[must_use]
    pub const fn delivery(&self) -> ChannelDeliveryKind {
        self.delivery
    }
}

impl fmt::Display for ChannelDeliveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Channel {} rejected {} handoff: {}",
            self.channel_id, self.delivery, self.source
        )
    }
}

impl Error for ChannelDeliveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// The result of delivering one applied Event to one subscribed Channel.
#[derive(Debug)]
#[non_exhaustive]
pub enum ChannelDeliveryResult {
    /// Every handoff required for this Channel was accepted.
    Delivered {
        /// The target Channel.
        channel_id: ChannelId,
    },
    /// One handoff for this Channel failed.
    Failed(ChannelDeliveryError),
}

impl ChannelDeliveryResult {
    /// Returns the target Channel.
    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        match self {
            Self::Delivered { channel_id } => *channel_id,
            Self::Failed(error) => error.channel_id(),
        }
    }

    /// Returns whether every required handoff was accepted.
    #[must_use]
    pub const fn is_delivered(&self) -> bool {
        matches!(self, Self::Delivered { .. })
    }

    /// Borrows the handoff error, when this target failed.
    #[must_use]
    pub const fn error(&self) -> Option<&ChannelDeliveryError> {
        match self {
            Self::Delivered { .. } => None,
            Self::Failed(error) => Some(error),
        }
    }
}

/// The complete result of processing one Provider Event.
#[derive(Debug)]
pub struct ProviderEventReport {
    outcome: ProviderEventOutcome,
    deliveries: Vec<ChannelDeliveryResult>,
}

impl ProviderEventReport {
    fn new(outcome: ProviderEventOutcome, deliveries: Vec<ChannelDeliveryResult>) -> Self {
        Self {
            outcome,
            deliveries,
        }
    }

    /// Returns whether the Event advanced state or was already applied.
    #[must_use]
    pub const fn outcome(&self) -> ProviderEventOutcome {
        self.outcome
    }

    /// Borrows per-Channel results in stable Channel-ID order.
    #[must_use]
    pub fn deliveries(&self) -> &[ChannelDeliveryResult] {
        &self.deliveries
    }

    /// Returns whether at least one subscribed Channel handoff failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.deliveries
            .iter()
            .any(|delivery| !delivery.is_delivered())
    }

    fn first_failure(&self) -> Option<&ChannelDeliveryError> {
        self.deliveries
            .iter()
            .find_map(ChannelDeliveryResult::error)
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

/// An error raised while registering an endpoint with the Bridge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EndpointRegistrationError {
    /// A Provider with the same strongly typed ID is already registered.
    ProviderAlreadyRegistered {
        /// The duplicate Provider ID.
        provider_id: ProviderId,
    },
    /// A Channel with the same strongly typed ID is already registered.
    ChannelAlreadyRegistered {
        /// The duplicate Channel ID.
        channel_id: ChannelId,
    },
}

impl fmt::Display for EndpointRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderAlreadyRegistered { provider_id } => {
                write!(formatter, "Provider {provider_id} is already registered")
            }
            Self::ChannelAlreadyRegistered { channel_id } => {
                write!(formatter, "Channel {channel_id} is already registered")
            }
        }
    }
}

impl Error for EndpointRegistrationError {}

/// The result of subscribing one Channel to one existing Session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SubscribeOutcome {
    /// A new subscription became active.
    Subscribed {
        /// Whether the Channel first accepted the current Session view.
        session_view_delivered: bool,
        /// The last Event already represented by the synchronized baseline.
        baseline_sequence: EventSequence,
    },
    /// The exact subscription was already active; no view was redelivered.
    AlreadySubscribed {
        /// The current Aggregate cursor at the time of the repeated request.
        current_sequence: EventSequence,
    },
}

/// One current Session entry exposed to a Channel during discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredSession {
    session: AgentSession,
    last_sequence: EventSequence,
}

impl DiscoveredSession {
    fn new(session: AgentSession, last_sequence: EventSequence) -> Self {
        Self {
            session,
            last_sequence,
        }
    }

    /// Borrows the current complete Session view.
    #[must_use]
    pub const fn session(&self) -> &AgentSession {
        &self.session
    }

    /// Returns the last Event represented by the current Aggregate.
    #[must_use]
    pub const fn last_sequence(&self) -> EventSequence {
        self.last_sequence
    }
}

/// A stable point-in-time discovery snapshot for a registered Channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelDiscoverySnapshot {
    providers: Vec<ProviderDescriptor>,
    sessions: Vec<DiscoveredSession>,
}

impl ChannelDiscoverySnapshot {
    fn new(providers: Vec<ProviderDescriptor>, sessions: Vec<DiscoveredSession>) -> Self {
        Self {
            providers,
            sessions,
        }
    }

    /// Borrows Provider descriptors in stable Provider-ID order.
    #[must_use]
    pub fn providers(&self) -> &[ProviderDescriptor] {
        &self.providers
    }

    /// Borrows current Sessions in stable Session-ID order.
    #[must_use]
    pub fn sessions(&self) -> &[DiscoveredSession] {
        &self.sessions
    }
}

/// The result of cancelling one Session-to-Channel subscription.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UnsubscribeOutcome {
    /// The active subscription was removed.
    Unsubscribed,
    /// The requested subscription was not active.
    NotSubscribed,
}

/// An error raised while changing a Session-to-Channel subscription.
#[derive(Debug)]
#[non_exhaustive]
pub enum SubscriptionError {
    /// The requested Channel is not registered.
    ChannelNotFound {
        /// The unknown Channel.
        channel_id: ChannelId,
    },
    /// The requested Session has not been started.
    SessionNotFound {
        /// The unknown Session.
        session_id: SessionId,
    },
    /// The Channel rejected the initial current Session view.
    InitialSessionHandoff(ChannelDeliveryError),
}

impl fmt::Display for SubscriptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelNotFound { channel_id } => {
                write!(formatter, "Channel {channel_id} is not registered")
            }
            Self::SessionNotFound { session_id } => {
                write!(
                    formatter,
                    "session {session_id} is not registered with the Bridge"
                )
            }
            Self::InitialSessionHandoff(source) => {
                write!(
                    formatter,
                    "initial subscription synchronization failed: {source}"
                )
            }
        }
    }
}

impl Error for SubscriptionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InitialSessionHandoff(source) => Some(source),
            Self::ChannelNotFound { .. } | Self::SessionNotFound { .. } => None,
        }
    }
}

/// An error raised while processing a Provider Event through the Bridge.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProviderEventError {
    /// The publishing Provider is not registered.
    ProviderNotFound {
        /// The unknown Provider.
        provider_id: ProviderId,
    },
    /// A subscribed Channel disappeared before route computation.
    ChannelNotFound {
        /// The unavailable Channel.
        channel_id: ChannelId,
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
    /// At least one Channel handoff failed after state was advanced.
    ChannelHandoffs {
        /// All attempted deliveries, including successful targets.
        report: ProviderEventReport,
    },
}

impl ProviderEventError {
    /// Borrows the complete fan-out report for a partial handoff failure.
    #[must_use]
    pub const fn report(&self) -> Option<&ProviderEventReport> {
        match self {
            Self::ChannelHandoffs { report } => Some(report),
            _ => None,
        }
    }
}

impl fmt::Display for ProviderEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderNotFound { provider_id } => {
                write!(formatter, "Provider {provider_id} is not registered")
            }
            Self::ChannelNotFound { channel_id } => {
                write!(
                    formatter,
                    "subscribed Channel {channel_id} is not registered"
                )
            }
            Self::SessionNotStarted { session_id } => write!(
                formatter,
                "session {session_id} has not been started by a sequence-one SessionStarted event"
            ),
            Self::CapabilityRoute(source) => source.fmt(formatter),
            Self::Reduce(source) => source.fmt(formatter),
            Self::ChannelHandoffs { report } => write!(
                formatter,
                "{} of {} Channel handoffs failed",
                report
                    .deliveries()
                    .iter()
                    .filter(|delivery| !delivery.is_delivered())
                    .count(),
                report.deliveries().len()
            ),
        }
    }
}

impl Error for ProviderEventError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CapabilityRoute(source) => Some(source),
            Self::Reduce(source) => Some(source),
            Self::ChannelHandoffs { report } => report
                .first_failure()
                .map(|source| source as &(dyn Error + 'static)),
            Self::ProviderNotFound { .. }
            | Self::ChannelNotFound { .. }
            | Self::SessionNotStarted { .. } => None,
        }
    }
}

impl From<CapabilityRouteError> for ProviderEventError {
    fn from(source: CapabilityRouteError) -> Self {
        Self::CapabilityRoute(source)
    }
}

impl From<ReduceError> for ProviderEventError {
    fn from(source: ReduceError) -> Self {
        Self::Reduce(source)
    }
}

/// An error raised while processing a Channel Action through the Bridge.
#[derive(Debug)]
#[non_exhaustive]
pub enum ChannelActionError {
    /// The submitting Channel is not registered.
    ChannelNotFound {
        /// The unknown Channel.
        channel_id: ChannelId,
    },
    /// The Action targets a Session unknown to this Bridge.
    SessionNotFound {
        /// The unknown Session.
        session_id: SessionId,
    },
    /// The submitting Channel is not subscribed to the target Session.
    ChannelNotSubscribed {
        /// The source Channel.
        channel_id: ChannelId,
        /// The target Session.
        session_id: SessionId,
    },
    /// The Provider owning the target Session is not registered.
    ProviderNotFound {
        /// The unavailable Provider.
        provider_id: ProviderId,
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
        source: Box<dyn Error + Send + Sync + 'static>,
    },
}

impl fmt::Display for ChannelActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelNotFound { channel_id } => {
                write!(formatter, "Channel {channel_id} is not registered")
            }
            Self::SessionNotFound { session_id } => {
                write!(
                    formatter,
                    "session {session_id} is not registered with the Bridge"
                )
            }
            Self::ChannelNotSubscribed {
                channel_id,
                session_id,
            } => write!(
                formatter,
                "Channel {channel_id} is not subscribed to session {session_id}"
            ),
            Self::ProviderNotFound { provider_id } => {
                write!(formatter, "Provider {provider_id} is not registered")
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

impl Error for ChannelActionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CapabilityRoute(source) => Some(source),
            Self::ProviderHandoff { source, .. } => Some(source.as_ref()),
            Self::ChannelNotFound { .. }
            | Self::SessionNotFound { .. }
            | Self::ChannelNotSubscribed { .. }
            | Self::ProviderNotFound { .. }
            | Self::InteractionNotPending { .. } => None,
        }
    }
}

impl From<CapabilityRouteError> for ChannelActionError {
    fn from(source: CapabilityRouteError) -> Self {
        Self::CapabilityRoute(source)
    }
}

/// A synchronous, in-memory multi-endpoint orchestration service.
///
/// A Bridge owns heterogeneous Provider and Channel Ports, snapshots their
/// descriptors at explicit registration, and maintains any number of Session
/// Aggregates. Channel delivery and reverse Actions require an active explicit
/// Session-to-Channel subscription.
pub struct Bridge {
    providers: BTreeMap<ProviderId, RegisteredProvider>,
    channels: BTreeMap<ChannelId, RegisteredChannel>,
    session_config: SessionAggregateConfig,
    sessions: BTreeMap<SessionId, SessionAggregate>,
    subscriptions: BTreeMap<SessionId, BTreeSet<ChannelId>>,
}

impl Bridge {
    /// Creates an empty Bridge using default Aggregate settings.
    #[must_use]
    pub fn new() -> Self {
        Self::with_session_config(SessionAggregateConfig::default())
    }

    /// Creates an empty Bridge using explicit Aggregate settings.
    #[must_use]
    pub fn with_session_config(session_config: SessionAggregateConfig) -> Self {
        Self {
            providers: BTreeMap::new(),
            channels: BTreeMap::new(),
            session_config,
            sessions: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
        }
    }

    /// Registers one Provider and snapshots its Descriptor.
    pub fn register_provider<P>(&mut self, provider: P) -> Result<(), EndpointRegistrationError>
    where
        P: ProviderPort + 'static,
    {
        let descriptor = provider.descriptor().clone();
        let provider_id = descriptor.id();
        if self.providers.contains_key(&provider_id) {
            return Err(EndpointRegistrationError::ProviderAlreadyRegistered { provider_id });
        }
        let _ = self
            .providers
            .insert(provider_id, RegisteredProvider::new(provider, descriptor));
        Ok(())
    }

    /// Registers one Channel and snapshots its Descriptor.
    pub fn register_channel<C>(&mut self, channel: C) -> Result<(), EndpointRegistrationError>
    where
        C: ChannelPort + 'static,
    {
        let descriptor = channel.descriptor().clone();
        let channel_id = descriptor.id();
        if self.channels.contains_key(&channel_id) {
            return Err(EndpointRegistrationError::ChannelAlreadyRegistered { channel_id });
        }
        let _ = self
            .channels
            .insert(channel_id, RegisteredChannel::new(channel, descriptor));
        Ok(())
    }

    /// Borrows a registered Provider Descriptor snapshot by strongly typed ID.
    #[must_use]
    pub fn provider_descriptor(&self, provider_id: ProviderId) -> Option<&ProviderDescriptor> {
        self.providers
            .get(&provider_id)
            .map(RegisteredProvider::descriptor)
    }

    /// Iterates over Provider Descriptor snapshots in stable Provider-ID order.
    pub fn provider_descriptors(&self) -> impl ExactSizeIterator<Item = &ProviderDescriptor> {
        self.providers.values().map(RegisteredProvider::descriptor)
    }

    /// Borrows a registered Channel Descriptor snapshot by strongly typed ID.
    #[must_use]
    pub fn channel_descriptor(&self, channel_id: ChannelId) -> Option<&ChannelDescriptor> {
        self.channels
            .get(&channel_id)
            .map(RegisteredChannel::descriptor)
    }

    /// Iterates over Channel Descriptor snapshots in stable Channel-ID order.
    pub fn channel_descriptors(&self) -> impl ExactSizeIterator<Item = &ChannelDescriptor> {
        self.channels.values().map(RegisteredChannel::descriptor)
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

    /// Captures registered Providers and current Sessions in stable ID order.
    #[must_use]
    pub fn discovery_snapshot(&self) -> ChannelDiscoverySnapshot {
        ChannelDiscoverySnapshot::new(
            self.provider_descriptors().cloned().collect(),
            self.session_aggregates()
                .map(|aggregate| {
                    DiscoveredSession::new(aggregate.session().clone(), aggregate.last_sequence())
                })
                .collect(),
        )
    }

    /// Returns whether a Channel currently subscribes to a Session.
    #[must_use]
    pub fn is_subscribed(&self, channel_id: ChannelId, session_id: SessionId) -> bool {
        self.subscriptions
            .get(&session_id)
            .is_some_and(|channels| channels.contains(&channel_id))
    }

    /// Iterates over a Session's subscribers in stable Channel-ID order.
    pub fn session_subscribers(
        &self,
        session_id: SessionId,
    ) -> impl Iterator<Item = ChannelId> + '_ {
        self.subscriptions
            .get(&session_id)
            .into_iter()
            .flat_map(|channels| channels.iter().copied())
    }

    /// Subscribes a Channel to an existing Session.
    ///
    /// A Channel declaring `SESSION_VIEW` must first accept the current Session
    /// view. If that initial handoff fails, the subscription is not activated.
    pub fn subscribe(
        &mut self,
        channel_id: ChannelId,
        session_id: SessionId,
    ) -> Result<SubscribeOutcome, SubscriptionError> {
        if !self.channels.contains_key(&channel_id) {
            return Err(SubscriptionError::ChannelNotFound { channel_id });
        }
        let aggregate = self
            .sessions
            .get(&session_id)
            .ok_or(SubscriptionError::SessionNotFound { session_id })?;
        let session = aggregate.session().clone();
        let baseline_sequence = aggregate.last_sequence();

        if self.is_subscribed(channel_id, session_id) {
            return Ok(SubscribeOutcome::AlreadySubscribed {
                current_sequence: baseline_sequence,
            });
        }

        let channel = self
            .channels
            .get_mut(&channel_id)
            .ok_or(SubscriptionError::ChannelNotFound { channel_id })?;
        let session_view_delivered = channel
            .descriptor()
            .capabilities()
            .contains(ChannelCapabilities::SESSION_VIEW);
        if session_view_delivered {
            channel.deliver_session(session).map_err(|source| {
                SubscriptionError::InitialSessionHandoff(ChannelDeliveryError::new(
                    channel_id,
                    ChannelDeliveryKind::Session,
                    source,
                ))
            })?;
        }

        let _ = self
            .subscriptions
            .entry(session_id)
            .or_default()
            .insert(channel_id);
        Ok(SubscribeOutcome::Subscribed {
            session_view_delivered,
            baseline_sequence,
        })
    }

    /// Cancels a Channel's subscription to an existing Session.
    pub fn unsubscribe(
        &mut self,
        channel_id: ChannelId,
        session_id: SessionId,
    ) -> Result<UnsubscribeOutcome, SubscriptionError> {
        if !self.channels.contains_key(&channel_id) {
            return Err(SubscriptionError::ChannelNotFound { channel_id });
        }
        if !self.sessions.contains_key(&session_id) {
            return Err(SubscriptionError::SessionNotFound { session_id });
        }

        let removed = self
            .subscriptions
            .get_mut(&session_id)
            .is_some_and(|channels| channels.remove(&channel_id));
        let remove_empty_set = self
            .subscriptions
            .get(&session_id)
            .is_some_and(BTreeSet::is_empty);
        if remove_empty_set {
            let _ = self.subscriptions.remove(&session_id);
        }

        Ok(if removed {
            UnsubscribeOutcome::Unsubscribed
        } else {
            UnsubscribeOutcome::NotSubscribed
        })
    }

    /// Removes every active subscription owned by one registered Channel.
    ///
    /// Returns the number of Session subscriptions that were removed.
    pub fn unsubscribe_channel(
        &mut self,
        channel_id: ChannelId,
    ) -> Result<usize, SubscriptionError> {
        if !self.channels.contains_key(&channel_id) {
            return Err(SubscriptionError::ChannelNotFound { channel_id });
        }

        let mut removed = 0_usize;
        self.subscriptions.retain(|_, channels| {
            if channels.remove(&channel_id) {
                removed += 1;
            }
            !channels.is_empty()
        });
        Ok(removed)
    }

    /// Processes one normalized Event from a registered Provider.
    ///
    /// All routes are computed before reduction. Successful reduction is
    /// retained before Channel fan-out, and every subscribed target is
    /// attempted even when another target rejects its handoff.
    pub fn handle_provider_event(
        &mut self,
        provider_id: ProviderId,
        event: AgentEvent,
    ) -> Result<ProviderEventReport, ProviderEventError> {
        let provider_descriptor = self
            .providers
            .get(&provider_id)
            .ok_or(ProviderEventError::ProviderNotFound { provider_id })?
            .descriptor()
            .clone();
        let session_id = event.session_id();
        let routing_session = if let Some(aggregate) = self.sessions.get(&session_id) {
            aggregate.session().clone()
        } else {
            let AgentEventPayload::SessionStarted(session) = event.payload() else {
                return Err(ProviderEventError::SessionNotStarted { session_id });
            };
            session.clone()
        };

        CapabilityRouter::validate_provider_event(&provider_descriptor, &routing_session, &event)?;

        let subscriber_ids: Vec<_> = self.session_subscribers(session_id).collect();
        let mut routes = Vec::with_capacity(subscriber_ids.len());
        for channel_id in subscriber_ids {
            let channel_descriptor = self
                .channels
                .get(&channel_id)
                .ok_or(ProviderEventError::ChannelNotFound { channel_id })?
                .descriptor();
            let route = CapabilityRouter::channel_event_route(
                &provider_descriptor,
                channel_descriptor,
                &routing_session,
                &event,
            )?;
            let deliver_session = channel_descriptor
                .capabilities()
                .contains(ChannelCapabilities::SESSION_VIEW)
                && event_updates_session_view(&event);
            routes.push((channel_id, route, deliver_session));
        }

        let session_view = if let Some(aggregate) = self.sessions.get_mut(&session_id) {
            let outcome = aggregate.apply(event.clone())?;
            if matches!(outcome, ApplyOutcome::AlreadyApplied) {
                return Ok(ProviderEventReport::new(
                    ProviderEventOutcome::AlreadyApplied,
                    Vec::new(),
                ));
            }
            routes
                .iter()
                .any(|(_, _, deliver_session)| *deliver_session)
                .then(|| aggregate.session().clone())
        } else {
            let aggregate = SessionAggregate::from_initial_event_with_config(
                event.clone(),
                self.session_config,
            )?;
            let session_view = routes
                .iter()
                .any(|(_, _, deliver_session)| *deliver_session)
                .then(|| aggregate.session().clone());
            let _ = self.sessions.insert(session_id, aggregate);
            session_view
        };

        let mut deliveries = Vec::with_capacity(routes.len());
        for (channel_id, route, deliver_session) in routes {
            let Some(channel) = self.channels.get_mut(&channel_id) else {
                return Err(ProviderEventError::ChannelNotFound { channel_id });
            };
            if let Err(source) = channel.deliver_event(event.clone(), route) {
                deliveries.push(ChannelDeliveryResult::Failed(ChannelDeliveryError::new(
                    channel_id,
                    ChannelDeliveryKind::Event,
                    source,
                )));
                continue;
            }

            if deliver_session
                && let Some(session) = session_view.as_ref()
                && let Err(source) = channel.deliver_session(session.clone())
            {
                deliveries.push(ChannelDeliveryResult::Failed(ChannelDeliveryError::new(
                    channel_id,
                    ChannelDeliveryKind::Session,
                    source,
                )));
                continue;
            }
            deliveries.push(ChannelDeliveryResult::Delivered { channel_id });
        }

        let report = ProviderEventReport::new(ProviderEventOutcome::Applied, deliveries);
        if report.has_failures() {
            Err(ProviderEventError::ChannelHandoffs { report })
        } else {
            Ok(report)
        }
    }

    /// Validates and hands one Interaction Response to its Session's Provider.
    pub fn handle_interaction_response(
        &mut self,
        channel_id: ChannelId,
        response: InteractionResponse,
    ) -> Result<(), ChannelActionError> {
        let channel_descriptor = self.action_channel_descriptor(channel_id)?;
        let session_id = response.session_id();
        self.validate_action_subscription(channel_id, session_id)?;
        let aggregate = self
            .sessions
            .get(&session_id)
            .ok_or(ChannelActionError::SessionNotFound { session_id })?;
        let provider_id = aggregate.session().provider_id();
        let provider_descriptor = self
            .providers
            .get(&provider_id)
            .ok_or(ChannelActionError::ProviderNotFound { provider_id })?
            .descriptor()
            .clone();
        let interaction_id = response.request_id();
        let request = aggregate.pending_interaction(interaction_id).ok_or(
            ChannelActionError::InteractionNotPending {
                session_id,
                interaction_id,
            },
        )?;

        CapabilityRouter::validate_interaction_response(
            &provider_descriptor,
            &channel_descriptor,
            aggregate.session(),
            request,
            &response,
        )?;

        self.providers
            .get_mut(&provider_id)
            .ok_or(ChannelActionError::ProviderNotFound { provider_id })?
            .accept_interaction_response(response)
            .map_err(|source| ChannelActionError::ProviderHandoff {
                provider_id,
                handoff: ProviderHandoffKind::InteractionResponse,
                source,
            })
    }

    /// Validates and hands one Agent Command to its Session's Provider.
    pub fn handle_command(
        &mut self,
        channel_id: ChannelId,
        command: AgentCommand,
    ) -> Result<(), ChannelActionError> {
        let channel_descriptor = self.action_channel_descriptor(channel_id)?;
        let session_id = command.session_id();
        self.validate_action_subscription(channel_id, session_id)?;
        let aggregate = self
            .sessions
            .get(&session_id)
            .ok_or(ChannelActionError::SessionNotFound { session_id })?;
        let provider_id = aggregate.session().provider_id();
        let provider_descriptor = self
            .providers
            .get(&provider_id)
            .ok_or(ChannelActionError::ProviderNotFound { provider_id })?
            .descriptor()
            .clone();

        CapabilityRouter::validate_command(
            &provider_descriptor,
            &channel_descriptor,
            aggregate.session(),
            &command,
        )?;

        self.providers
            .get_mut(&provider_id)
            .ok_or(ChannelActionError::ProviderNotFound { provider_id })?
            .accept_command(command)
            .map_err(|source| ChannelActionError::ProviderHandoff {
                provider_id,
                handoff: ProviderHandoffKind::Command,
                source,
            })
    }

    fn action_channel_descriptor(
        &self,
        channel_id: ChannelId,
    ) -> Result<ChannelDescriptor, ChannelActionError> {
        self.channels
            .get(&channel_id)
            .ok_or(ChannelActionError::ChannelNotFound { channel_id })
            .map(|channel| channel.descriptor().clone())
    }

    fn validate_action_subscription(
        &self,
        channel_id: ChannelId,
        session_id: SessionId,
    ) -> Result<(), ChannelActionError> {
        if !self.sessions.contains_key(&session_id) {
            return Err(ChannelActionError::SessionNotFound { session_id });
        }
        if !self.is_subscribed(channel_id, session_id) {
            return Err(ChannelActionError::ChannelNotSubscribed {
                channel_id,
                session_id,
            });
        }
        Ok(())
    }
}

impl Default for Bridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderEventSink for Bridge {
    type Error = ProviderEventError;

    fn publish_event(
        &mut self,
        provider_id: ProviderId,
        event: AgentEvent,
    ) -> Result<(), Self::Error> {
        let _ = self.handle_provider_event(provider_id, event)?;
        Ok(())
    }
}

impl ChannelActionSink for Bridge {
    type Error = ChannelActionError;

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

fn event_updates_session_view(event: &AgentEvent) -> bool {
    matches!(
        event.payload(),
        AgentEventPayload::SessionStarted(_)
            | AgentEventPayload::StateChanged(_)
            | AgentEventPayload::ConnectionChanged(_)
            | AgentEventPayload::SessionEnded(_)
    )
}
