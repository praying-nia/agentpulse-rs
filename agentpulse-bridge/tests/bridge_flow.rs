//! End-to-end contract tests for the minimal Bridge closed loop.

use std::{error::Error, fmt};

use agentpulse_bridge::{
    Bridge, ChannelActionError, ChannelDeliveryKind, ChannelPort, ProviderEventError,
    ProviderEventOutcome, ProviderHandoffKind, ProviderPort,
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
    capabilities: ProviderCapabilities,
) -> Result<ProviderDescriptor, DomainError> {
    Ok(ProviderDescriptor::new(
        provider_id,
        ProviderKind::new("fake")?,
        text("Fake Provider")?,
        capabilities,
    ))
}

fn channel_descriptor(
    channel_id: ChannelId,
    capabilities: ChannelCapabilities,
) -> Result<ChannelDescriptor, DomainError> {
    Ok(ChannelDescriptor::new(
        channel_id,
        ChannelKind::new("test")?,
        text("Test Channel")?,
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
struct HandoffRejected(&'static str);

impl fmt::Display for HandoffRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for HandoffRejected {}

struct FakeProvider {
    descriptor: ProviderDescriptor,
    responses: Vec<InteractionResponse>,
    commands: Vec<AgentCommand>,
    reject_actions: bool,
}

impl ProviderPort for FakeProvider {
    type Error = HandoffRejected;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        if self.reject_actions {
            return Err(HandoffRejected("provider rejected action"));
        }
        self.responses.push(response);
        Ok(())
    }

    fn accept_command(&mut self, command: AgentCommand) -> Result<(), Self::Error> {
        if self.reject_actions {
            return Err(HandoffRejected("provider rejected action"));
        }
        self.commands.push(command);
        Ok(())
    }
}

struct TestChannel {
    descriptor: ChannelDescriptor,
    events: Vec<(AgentEvent, ChannelEventRoute)>,
    sessions: Vec<AgentSession>,
    event_attempts: usize,
    session_attempts: usize,
    reject_event: bool,
    reject_session: bool,
}

impl ChannelPort for TestChannel {
    type Error = HandoffRejected;

    fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), Self::Error> {
        self.event_attempts += 1;
        if self.reject_event {
            return Err(HandoffRejected("channel rejected event"));
        }
        self.events.push((event, route));
        Ok(())
    }

    fn deliver_session(&mut self, session: AgentSession) -> Result<(), Self::Error> {
        self.session_attempts += 1;
        if self.reject_session {
            return Err(HandoffRejected("channel rejected session"));
        }
        self.sessions.push(session);
        Ok(())
    }
}

fn bridge(
    provider_id: ProviderId,
    provider_capabilities: ProviderCapabilities,
    channel_id: ChannelId,
    channel_capabilities: ChannelCapabilities,
) -> Result<Bridge<FakeProvider, TestChannel>, DomainError> {
    bridge_with_rejections(
        provider_id,
        provider_capabilities,
        channel_id,
        channel_capabilities,
        false,
        false,
        false,
    )
}

fn bridge_with_rejections(
    provider_id: ProviderId,
    provider_capabilities: ProviderCapabilities,
    channel_id: ChannelId,
    channel_capabilities: ChannelCapabilities,
    reject_actions: bool,
    reject_event: bool,
    reject_session: bool,
) -> Result<Bridge<FakeProvider, TestChannel>, DomainError> {
    Ok(Bridge::new(
        FakeProvider {
            descriptor: provider_descriptor(provider_id, provider_capabilities)?,
            responses: Vec::new(),
            commands: Vec::new(),
            reject_actions,
        },
        TestChannel {
            descriptor: channel_descriptor(channel_id, channel_capabilities)?,
            events: Vec::new(),
            sessions: Vec::new(),
            event_attempts: 0,
            session_attempts: 0,
            reject_event,
            reject_session,
        },
    ))
}

#[test]
fn provider_events_reduce_and_deliver_current_session_views() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let start = initial_event(session(provider_id, session_id)?)?;
    let state_changed = event(
        session_id,
        2,
        110,
        AgentEventPayload::StateChanged(AgentState::Running),
    )?;
    let message = event(
        session_id,
        3,
        115,
        AgentEventPayload::Message(AgentMessage::new(AgentMessageLevel::Info, text("Working")?)),
    )?;
    let mut bridge = bridge(
        provider_id,
        full_provider_capabilities(),
        channel_id,
        full_channel_capabilities(),
    )?;

    assert_eq!(
        bridge.handle_provider_event(provider_id, start)?,
        ProviderEventOutcome::Applied
    );
    assert_eq!(
        bridge.handle_provider_event(provider_id, state_changed)?,
        ProviderEventOutcome::Applied
    );
    assert_eq!(
        bridge.handle_provider_event(provider_id, message.clone())?,
        ProviderEventOutcome::Applied
    );
    assert_eq!(
        bridge.handle_provider_event(provider_id, message)?,
        ProviderEventOutcome::AlreadyApplied
    );

    let aggregate = bridge
        .session_aggregate(session_id)
        .ok_or(HandoffRejected("missing Session Aggregate"))?;
    assert_eq!(aggregate.session().state(), AgentState::Running);
    assert_eq!(aggregate.session().revision(), Revision::new(2)?);
    assert_eq!(aggregate.last_sequence(), EventSequence::new(3)?);
    assert_eq!(bridge.session_aggregates().len(), 1);
    assert_eq!(bridge.channel().events.len(), 3);
    assert_eq!(bridge.channel().sessions.len(), 2);
    assert_eq!(bridge.channel().event_attempts, 3);
    assert_eq!(bridge.channel().session_attempts, 2);
    assert_eq!(bridge.channel().events[0].1, ChannelEventRoute::ObserveOnly);
    Ok(())
}

#[test]
fn interaction_response_and_command_complete_the_action_loop() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let interaction_id = InteractionId::new();
    let request = text_request(session_id, interaction_id)?;
    let response = text_response(session_id, channel_id, interaction_id)?;
    let command = submit_prompt(session_id, channel_id)?;
    let mut bridge = bridge(
        provider_id,
        full_provider_capabilities(),
        channel_id,
        full_channel_capabilities(),
    )?;

    let _ = bridge.handle_provider_event(
        provider_id,
        initial_event(session(provider_id, session_id)?)?,
    )?;
    let _ = bridge.handle_provider_event(
        provider_id,
        event(
            session_id,
            2,
            120,
            AgentEventPayload::InteractionRequested(request),
        )?,
    )?;

    assert_eq!(
        bridge.channel().events[1].1,
        ChannelEventRoute::Interaction(InteractionRoute::Interactive)
    );
    bridge.handle_interaction_response(channel_id, response.clone())?;
    bridge.handle_command(channel_id, command.clone())?;
    assert_eq!(bridge.provider().responses, vec![response.clone()]);
    assert_eq!(bridge.provider().commands, vec![command]);
    assert!(
        bridge
            .session_aggregate(session_id)
            .is_some_and(|aggregate| aggregate.pending_interaction(interaction_id).is_some())
    );

    let _ = bridge.handle_provider_event(
        provider_id,
        event(
            session_id,
            3,
            130,
            AgentEventPayload::InteractionResponded(response),
        )?,
    )?;
    assert!(
        bridge
            .session_aggregate(session_id)
            .is_some_and(|aggregate| aggregate.pending_interaction(interaction_id).is_none())
    );
    Ok(())
}

#[test]
fn read_only_interactions_and_invalid_sources_never_reach_the_provider() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let interaction_id = InteractionId::new();
    let mut bridge = bridge(
        provider_id,
        ProviderCapabilities::SESSION_STATE | ProviderCapabilities::USER_INPUT_REQUEST,
        channel_id,
        full_channel_capabilities(),
    )?;
    let _ = bridge.handle_provider_event(
        provider_id,
        initial_event(session(provider_id, session_id)?)?,
    )?;
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
        bridge.channel().events[1].1,
        ChannelEventRoute::Interaction(InteractionRoute::ReadOnly)
    );
    let response = text_response(session_id, channel_id, interaction_id)?;
    assert!(matches!(
        bridge.handle_interaction_response(channel_id, response.clone()),
        Err(ChannelActionError::CapabilityRoute(
            CapabilityRouteError::MissingProviderCapabilities { .. }
        ))
    ));
    assert!(matches!(
        bridge.handle_interaction_response(ChannelId::new(), response),
        Err(ChannelActionError::SourceChannelMismatch { .. })
    ));
    assert!(matches!(
        bridge.handle_command(channel_id, submit_prompt(SessionId::new(), channel_id)?),
        Err(ChannelActionError::SessionNotFound { .. })
    ));
    assert!(bridge.provider().responses.is_empty());
    assert!(bridge.provider().commands.is_empty());
    Ok(())
}

#[test]
fn missing_pending_interactions_are_rejected_before_capability_checks() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let mut bridge = bridge(
        provider_id,
        full_provider_capabilities(),
        channel_id,
        full_channel_capabilities(),
    )?;
    let _ = bridge.handle_provider_event(
        provider_id,
        initial_event(session(provider_id, session_id)?)?,
    )?;

    assert!(matches!(
        bridge.handle_interaction_response(
            channel_id,
            text_response(session_id, channel_id, InteractionId::new())?
        ),
        Err(ChannelActionError::InteractionNotPending { .. })
    ));
    assert!(bridge.provider().responses.is_empty());
    Ok(())
}

#[test]
fn invalid_provider_events_do_not_mutate_or_deliver() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let mut bridge = bridge(
        provider_id,
        ProviderCapabilities::SESSION_STATE,
        channel_id,
        full_channel_capabilities(),
    )?;

    assert!(matches!(
        bridge.handle_provider_event(
            ProviderId::new(),
            initial_event(session(provider_id, session_id)?)?
        ),
        Err(ProviderEventError::SourceProviderMismatch { .. })
    ));
    assert!(matches!(
        bridge.handle_provider_event(
            provider_id,
            event(
                SessionId::new(),
                1,
                100,
                AgentEventPayload::StateChanged(AgentState::Running)
            )?
        ),
        Err(ProviderEventError::SessionNotStarted { .. })
    ));
    assert_eq!(bridge.session_aggregates().len(), 0);
    assert_eq!(bridge.channel().event_attempts, 0);

    let _ = bridge.handle_provider_event(
        provider_id,
        initial_event(session(provider_id, session_id)?)?,
    )?;
    assert!(matches!(
        bridge.handle_provider_event(
            provider_id,
            event(
                session_id,
                2,
                110,
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
        bridge.handle_provider_event(
            provider_id,
            event(
                session_id,
                3,
                120,
                AgentEventPayload::Message(AgentMessage::new(
                    AgentMessageLevel::Info,
                    text("Skipped")?
                ))
            )?
        ),
        Err(ProviderEventError::Reduce(ReduceError::SequenceGap { .. }))
    ));

    let aggregate = bridge
        .session_aggregate(session_id)
        .ok_or(HandoffRejected("missing Session Aggregate"))?;
    assert_eq!(aggregate.last_sequence(), EventSequence::FIRST);
    assert_eq!(bridge.channel().event_attempts, 1);
    Ok(())
}

#[test]
fn session_views_are_only_sent_to_channels_that_support_them() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let mut bridge = bridge(
        provider_id,
        full_provider_capabilities(),
        channel_id,
        ChannelCapabilities::TEXT_INPUT | ChannelCapabilities::REMOTE_COMMAND,
    )?;

    let _ = bridge.handle_provider_event(
        provider_id,
        initial_event(session(provider_id, session_id)?)?,
    )?;
    let _ = bridge.handle_provider_event(
        provider_id,
        event(
            session_id,
            2,
            110,
            AgentEventPayload::StateChanged(AgentState::Running),
        )?,
    )?;

    assert_eq!(bridge.channel().events.len(), 2);
    assert_eq!(bridge.channel().session_attempts, 0);
    assert!(bridge.channel().sessions.is_empty());
    Ok(())
}

#[test]
fn channel_handoff_errors_keep_reduced_state_and_report_the_phase() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let event_failure_session_id = SessionId::new();
    let mut event_failure_bridge = bridge_with_rejections(
        provider_id,
        full_provider_capabilities(),
        channel_id,
        full_channel_capabilities(),
        false,
        true,
        false,
    )?;
    assert!(matches!(
        event_failure_bridge.handle_provider_event(
            provider_id,
            initial_event(session(provider_id, event_failure_session_id)?)?
        ),
        Err(ProviderEventError::ChannelHandoff {
            delivery: ChannelDeliveryKind::Event,
            ..
        })
    ));
    assert!(
        event_failure_bridge
            .session_aggregate(event_failure_session_id)
            .is_some()
    );
    assert_eq!(event_failure_bridge.channel().event_attempts, 1);
    assert_eq!(event_failure_bridge.channel().session_attempts, 0);

    let session_id = SessionId::new();
    let start = initial_event(session(provider_id, session_id)?)?;
    let mut bridge = bridge_with_rejections(
        provider_id,
        full_provider_capabilities(),
        channel_id,
        full_channel_capabilities(),
        false,
        false,
        true,
    )?;

    assert!(matches!(
        bridge.handle_provider_event(provider_id, start.clone()),
        Err(ProviderEventError::ChannelHandoff {
            delivery: ChannelDeliveryKind::Session,
            ..
        })
    ));
    assert!(bridge.session_aggregate(session_id).is_some());
    assert_eq!(bridge.channel().event_attempts, 1);
    assert_eq!(bridge.channel().session_attempts, 1);
    assert_eq!(
        bridge.handle_provider_event(provider_id, start)?,
        ProviderEventOutcome::AlreadyApplied
    );
    assert_eq!(bridge.channel().event_attempts, 1);
    assert_eq!(bridge.channel().session_attempts, 1);
    Ok(())
}

#[test]
fn provider_handoff_errors_remain_typed_and_pending_state_is_unchanged() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let interaction_id = InteractionId::new();
    let mut bridge = bridge_with_rejections(
        provider_id,
        full_provider_capabilities(),
        channel_id,
        full_channel_capabilities(),
        true,
        false,
        false,
    )?;
    let _ = bridge.handle_provider_event(
        provider_id,
        initial_event(session(provider_id, session_id)?)?,
    )?;
    let _ = bridge.handle_provider_event(
        provider_id,
        event(
            session_id,
            2,
            120,
            AgentEventPayload::InteractionRequested(text_request(session_id, interaction_id)?),
        )?,
    )?;

    assert!(matches!(
        bridge.handle_interaction_response(
            channel_id,
            text_response(session_id, channel_id, interaction_id)?
        ),
        Err(ChannelActionError::ProviderHandoff {
            handoff: ProviderHandoffKind::InteractionResponse,
            ..
        })
    ));
    assert!(matches!(
        bridge.handle_command(channel_id, submit_prompt(session_id, channel_id)?),
        Err(ChannelActionError::ProviderHandoff {
            handoff: ProviderHandoffKind::Command,
            ..
        })
    ));
    assert!(
        bridge
            .session_aggregate(session_id)
            .is_some_and(|aggregate| aggregate.pending_interaction(interaction_id).is_some())
    );
    assert!(bridge.provider().responses.is_empty());
    assert!(bridge.provider().commands.is_empty());
    Ok(())
}
