//! End-to-end contracts for multi-endpoint Bridge orchestration.

use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use agentpulse_bridge::{
    Bridge, ChannelActionError, ChannelDeliveryKind, ChannelDeliveryResult, ChannelPort,
    EndpointRegistrationError, ProviderEventError, ProviderEventOutcome, ProviderHandoffKind,
    ProviderPort, SubscribeOutcome, SubscriptionError, UnsubscribeOutcome,
};
use agentpulse_core::{
    AgentCommand, AgentCommandPayload, AgentEvent, AgentEventPayload, AgentMessage,
    AgentMessageLevel, AgentSession, AgentState, CapabilityRouteError, ChannelCapabilities,
    ChannelDescriptor, ChannelEventRoute, ChannelId, ChannelKind, CommandId, DomainError, EventId,
    EventSequence, InteractionId, InteractionRequest, InteractionRequestPayload,
    InteractionResponse, InteractionResponsePayload, InteractionRoute, NonEmptyText,
    ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind, ReduceError, Revision,
    SessionId, TextInputRequest, Timestamp, ToolActivity, ToolCallId,
};

type TestResult = Result<(), Box<dyn Error>>;

fn text(value: &str) -> Result<NonEmptyText, DomainError> {
    NonEmptyText::new(value)
}

fn timestamp(value: i128) -> Result<Timestamp, DomainError> {
    Timestamp::from_unix_timestamp_nanos(value)
}

fn provider_descriptor(
    provider_id: ProviderId,
    kind: &str,
    name: &str,
    capabilities: ProviderCapabilities,
) -> Result<ProviderDescriptor, DomainError> {
    Ok(ProviderDescriptor::new(
        provider_id,
        ProviderKind::new(kind)?,
        text(name)?,
        capabilities,
    ))
}

fn channel_descriptor(
    channel_id: ChannelId,
    kind: &str,
    name: &str,
    capabilities: ChannelCapabilities,
) -> Result<ChannelDescriptor, DomainError> {
    Ok(ChannelDescriptor::new(
        channel_id,
        ChannelKind::new(kind)?,
        text(name)?,
        capabilities,
    ))
}

fn session(provider_id: ProviderId, session_id: SessionId) -> Result<AgentSession, DomainError> {
    AgentSession::builder(session_id, provider_id, timestamp(100)?).build()
}

fn event(
    session_id: SessionId,
    sequence: u64,
    occurred_at: i128,
    payload: AgentEventPayload,
) -> Result<AgentEvent, DomainError> {
    AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::new(sequence)?,
        timestamp(occurred_at)?,
        payload,
    )
}

fn initial_event(session: AgentSession) -> Result<AgentEvent, DomainError> {
    event(
        session.id(),
        EventSequence::FIRST.get(),
        session.created_at().unix_timestamp_nanos(),
        AgentEventPayload::SessionStarted(session),
    )
}

fn text_request(
    session_id: SessionId,
    interaction_id: InteractionId,
) -> Result<InteractionRequest, DomainError> {
    Ok(InteractionRequest::new(
        interaction_id,
        session_id,
        timestamp(120)?,
        text("Continue?")?,
        InteractionRequestPayload::Text(TextInputRequest::new(false)),
    ))
}

fn text_response(
    session_id: SessionId,
    channel_id: ChannelId,
    interaction_id: InteractionId,
) -> Result<InteractionResponse, DomainError> {
    Ok(InteractionResponse::new(
        interaction_id,
        session_id,
        channel_id,
        timestamp(130)?,
        InteractionResponsePayload::Text(text("Continue")?),
    ))
}

fn submit_prompt(
    session_id: SessionId,
    channel_id: ChannelId,
) -> Result<AgentCommand, DomainError> {
    Ok(AgentCommand::new(
        CommandId::new(),
        session_id,
        channel_id,
        timestamp(140)?,
        AgentCommandPayload::SubmitPrompt {
            text: text("Run tests")?,
        },
    ))
}

fn message_event(
    session_id: SessionId,
    sequence: u64,
    message: &str,
) -> Result<AgentEvent, DomainError> {
    event(
        session_id,
        sequence,
        100 + i128::from(sequence),
        AgentEventPayload::Message(AgentMessage::new(AgentMessageLevel::Info, text(message)?)),
    )
}

fn full_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities::SESSION_STATE
        | ProviderCapabilities::USER_INPUT_REQUEST
        | ProviderCapabilities::USER_INPUT_RESPONSE
        | ProviderCapabilities::PROMPT_SUBMIT
        | ProviderCapabilities::CANCEL
}

fn full_channel_capabilities() -> ChannelCapabilities {
    ChannelCapabilities::SESSION_VIEW
        | ChannelCapabilities::TEXT_INPUT
        | ChannelCapabilities::REMOTE_COMMAND
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PrimaryRejected(&'static str);

impl fmt::Display for PrimaryRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for PrimaryRejected {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SecondaryRejected(&'static str);

impl fmt::Display for SecondaryRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for SecondaryRejected {}

#[derive(Default)]
struct ProviderState {
    responses: Vec<InteractionResponse>,
    commands: Vec<AgentCommand>,
    response_attempts: usize,
    command_attempts: usize,
    reject_actions: bool,
}

type ProviderStateHandle = Arc<Mutex<ProviderState>>;

fn provider_state(
    handle: &ProviderStateHandle,
) -> Result<MutexGuard<'_, ProviderState>, PrimaryRejected> {
    handle
        .lock()
        .map_err(|_| PrimaryRejected("Provider state lock poisoned"))
}

struct PrimaryProvider {
    descriptor: ProviderDescriptor,
    state: ProviderStateHandle,
}

impl ProviderPort for PrimaryProvider {
    type Error = PrimaryRejected;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        let mut state = provider_state(&self.state)?;
        state.response_attempts += 1;
        if state.reject_actions {
            return Err(PrimaryRejected("primary Provider rejected action"));
        }
        state.responses.push(response);
        Ok(())
    }

    fn accept_command(&mut self, command: AgentCommand) -> Result<(), Self::Error> {
        let mut state = provider_state(&self.state)?;
        state.command_attempts += 1;
        if state.reject_actions {
            return Err(PrimaryRejected("primary Provider rejected action"));
        }
        state.commands.push(command);
        Ok(())
    }
}

struct SecondaryProvider {
    descriptor: ProviderDescriptor,
    state: ProviderStateHandle,
}

impl ProviderPort for SecondaryProvider {
    type Error = SecondaryRejected;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SecondaryRejected("Provider state lock poisoned"))?;
        state.response_attempts += 1;
        if state.reject_actions {
            return Err(SecondaryRejected("secondary Provider rejected action"));
        }
        state.responses.push(response);
        Ok(())
    }

    fn accept_command(&mut self, command: AgentCommand) -> Result<(), Self::Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SecondaryRejected("Provider state lock poisoned"))?;
        state.command_attempts += 1;
        if state.reject_actions {
            return Err(SecondaryRejected("secondary Provider rejected action"));
        }
        state.commands.push(command);
        Ok(())
    }
}

#[derive(Default)]
struct ChannelState {
    events: Vec<(AgentEvent, ChannelEventRoute)>,
    sessions: Vec<AgentSession>,
    event_attempts: usize,
    session_attempts: usize,
    reject_event: bool,
    reject_session: bool,
}

type ChannelStateHandle = Arc<Mutex<ChannelState>>;

fn channel_state(
    handle: &ChannelStateHandle,
) -> Result<MutexGuard<'_, ChannelState>, PrimaryRejected> {
    handle
        .lock()
        .map_err(|_| PrimaryRejected("Channel state lock poisoned"))
}

struct PrimaryChannel {
    descriptor: ChannelDescriptor,
    state: ChannelStateHandle,
}

impl ChannelPort for PrimaryChannel {
    type Error = PrimaryRejected;

    fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), Self::Error> {
        let mut state = channel_state(&self.state)?;
        state.event_attempts += 1;
        if state.reject_event {
            return Err(PrimaryRejected("primary Channel rejected event"));
        }
        state.events.push((event, route));
        Ok(())
    }

    fn deliver_session(&mut self, session: AgentSession) -> Result<(), Self::Error> {
        let mut state = channel_state(&self.state)?;
        state.session_attempts += 1;
        if state.reject_session {
            return Err(PrimaryRejected("primary Channel rejected session"));
        }
        state.sessions.push(session);
        Ok(())
    }
}

struct SecondaryChannel {
    descriptor: ChannelDescriptor,
    state: ChannelStateHandle,
}

impl ChannelPort for SecondaryChannel {
    type Error = SecondaryRejected;

    fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), Self::Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SecondaryRejected("Channel state lock poisoned"))?;
        state.event_attempts += 1;
        if state.reject_event {
            return Err(SecondaryRejected("secondary Channel rejected event"));
        }
        state.events.push((event, route));
        Ok(())
    }

    fn deliver_session(&mut self, session: AgentSession) -> Result<(), Self::Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SecondaryRejected("Channel state lock poisoned"))?;
        state.session_attempts += 1;
        if state.reject_session {
            return Err(SecondaryRejected("secondary Channel rejected session"));
        }
        state.sessions.push(session);
        Ok(())
    }
}

fn primary_provider(
    provider_id: ProviderId,
    name: &str,
    capabilities: ProviderCapabilities,
) -> Result<(PrimaryProvider, ProviderStateHandle), DomainError> {
    let state = Arc::new(Mutex::new(ProviderState::default()));
    Ok((
        PrimaryProvider {
            descriptor: provider_descriptor(provider_id, "primary", name, capabilities)?,
            state: Arc::clone(&state),
        },
        state,
    ))
}

fn secondary_provider(
    provider_id: ProviderId,
    name: &str,
    capabilities: ProviderCapabilities,
) -> Result<(SecondaryProvider, ProviderStateHandle), DomainError> {
    let state = Arc::new(Mutex::new(ProviderState::default()));
    Ok((
        SecondaryProvider {
            descriptor: provider_descriptor(provider_id, "secondary", name, capabilities)?,
            state: Arc::clone(&state),
        },
        state,
    ))
}

fn primary_channel(
    channel_id: ChannelId,
    name: &str,
    capabilities: ChannelCapabilities,
) -> Result<(PrimaryChannel, ChannelStateHandle), DomainError> {
    let state = Arc::new(Mutex::new(ChannelState::default()));
    Ok((
        PrimaryChannel {
            descriptor: channel_descriptor(channel_id, "primary", name, capabilities)?,
            state: Arc::clone(&state),
        },
        state,
    ))
}

fn secondary_channel(
    channel_id: ChannelId,
    name: &str,
    capabilities: ChannelCapabilities,
) -> Result<(SecondaryChannel, ChannelStateHandle), DomainError> {
    let state = Arc::new(Mutex::new(ChannelState::default()));
    Ok((
        SecondaryChannel {
            descriptor: channel_descriptor(channel_id, "secondary", name, capabilities)?,
            state: Arc::clone(&state),
        },
        state,
    ))
}

fn start_session(
    bridge: &mut Bridge,
    provider_id: ProviderId,
    session_id: SessionId,
) -> TestResult {
    let report = bridge.handle_provider_event(
        provider_id,
        initial_event(session(provider_id, session_id)?)?,
    )?;
    assert_eq!(report.outcome(), ProviderEventOutcome::Applied);
    assert!(report.deliveries().is_empty());
    Ok(())
}

fn delivery_for(
    deliveries: &[ChannelDeliveryResult],
    channel_id: ChannelId,
) -> Result<&ChannelDeliveryResult, PrimaryRejected> {
    deliveries
        .iter()
        .find(|delivery| delivery.channel_id() == channel_id)
        .ok_or(PrimaryRejected("missing Channel delivery result"))
}

#[test]
fn heterogeneous_endpoints_register_with_stable_descriptor_lookup() -> TestResult {
    let provider_a = ProviderId::new();
    let provider_b = ProviderId::new();
    let channel_a = ChannelId::new();
    let channel_b = ChannelId::new();
    let (primary, _) =
        primary_provider(provider_a, "Primary Provider", full_provider_capabilities())?;
    let (secondary, _) = secondary_provider(
        provider_b,
        "Secondary Provider",
        full_provider_capabilities(),
    )?;
    let (primary_output, _) =
        primary_channel(channel_a, "Primary Channel", full_channel_capabilities())?;
    let (secondary_output, _) =
        secondary_channel(channel_b, "Secondary Channel", full_channel_capabilities())?;
    let mut bridge = Bridge::new();

    bridge.register_provider(primary)?;
    bridge.register_provider(secondary)?;
    bridge.register_channel(primary_output)?;
    bridge.register_channel(secondary_output)?;

    let (duplicate_provider, _) = secondary_provider(
        provider_a,
        "Duplicate Provider",
        full_provider_capabilities(),
    )?;
    assert!(matches!(
        bridge.register_provider(duplicate_provider),
        Err(EndpointRegistrationError::ProviderAlreadyRegistered { provider_id })
            if provider_id == provider_a
    ));
    let (duplicate_channel, _) =
        secondary_channel(channel_a, "Duplicate Channel", full_channel_capabilities())?;
    assert!(matches!(
        bridge.register_channel(duplicate_channel),
        Err(EndpointRegistrationError::ChannelAlreadyRegistered { channel_id })
            if channel_id == channel_a
    ));

    assert_eq!(
        bridge
            .provider_descriptor(provider_a)
            .ok_or(PrimaryRejected("missing Provider Descriptor"))?
            .display_name()
            .as_str(),
        "Primary Provider"
    );
    assert_eq!(
        bridge
            .channel_descriptor(channel_a)
            .ok_or(PrimaryRejected("missing Channel Descriptor"))?
            .display_name()
            .as_str(),
        "Primary Channel"
    );

    let mut expected_providers = vec![provider_a, provider_b];
    expected_providers.sort();
    assert_eq!(
        bridge
            .provider_descriptors()
            .map(ProviderDescriptor::id)
            .collect::<Vec<_>>(),
        expected_providers
    );
    let mut expected_channels = vec![channel_a, channel_b];
    expected_channels.sort();
    assert_eq!(
        bridge
            .channel_descriptors()
            .map(ChannelDescriptor::id)
            .collect::<Vec<_>>(),
        expected_channels
    );
    Ok(())
}

#[test]
fn subscriptions_sync_current_views_and_are_idempotent() -> TestResult {
    let provider_id = ProviderId::new();
    let session_id = SessionId::new();
    let view_channel_id = ChannelId::new();
    let event_channel_id = ChannelId::new();
    let (provider, _) = primary_provider(provider_id, "Provider", full_provider_capabilities())?;
    let (view_channel, view_state) =
        primary_channel(view_channel_id, "View Channel", full_channel_capabilities())?;
    let (event_channel, event_state) = secondary_channel(
        event_channel_id,
        "Event Channel",
        ChannelCapabilities::TEXT_INPUT | ChannelCapabilities::REMOTE_COMMAND,
    )?;
    let mut bridge = Bridge::new();
    bridge.register_provider(provider)?;
    bridge.register_channel(view_channel)?;
    bridge.register_channel(event_channel)?;
    start_session(&mut bridge, provider_id, session_id)?;

    assert_eq!(
        bridge.subscribe(view_channel_id, session_id)?,
        SubscribeOutcome::Subscribed {
            session_view_delivered: true
        }
    );
    assert_eq!(
        bridge.subscribe(event_channel_id, session_id)?,
        SubscribeOutcome::Subscribed {
            session_view_delivered: false
        }
    );
    assert_eq!(
        bridge.subscribe(view_channel_id, session_id)?,
        SubscribeOutcome::AlreadySubscribed
    );
    assert_eq!(channel_state(&view_state)?.sessions.len(), 1);
    assert_eq!(channel_state(&event_state)?.session_attempts, 0);

    let mut expected = vec![view_channel_id, event_channel_id];
    expected.sort();
    assert_eq!(
        bridge.session_subscribers(session_id).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        bridge.unsubscribe(event_channel_id, session_id)?,
        UnsubscribeOutcome::Unsubscribed
    );
    assert_eq!(
        bridge.unsubscribe(event_channel_id, session_id)?,
        UnsubscribeOutcome::NotSubscribed
    );
    assert!(matches!(
        bridge.subscribe(ChannelId::new(), session_id),
        Err(SubscriptionError::ChannelNotFound { .. })
    ));
    assert!(matches!(
        bridge.subscribe(view_channel_id, SessionId::new()),
        Err(SubscriptionError::SessionNotFound { .. })
    ));
    Ok(())
}

#[test]
fn failed_initial_sync_does_not_activate_a_subscription() -> TestResult {
    let provider_id = ProviderId::new();
    let session_id = SessionId::new();
    let channel_id = ChannelId::new();
    let (provider, _) = primary_provider(provider_id, "Provider", full_provider_capabilities())?;
    let (channel, channel_handle) =
        primary_channel(channel_id, "Channel", full_channel_capabilities())?;
    channel_state(&channel_handle)?.reject_session = true;
    let mut bridge = Bridge::new();
    bridge.register_provider(provider)?;
    bridge.register_channel(channel)?;
    start_session(&mut bridge, provider_id, session_id)?;

    assert!(matches!(
        bridge.subscribe(channel_id, session_id),
        Err(SubscriptionError::InitialSessionHandoff(ref source))
            if source.channel_id() == channel_id
                && source.delivery() == ChannelDeliveryKind::Session
    ));
    assert!(!bridge.is_subscribed(channel_id, session_id));
    assert_eq!(channel_state(&channel_handle)?.session_attempts, 1);

    channel_state(&channel_handle)?.reject_session = false;
    assert_eq!(
        bridge.subscribe(channel_id, session_id)?,
        SubscribeOutcome::Subscribed {
            session_view_delivered: true
        }
    );
    assert!(bridge.is_subscribed(channel_id, session_id));
    Ok(())
}

#[test]
fn provider_events_only_reach_subscribers_for_the_target_session() -> TestResult {
    let provider_a = ProviderId::new();
    let provider_b = ProviderId::new();
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let channel_a = ChannelId::new();
    let channel_b = ChannelId::new();
    let (primary, _) = primary_provider(provider_a, "Primary", full_provider_capabilities())?;
    let (secondary, _) = secondary_provider(provider_b, "Secondary", full_provider_capabilities())?;
    let (primary_output, state_a) =
        primary_channel(channel_a, "Primary Output", full_channel_capabilities())?;
    let (secondary_output, state_b) =
        secondary_channel(channel_b, "Secondary Output", full_channel_capabilities())?;
    let mut bridge = Bridge::new();
    bridge.register_provider(primary)?;
    bridge.register_provider(secondary)?;
    bridge.register_channel(primary_output)?;
    bridge.register_channel(secondary_output)?;
    start_session(&mut bridge, provider_a, session_a)?;
    start_session(&mut bridge, provider_b, session_b)?;
    let _ = bridge.subscribe(channel_a, session_a)?;
    let _ = bridge.subscribe(channel_b, session_b)?;

    let report_a = bridge.handle_provider_event(provider_a, message_event(session_a, 2, "A")?)?;
    assert_eq!(report_a.deliveries().len(), 1);
    assert_eq!(report_a.deliveries()[0].channel_id(), channel_a);
    assert_eq!(channel_state(&state_a)?.events.len(), 1);
    assert!(channel_state(&state_b)?.events.is_empty());

    let report_b = bridge.handle_provider_event(provider_b, message_event(session_b, 2, "B")?)?;
    assert_eq!(report_b.deliveries().len(), 1);
    assert_eq!(report_b.deliveries()[0].channel_id(), channel_b);
    assert_eq!(channel_state(&state_b)?.events.len(), 1);

    assert_eq!(
        bridge.unsubscribe(channel_a, session_a)?,
        UnsubscribeOutcome::Unsubscribed
    );
    let report =
        bridge.handle_provider_event(provider_a, message_event(session_a, 3, "A again")?)?;
    assert!(report.deliveries().is_empty());
    assert_eq!(channel_state(&state_a)?.event_attempts, 1);
    assert_eq!(
        bridge
            .session_aggregate(session_a)
            .ok_or(PrimaryRejected("missing Aggregate"))?
            .last_sequence(),
        EventSequence::new(3)?
    );
    Ok(())
}

#[test]
fn fanout_computes_interaction_routes_per_channel() -> TestResult {
    let provider_id = ProviderId::new();
    let session_id = SessionId::new();
    let interactive_channel_id = ChannelId::new();
    let read_only_channel_id = ChannelId::new();
    let interaction_id = InteractionId::new();
    let (provider, _) = primary_provider(provider_id, "Provider", full_provider_capabilities())?;
    let (interactive_channel, interactive_state) = primary_channel(
        interactive_channel_id,
        "Interactive",
        full_channel_capabilities(),
    )?;
    let (read_only_channel, read_only_state) = secondary_channel(
        read_only_channel_id,
        "Read Only",
        ChannelCapabilities::SESSION_VIEW,
    )?;
    let mut bridge = Bridge::new();
    bridge.register_provider(provider)?;
    bridge.register_channel(interactive_channel)?;
    bridge.register_channel(read_only_channel)?;
    start_session(&mut bridge, provider_id, session_id)?;
    let _ = bridge.subscribe(interactive_channel_id, session_id)?;
    let _ = bridge.subscribe(read_only_channel_id, session_id)?;

    let report = bridge.handle_provider_event(
        provider_id,
        event(
            session_id,
            2,
            120,
            AgentEventPayload::InteractionRequested(text_request(session_id, interaction_id)?),
        )?,
    )?;
    let mut expected = vec![interactive_channel_id, read_only_channel_id];
    expected.sort();
    assert_eq!(
        report
            .deliveries()
            .iter()
            .map(ChannelDeliveryResult::channel_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        channel_state(&interactive_state)?.events[0].1,
        ChannelEventRoute::Interaction(InteractionRoute::Interactive)
    );
    assert_eq!(
        channel_state(&read_only_state)?.events[0].1,
        ChannelEventRoute::Interaction(InteractionRoute::ReadOnly)
    );
    Ok(())
}

#[test]
fn fanout_failures_are_isolated_reported_and_not_retried() -> TestResult {
    let provider_id = ProviderId::new();
    let session_id = SessionId::new();
    let successful_id = ChannelId::new();
    let event_failure_id = ChannelId::new();
    let session_failure_id = ChannelId::new();
    let (provider, _) = primary_provider(provider_id, "Provider", full_provider_capabilities())?;
    let (successful, successful_state) =
        primary_channel(successful_id, "Successful", full_channel_capabilities())?;
    let (event_failure, event_failure_state) = secondary_channel(
        event_failure_id,
        "Event Failure",
        full_channel_capabilities(),
    )?;
    let (session_failure, session_failure_state) = secondary_channel(
        session_failure_id,
        "Session Failure",
        full_channel_capabilities(),
    )?;
    let mut bridge = Bridge::new();
    bridge.register_provider(provider)?;
    bridge.register_channel(successful)?;
    bridge.register_channel(event_failure)?;
    bridge.register_channel(session_failure)?;
    start_session(&mut bridge, provider_id, session_id)?;
    let _ = bridge.subscribe(successful_id, session_id)?;
    let _ = bridge.subscribe(event_failure_id, session_id)?;
    let _ = bridge.subscribe(session_failure_id, session_id)?;
    channel_state(&event_failure_state)?.reject_event = true;
    channel_state(&session_failure_state)?.reject_session = true;

    let state_changed = event(
        session_id,
        2,
        110,
        AgentEventPayload::StateChanged(AgentState::Running),
    )?;
    let error = bridge
        .handle_provider_event(provider_id, state_changed.clone())
        .err()
        .ok_or(PrimaryRejected("expected partial fan-out failure"))?;
    let report = error
        .report()
        .ok_or(PrimaryRejected("missing fan-out report"))?;
    assert_eq!(report.outcome(), ProviderEventOutcome::Applied);
    assert_eq!(report.deliveries().len(), 3);
    assert!(delivery_for(report.deliveries(), successful_id)?.is_delivered());
    let event_failure = delivery_for(report.deliveries(), event_failure_id)?
        .error()
        .ok_or(PrimaryRejected("missing Event failure"))?;
    assert_eq!(event_failure.delivery(), ChannelDeliveryKind::Event);
    assert!(
        event_failure
            .source()
            .is_some_and(|source| source.downcast_ref::<SecondaryRejected>().is_some())
    );
    assert_eq!(
        delivery_for(report.deliveries(), session_failure_id)?
            .error()
            .ok_or(PrimaryRejected("missing Session failure"))?
            .delivery(),
        ChannelDeliveryKind::Session
    );
    assert_eq!(channel_state(&successful_state)?.event_attempts, 1);
    assert_eq!(channel_state(&event_failure_state)?.event_attempts, 1);
    assert_eq!(channel_state(&event_failure_state)?.session_attempts, 1);
    assert_eq!(channel_state(&session_failure_state)?.event_attempts, 1);
    assert_eq!(channel_state(&session_failure_state)?.session_attempts, 2);
    assert_eq!(
        bridge
            .session_aggregate(session_id)
            .ok_or(PrimaryRejected("missing Aggregate"))?
            .session()
            .state(),
        AgentState::Running
    );

    let retry = bridge.handle_provider_event(provider_id, state_changed)?;
    assert_eq!(retry.outcome(), ProviderEventOutcome::AlreadyApplied);
    assert!(retry.deliveries().is_empty());
    assert_eq!(channel_state(&successful_state)?.event_attempts, 1);
    assert_eq!(channel_state(&event_failure_state)?.event_attempts, 1);
    assert_eq!(channel_state(&session_failure_state)?.event_attempts, 1);
    Ok(())
}

#[test]
fn actions_follow_the_session_owner_and_require_an_active_subscription() -> TestResult {
    let provider_a = ProviderId::new();
    let provider_b = ProviderId::new();
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let channel_id = ChannelId::new();
    let interaction_id = InteractionId::new();
    let (primary, state_a) = primary_provider(provider_a, "Primary", full_provider_capabilities())?;
    let (secondary, state_b) =
        secondary_provider(provider_b, "Secondary", full_provider_capabilities())?;
    let (channel, _) = primary_channel(channel_id, "Channel", full_channel_capabilities())?;
    let mut bridge = Bridge::new();
    bridge.register_provider(primary)?;
    bridge.register_provider(secondary)?;
    bridge.register_channel(channel)?;
    start_session(&mut bridge, provider_a, session_a)?;
    start_session(&mut bridge, provider_b, session_b)?;
    let _ = bridge.subscribe(channel_id, session_a)?;
    let _ = bridge.subscribe(channel_id, session_b)?;
    let _ = bridge.handle_provider_event(
        provider_b,
        event(
            session_b,
            2,
            120,
            AgentEventPayload::InteractionRequested(text_request(session_b, interaction_id)?),
        )?,
    )?;

    let response = text_response(session_b, channel_id, interaction_id)?;
    let command = submit_prompt(session_b, channel_id)?;
    bridge.handle_interaction_response(channel_id, response.clone())?;
    assert!(
        bridge
            .session_aggregate(session_b)
            .is_some_and(|aggregate| aggregate.pending_interaction(interaction_id).is_some())
    );
    bridge.handle_command(channel_id, command.clone())?;
    assert!(provider_state(&state_a)?.responses.is_empty());
    assert!(provider_state(&state_a)?.commands.is_empty());
    assert_eq!(provider_state(&state_b)?.responses, vec![response.clone()]);
    assert_eq!(provider_state(&state_b)?.commands, vec![command]);

    let _ = bridge.handle_provider_event(
        provider_b,
        event(
            session_b,
            3,
            130,
            AgentEventPayload::InteractionResponded(response),
        )?,
    )?;
    assert!(
        bridge
            .session_aggregate(session_b)
            .is_some_and(|aggregate| aggregate.pending_interaction(interaction_id).is_none())
    );

    let _ = bridge.unsubscribe(channel_id, session_b)?;
    assert!(matches!(
        bridge.handle_command(channel_id, submit_prompt(session_b, channel_id)?),
        Err(ChannelActionError::ChannelNotSubscribed {
            channel_id: actual_channel,
            session_id: actual_session
        }) if actual_channel == channel_id && actual_session == session_b
    ));
    assert_eq!(provider_state(&state_b)?.command_attempts, 1);
    Ok(())
}

#[test]
fn invalid_provider_events_do_not_mutate_or_cross_provider_boundaries() -> TestResult {
    let provider_a = ProviderId::new();
    let provider_b = ProviderId::new();
    let session_b = SessionId::new();
    let channel_id = ChannelId::new();
    let (primary, _) =
        primary_provider(provider_a, "Primary", ProviderCapabilities::SESSION_STATE)?;
    let (secondary, _) =
        secondary_provider(provider_b, "Secondary", ProviderCapabilities::SESSION_STATE)?;
    let (channel, channel_handle) =
        primary_channel(channel_id, "Channel", full_channel_capabilities())?;
    let mut bridge = Bridge::new();
    bridge.register_provider(primary)?;
    bridge.register_provider(secondary)?;
    bridge.register_channel(channel)?;

    assert!(matches!(
        bridge.handle_provider_event(
            ProviderId::new(),
            initial_event(session(provider_a, SessionId::new())?)?
        ),
        Err(ProviderEventError::ProviderNotFound { .. })
    ));
    assert!(matches!(
        bridge.handle_provider_event(
            provider_a,
            event(
                SessionId::new(),
                1,
                100,
                AgentEventPayload::StateChanged(AgentState::Running)
            )?
        ),
        Err(ProviderEventError::SessionNotStarted { .. })
    ));
    start_session(&mut bridge, provider_b, session_b)?;
    let _ = bridge.subscribe(channel_id, session_b)?;

    assert!(matches!(
        bridge.handle_provider_event(provider_a, message_event(session_b, 2, "crossed")?),
        Err(ProviderEventError::CapabilityRoute(
            CapabilityRouteError::ProviderMismatch { .. }
        ))
    ));
    assert_eq!(
        bridge
            .session_aggregate(session_b)
            .ok_or(PrimaryRejected("missing Aggregate"))?
            .last_sequence(),
        EventSequence::FIRST
    );
    assert!(channel_state(&channel_handle)?.events.is_empty());

    assert!(matches!(
        bridge.handle_provider_event(
            provider_b,
            event(
                session_b,
                2,
                120,
                AgentEventPayload::ToolActivity(ToolActivity::Started {
                    call_id: ToolCallId::new(),
                    name: text("shell")?,
                    summary: None,
                })
            )?
        ),
        Err(ProviderEventError::CapabilityRoute(
            CapabilityRouteError::MissingProviderCapabilities { .. }
        ))
    ));
    assert!(matches!(
        bridge.handle_provider_event(provider_b, message_event(session_b, 3, "gap")?),
        Err(ProviderEventError::Reduce(ReduceError::SequenceGap { .. }))
    ));
    assert_eq!(
        bridge
            .session_aggregate(session_b)
            .ok_or(PrimaryRejected("missing Aggregate"))?
            .last_sequence(),
        EventSequence::FIRST
    );
    assert!(channel_state(&channel_handle)?.events.is_empty());
    Ok(())
}

#[test]
fn action_validation_prevents_invalid_provider_handoffs() -> TestResult {
    let provider_id = ProviderId::new();
    let session_id = SessionId::new();
    let channel_id = ChannelId::new();
    let interaction_id = InteractionId::new();
    let (provider, provider_handle) = primary_provider(
        provider_id,
        "Provider",
        ProviderCapabilities::SESSION_STATE | ProviderCapabilities::USER_INPUT_REQUEST,
    )?;
    let (channel, channel_handle) =
        primary_channel(channel_id, "Channel", full_channel_capabilities())?;
    let mut bridge = Bridge::new();
    bridge.register_provider(provider)?;
    bridge.register_channel(channel)?;
    start_session(&mut bridge, provider_id, session_id)?;
    let _ = bridge.subscribe(channel_id, session_id)?;
    let _ = bridge.handle_provider_event(
        provider_id,
        event(
            session_id,
            2,
            120,
            AgentEventPayload::InteractionRequested(text_request(session_id, interaction_id)?),
        )?,
    )?;
    assert_eq!(
        channel_state(&channel_handle)?.events[0].1,
        ChannelEventRoute::Interaction(InteractionRoute::ReadOnly)
    );

    assert!(matches!(
        bridge.handle_interaction_response(
            channel_id,
            text_response(session_id, channel_id, interaction_id)?
        ),
        Err(ChannelActionError::CapabilityRoute(
            CapabilityRouteError::MissingProviderCapabilities { .. }
        ))
    ));
    assert!(matches!(
        bridge.handle_interaction_response(
            channel_id,
            text_response(session_id, channel_id, InteractionId::new())?
        ),
        Err(ChannelActionError::InteractionNotPending { .. })
    ));
    assert!(matches!(
        bridge.handle_interaction_response(
            channel_id,
            text_response(session_id, ChannelId::new(), interaction_id)?
        ),
        Err(ChannelActionError::CapabilityRoute(
            CapabilityRouteError::ChannelMismatch { .. }
        ))
    ));
    assert!(matches!(
        bridge.handle_command(ChannelId::new(), submit_prompt(session_id, channel_id)?),
        Err(ChannelActionError::ChannelNotFound { .. })
    ));
    assert!(provider_state(&provider_handle)?.responses.is_empty());
    assert!(provider_state(&provider_handle)?.commands.is_empty());
    Ok(())
}

#[test]
fn provider_handoff_errors_preserve_the_adapter_source_and_state() -> TestResult {
    let provider_id = ProviderId::new();
    let session_id = SessionId::new();
    let channel_id = ChannelId::new();
    let interaction_id = InteractionId::new();
    let (provider, provider_handle) =
        primary_provider(provider_id, "Provider", full_provider_capabilities())?;
    provider_state(&provider_handle)?.reject_actions = true;
    let (channel, _) = primary_channel(channel_id, "Channel", full_channel_capabilities())?;
    let mut bridge = Bridge::new();
    bridge.register_provider(provider)?;
    bridge.register_channel(channel)?;
    start_session(&mut bridge, provider_id, session_id)?;
    let _ = bridge.subscribe(channel_id, session_id)?;
    let _ = bridge.handle_provider_event(
        provider_id,
        event(
            session_id,
            2,
            120,
            AgentEventPayload::InteractionRequested(text_request(session_id, interaction_id)?),
        )?,
    )?;

    match bridge.handle_interaction_response(
        channel_id,
        text_response(session_id, channel_id, interaction_id)?,
    ) {
        Err(ChannelActionError::ProviderHandoff {
            handoff: ProviderHandoffKind::InteractionResponse,
            source,
            ..
        }) => assert!(source.downcast_ref::<PrimaryRejected>().is_some()),
        result => return Err(format!("unexpected response result: {result:?}").into()),
    }
    match bridge.handle_command(channel_id, submit_prompt(session_id, channel_id)?) {
        Err(ChannelActionError::ProviderHandoff {
            handoff: ProviderHandoffKind::Command,
            source,
            ..
        }) => assert!(source.downcast_ref::<PrimaryRejected>().is_some()),
        result => return Err(format!("unexpected command result: {result:?}").into()),
    }
    assert!(
        bridge
            .session_aggregate(session_id)
            .is_some_and(|aggregate| aggregate.pending_interaction(interaction_id).is_some())
    );
    assert_eq!(provider_state(&provider_handle)?.response_attempts, 1);
    assert_eq!(provider_state(&provider_handle)?.command_attempts, 1);
    assert_eq!(
        bridge
            .session_aggregate(session_id)
            .ok_or(PrimaryRejected("missing Aggregate"))?
            .session()
            .revision(),
        Revision::FIRST
    );
    Ok(())
}
