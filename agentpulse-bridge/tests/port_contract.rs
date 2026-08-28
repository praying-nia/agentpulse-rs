//! Contract tests for independent Provider and Channel ports.

use std::{convert::Infallible, error::Error, fmt};

use agentpulse_bridge::{ChannelActionSink, ChannelPort, ProviderEventSink, ProviderPort};
use agentpulse_core::{
    AgentCommand, AgentCommandPayload, AgentEvent, AgentEventPayload, AgentSession,
    ChannelCapabilities, ChannelDescriptor, ChannelEventRoute, ChannelId, ChannelKind, CommandId,
    DomainError, EventId, EventSequence, InteractionId, InteractionResponse,
    InteractionResponsePayload, NonEmptyText, ProviderCapabilities, ProviderDescriptor, ProviderId,
    ProviderKind, SessionId, Timestamp,
};

type TestResult = Result<(), Box<dyn Error>>;

fn text(value: &str) -> Result<NonEmptyText, DomainError> {
    NonEmptyText::new(value)
}

fn timestamp(value: i128) -> Result<Timestamp, DomainError> {
    Timestamp::from_unix_timestamp_nanos(value)
}

fn provider_descriptor(id: ProviderId) -> Result<ProviderDescriptor, DomainError> {
    Ok(ProviderDescriptor::new(
        id,
        ProviderKind::new("fake")?,
        text("Fake Provider")?,
        ProviderCapabilities::SESSION_STATE
            | ProviderCapabilities::USER_INPUT_RESPONSE
            | ProviderCapabilities::CANCEL,
    ))
}

fn channel_descriptor(id: ChannelId) -> Result<ChannelDescriptor, DomainError> {
    Ok(ChannelDescriptor::new(
        id,
        ChannelKind::new("test")?,
        text("Test Channel")?,
        ChannelCapabilities::SESSION_VIEW
            | ChannelCapabilities::TEXT_INPUT
            | ChannelCapabilities::REMOTE_COMMAND,
    ))
}

fn session(provider_id: ProviderId) -> Result<AgentSession, DomainError> {
    AgentSession::builder(SessionId::new(), provider_id, timestamp(100)?).build()
}

fn response(
    session_id: SessionId,
    channel_id: ChannelId,
) -> Result<InteractionResponse, DomainError> {
    Ok(InteractionResponse::new(
        InteractionId::new(),
        session_id,
        channel_id,
        timestamp(110)?,
        InteractionResponsePayload::Text(text("Continue")?),
    ))
}

fn command(session_id: SessionId, channel_id: ChannelId) -> Result<AgentCommand, DomainError> {
    Ok(AgentCommand::new(
        CommandId::new(),
        session_id,
        channel_id,
        timestamp(110)?,
        AgentCommandPayload::CancelSession { reason: None },
    ))
}

fn initial_event(session: AgentSession) -> Result<AgentEvent, DomainError> {
    AgentEvent::new(
        EventId::new(),
        session.id(),
        EventSequence::FIRST,
        session.created_at(),
        AgentEventPayload::SessionStarted(session),
    )
}

struct FakeProvider {
    descriptor: ProviderDescriptor,
    responses: Vec<InteractionResponse>,
    commands: Vec<AgentCommand>,
}

impl ProviderPort for FakeProvider {
    type Error = Infallible;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        self.responses.push(response);
        Ok(())
    }

    fn accept_command(&mut self, command: AgentCommand) -> Result<(), Self::Error> {
        self.commands.push(command);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingProviderEventSink {
    events: Vec<(ProviderId, AgentEvent)>,
}

impl ProviderEventSink for RecordingProviderEventSink {
    type Error = Infallible;

    fn publish_event(
        &mut self,
        provider_id: ProviderId,
        event: AgentEvent,
    ) -> Result<(), Self::Error> {
        self.events.push((provider_id, event));
        Ok(())
    }
}

#[test]
fn fake_provider_publishes_events_and_accepts_validated_actions() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session = session(provider_id)?;
    let event = initial_event(session.clone())?;
    let response = response(session.id(), channel_id)?;
    let command = command(session.id(), channel_id)?;
    let mut provider = FakeProvider {
        descriptor: provider_descriptor(provider_id)?,
        responses: Vec::new(),
        commands: Vec::new(),
    };
    let mut event_sink = RecordingProviderEventSink::default();

    event_sink.publish_event(provider_id, event.clone())?;
    provider.accept_interaction_response(response.clone())?;
    provider.accept_command(command.clone())?;

    assert_eq!(provider.descriptor().id(), provider_id);
    assert_eq!(event_sink.events, vec![(provider_id, event)]);
    assert_eq!(provider.responses, vec![response]);
    assert_eq!(provider.commands, vec![command]);
    Ok(())
}

struct TestChannel {
    descriptor: ChannelDescriptor,
    events: Vec<(AgentEvent, ChannelEventRoute)>,
    sessions: Vec<AgentSession>,
}

impl ChannelPort for TestChannel {
    type Error = Infallible;

    fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), Self::Error> {
        self.events.push((event, route));
        Ok(())
    }

    fn deliver_session(&mut self, session: AgentSession) -> Result<(), Self::Error> {
        self.sessions.push(session);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingChannelActionSink {
    responses: Vec<(ChannelId, InteractionResponse)>,
    commands: Vec<(ChannelId, AgentCommand)>,
}

impl ChannelActionSink for RecordingChannelActionSink {
    type Error = Infallible;

    fn submit_interaction_response(
        &mut self,
        channel_id: ChannelId,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        self.responses.push((channel_id, response));
        Ok(())
    }

    fn submit_command(
        &mut self,
        channel_id: ChannelId,
        command: AgentCommand,
    ) -> Result<(), Self::Error> {
        self.commands.push((channel_id, command));
        Ok(())
    }
}

#[test]
fn test_channel_consumes_routed_views_and_submits_actions() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session = session(provider_id)?;
    let event = initial_event(session.clone())?;
    let response = response(session.id(), channel_id)?;
    let command = command(session.id(), channel_id)?;
    let mut channel = TestChannel {
        descriptor: channel_descriptor(channel_id)?,
        events: Vec::new(),
        sessions: Vec::new(),
    };
    let mut action_sink = RecordingChannelActionSink::default();

    channel.deliver_event(event.clone(), ChannelEventRoute::ObserveOnly)?;
    channel.deliver_session(session.clone())?;
    action_sink.submit_interaction_response(channel_id, response.clone())?;
    action_sink.submit_command(channel_id, command.clone())?;

    assert_eq!(channel.descriptor().id(), channel_id);
    assert_eq!(
        channel.events,
        vec![(event, ChannelEventRoute::ObserveOnly)]
    );
    assert_eq!(channel.sessions, vec![session]);
    assert_eq!(action_sink.responses, vec![(channel_id, response)]);
    assert_eq!(action_sink.commands, vec![(channel_id, command)]);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HandoffRejected;

impl fmt::Display for HandoffRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("handoff rejected")
    }
}

impl Error for HandoffRejected {}

struct RejectingProvider {
    descriptor: ProviderDescriptor,
}

impl ProviderPort for RejectingProvider {
    type Error = HandoffRejected;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        _response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        Err(HandoffRejected)
    }

    fn accept_command(&mut self, _command: AgentCommand) -> Result<(), Self::Error> {
        Err(HandoffRejected)
    }
}

#[test]
fn port_handoff_errors_remain_adapter_defined_and_visible() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session = session(provider_id)?;
    let mut provider = RejectingProvider {
        descriptor: provider_descriptor(provider_id)?,
    };

    assert_eq!(
        provider.accept_interaction_response(response(session.id(), channel_id)?),
        Err(HandoffRejected)
    );
    assert_eq!(
        provider.accept_command(command(session.id(), channel_id)?),
        Err(HandoffRejected)
    );
    Ok(())
}
