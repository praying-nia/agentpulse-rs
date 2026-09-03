//! Contract tests for centralized Provider-to-Channel capability routing.

use std::error::Error;

use agentpulse_core::{
    AgentCommand, AgentCommandPayload, AgentEvent, AgentEventPayload, AgentMessage,
    AgentMessageLevel, AgentSession, ApprovalCommandKind, ApprovalDisposition, ApprovalOption,
    ApprovalOptionId, ApprovalRequest, ApprovalSelection, ApprovalSubject, CapabilityRouteError,
    CapabilityRouter, ChannelCapabilities, ChannelDescriptor, ChannelEventRoute, ChannelId,
    ChannelKind, ChoiceOption, ChoiceOptionId, ChoiceRequest, ChoiceSelection, CommandId,
    DomainError, EventId, EventSequence, InteractionId, InteractionRequest,
    InteractionRequestPayload, InteractionResponse, InteractionResponsePayload, InteractionRoute,
    NonEmptyText, ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind, SessionId,
    TextInputRequest, Timestamp, ToolActivity, ToolCallId,
};

type TestResult = Result<(), Box<dyn Error>>;

fn text(value: &str) -> Result<NonEmptyText, DomainError> {
    NonEmptyText::new(value)
}

fn timestamp(value: i128) -> Result<Timestamp, DomainError> {
    Timestamp::from_unix_timestamp_nanos(value)
}

fn provider(
    id: ProviderId,
    capabilities: ProviderCapabilities,
) -> Result<ProviderDescriptor, DomainError> {
    Ok(ProviderDescriptor::new(
        id,
        ProviderKind::new("fake")?,
        text("Fake Provider")?,
        capabilities,
    ))
}

fn channel(
    id: ChannelId,
    capabilities: ChannelCapabilities,
) -> Result<ChannelDescriptor, DomainError> {
    Ok(ChannelDescriptor::new(
        id,
        ChannelKind::new("test")?,
        text("Test Channel")?,
        capabilities,
    ))
}

fn session(provider_id: ProviderId) -> Result<AgentSession, DomainError> {
    AgentSession::builder(SessionId::new(), provider_id, timestamp(100)?).build()
}

fn event(session_id: SessionId, payload: AgentEventPayload) -> Result<AgentEvent, DomainError> {
    AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::FIRST,
        timestamp(100)?,
        payload,
    )
}

fn approval(option_id: ApprovalOptionId) -> Result<ApprovalRequest, DomainError> {
    ApprovalRequest::actionable(
        ApprovalSubject::Command {
            kind: ApprovalCommandKind::Command,
            command: Some(text("cargo test")?),
            cwd: None,
            reason: None,
            network: None,
        },
        vec![ApprovalOption::new(
            option_id,
            ApprovalDisposition::Approve,
            text("Approve once")?,
        )],
    )
}

#[test]
fn provider_events_use_one_central_capability_mapping() -> TestResult {
    let provider_id = ProviderId::new();
    let session = session(provider_id)?;
    let descriptor = provider(
        provider_id,
        ProviderCapabilities::SESSION_STATE | ProviderCapabilities::TOOL_EVENTS,
    )?;

    let state_event = event(
        session.id(),
        AgentEventPayload::StateChanged(agentpulse_core::AgentState::Running),
    )?;
    CapabilityRouter::validate_provider_event(&descriptor, &session, &state_event)?;

    let tool_event = event(
        session.id(),
        AgentEventPayload::ToolActivity(ToolActivity::Started {
            call_id: ToolCallId::new(),
            name: text("shell")?,
            summary: None,
        }),
    )?;
    CapabilityRouter::validate_provider_event(&descriptor, &session, &tool_event)?;

    let message_event = event(
        session.id(),
        AgentEventPayload::Message(AgentMessage::new(
            AgentMessageLevel::Info,
            text("Still working")?,
        )),
    )?;
    CapabilityRouter::validate_provider_event(
        &provider(provider_id, ProviderCapabilities::NONE)?,
        &session,
        &message_event,
    )?;

    let missing_plan = event(
        session.id(),
        AgentEventPayload::PlanUpdated(agentpulse_core::PlanSnapshot::new(
            agentpulse_core::Revision::FIRST,
            Vec::new(),
        )?),
    )?;
    assert!(matches!(
        CapabilityRouter::validate_provider_event(&descriptor, &session, &missing_plan),
        Err(CapabilityRouteError::MissingProviderCapabilities {
            required: ProviderCapabilities::PLAN,
            ..
        })
    ));

    let wrong_provider = provider(ProviderId::new(), ProviderCapabilities::SESSION_STATE)?;
    assert!(matches!(
        CapabilityRouter::validate_provider_event(&wrong_provider, &session, &state_event),
        Err(CapabilityRouteError::ProviderMismatch { .. })
    ));

    let wrong_session_event = event(
        SessionId::new(),
        AgentEventPayload::StateChanged(agentpulse_core::AgentState::Running),
    )?;
    assert!(matches!(
        CapabilityRouter::validate_provider_event(&descriptor, &session, &wrong_session_event),
        Err(CapabilityRouteError::SessionMismatch { .. })
    ));
    Ok(())
}

#[test]
fn interaction_routes_degrade_to_read_only_without_a_complete_response_path() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session = session(provider_id)?;
    let approval_option_id = ApprovalOptionId::new();
    let request = InteractionRequest::new(
        InteractionId::new(),
        session.id(),
        timestamp(110)?,
        text("Approve?")?,
        InteractionRequestPayload::Approval(approval(approval_option_id)?),
    );
    let request_event = event(
        session.id(),
        AgentEventPayload::InteractionRequested(request.clone()),
    )?;
    let complete_provider = provider(
        provider_id,
        ProviderCapabilities::APPROVAL_REQUEST | ProviderCapabilities::APPROVAL_RESPONSE,
    )?;
    let interactive_channel = channel(channel_id, ChannelCapabilities::APPROVAL)?;

    assert_eq!(
        CapabilityRouter::channel_event_route(
            &complete_provider,
            &interactive_channel,
            &session,
            &request_event,
        )?,
        ChannelEventRoute::Interaction(InteractionRoute::Interactive)
    );

    let read_only_provider = provider(provider_id, ProviderCapabilities::APPROVAL_REQUEST)?;
    assert_eq!(
        CapabilityRouter::interaction_route(
            &read_only_provider,
            &interactive_channel,
            &session,
            &request,
        )?,
        InteractionRoute::ReadOnly
    );

    let response = InteractionResponse::new(
        request.id(),
        session.id(),
        channel_id,
        timestamp(120)?,
        InteractionResponsePayload::Approval(ApprovalSelection::new(approval_option_id)),
    );
    assert!(matches!(
        CapabilityRouter::validate_interaction_response(
            &read_only_provider,
            &interactive_channel,
            &session,
            &request,
            &response,
        ),
        Err(CapabilityRouteError::MissingProviderCapabilities {
            required: ProviderCapabilities::APPROVAL_RESPONSE,
            ..
        })
    ));

    let read_only_channel = channel(channel_id, ChannelCapabilities::NOTIFICATION)?;
    assert_eq!(
        CapabilityRouter::interaction_route(
            &complete_provider,
            &read_only_channel,
            &session,
            &request,
        )?,
        InteractionRoute::ReadOnly
    );
    assert!(matches!(
        CapabilityRouter::validate_interaction_response(
            &complete_provider,
            &read_only_channel,
            &session,
            &request,
            &response,
        ),
        Err(CapabilityRouteError::MissingChannelCapabilities {
            required: ChannelCapabilities::APPROVAL,
            ..
        })
    ));

    let undeclared_request = provider(provider_id, ProviderCapabilities::APPROVAL_RESPONSE)?;
    assert!(matches!(
        CapabilityRouter::interaction_route(
            &undeclared_request,
            &interactive_channel,
            &session,
            &request,
        ),
        Err(CapabilityRouteError::MissingProviderCapabilities {
            required: ProviderCapabilities::APPROVAL_REQUEST,
            ..
        })
    ));
    Ok(())
}

#[test]
fn every_interaction_kind_uses_its_end_to_end_capabilities() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session = session(provider_id)?;
    let provider_descriptor = provider(
        provider_id,
        ProviderCapabilities::APPROVAL_REQUEST
            | ProviderCapabilities::APPROVAL_RESPONSE
            | ProviderCapabilities::USER_INPUT_REQUEST
            | ProviderCapabilities::USER_INPUT_RESPONSE,
    )?;
    let channel_descriptor = channel(
        channel_id,
        ChannelCapabilities::APPROVAL
            | ChannelCapabilities::CHOICE_INPUT
            | ChannelCapabilities::TEXT_INPUT,
    )?;

    let option_id = ChoiceOptionId::new();
    let approval_option_id = ApprovalOptionId::new();
    let requests_and_responses = vec![
        (
            InteractionRequest::new(
                InteractionId::new(),
                session.id(),
                timestamp(110)?,
                text("Approve?")?,
                InteractionRequestPayload::Approval(approval(approval_option_id)?),
            ),
            InteractionResponsePayload::Approval(ApprovalSelection::new(approval_option_id)),
        ),
        (
            InteractionRequest::new(
                InteractionId::new(),
                session.id(),
                timestamp(110)?,
                text("Choose")?,
                InteractionRequestPayload::Choice(ChoiceRequest::new(
                    vec![ChoiceOption::new(option_id, text("First")?)],
                    false,
                )?),
            ),
            InteractionResponsePayload::Choice(ChoiceSelection::new(vec![option_id])?),
        ),
        (
            InteractionRequest::new(
                InteractionId::new(),
                session.id(),
                timestamp(110)?,
                text("Reply")?,
                InteractionRequestPayload::Text(TextInputRequest::new(false)),
            ),
            InteractionResponsePayload::Text(text("Continue")?),
        ),
    ];

    for (request, payload) in requests_and_responses {
        assert_eq!(
            CapabilityRouter::interaction_route(
                &provider_descriptor,
                &channel_descriptor,
                &session,
                &request,
            )?,
            InteractionRoute::Interactive
        );
        let response = InteractionResponse::new(
            request.id(),
            session.id(),
            channel_id,
            timestamp(120)?,
            payload,
        );
        CapabilityRouter::validate_interaction_response(
            &provider_descriptor,
            &channel_descriptor,
            &session,
            &request,
            &response,
        )?;
    }
    Ok(())
}

#[test]
fn interaction_responses_are_revalidated_before_provider_handoff() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session = session(provider_id)?;
    let provider_descriptor = provider(
        provider_id,
        ProviderCapabilities::USER_INPUT_REQUEST | ProviderCapabilities::USER_INPUT_RESPONSE,
    )?;
    let channel_descriptor = channel(channel_id, ChannelCapabilities::TEXT_INPUT)?;
    let request = InteractionRequest::new(
        InteractionId::new(),
        session.id(),
        timestamp(110)?,
        text("Reply")?,
        InteractionRequestPayload::Text(TextInputRequest::new(false)),
    )
    .with_expiration(timestamp(120)?)?;
    let expired = InteractionResponse::new(
        request.id(),
        session.id(),
        channel_id,
        timestamp(121)?,
        InteractionResponsePayload::Text(text("Too late")?),
    );

    assert!(matches!(
        CapabilityRouter::validate_interaction_response(
            &provider_descriptor,
            &channel_descriptor,
            &session,
            &request,
            &expired,
        ),
        Err(CapabilityRouteError::InvalidInteractionResponse {
            source: DomainError::InteractionExpired,
        })
    ));

    let wrong_channel = InteractionResponse::new(
        request.id(),
        session.id(),
        ChannelId::new(),
        timestamp(115)?,
        InteractionResponsePayload::Text(text("Wrong source")?),
    );
    assert!(matches!(
        CapabilityRouter::validate_interaction_response(
            &provider_descriptor,
            &channel_descriptor,
            &session,
            &request,
            &wrong_channel,
        ),
        Err(CapabilityRouteError::ChannelMismatch { .. })
    ));

    let wrong_type = InteractionResponse::new(
        request.id(),
        session.id(),
        channel_id,
        timestamp(115)?,
        InteractionResponsePayload::Approval(ApprovalSelection::new(ApprovalOptionId::new())),
    );
    assert!(matches!(
        CapabilityRouter::validate_interaction_response(
            &provider_descriptor,
            &channel_descriptor,
            &session,
            &request,
            &wrong_type,
        ),
        Err(CapabilityRouteError::InvalidInteractionResponse {
            source: DomainError::InteractionTypeMismatch,
        })
    ));

    let wrong_session = InteractionResponse::new(
        request.id(),
        SessionId::new(),
        channel_id,
        timestamp(115)?,
        InteractionResponsePayload::Text(text("Wrong session")?),
    );
    assert!(matches!(
        CapabilityRouter::validate_interaction_response(
            &provider_descriptor,
            &channel_descriptor,
            &session,
            &request,
            &wrong_session,
        ),
        Err(CapabilityRouteError::SessionMismatch { .. })
    ));
    Ok(())
}

#[test]
fn commands_require_the_complete_provider_and_channel_route() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session = session(provider_id)?;
    let provider_descriptor = provider(
        provider_id,
        ProviderCapabilities::PROMPT_SUBMIT | ProviderCapabilities::CANCEL,
    )?;
    let channel_descriptor = channel(
        channel_id,
        ChannelCapabilities::REMOTE_COMMAND | ChannelCapabilities::TEXT_INPUT,
    )?;
    let submit = AgentCommand::new(
        CommandId::new(),
        session.id(),
        channel_id,
        timestamp(110)?,
        AgentCommandPayload::SubmitPrompt {
            text: text("Continue")?,
        },
    );
    let cancel = AgentCommand::new(
        CommandId::new(),
        session.id(),
        channel_id,
        timestamp(110)?,
        AgentCommandPayload::CancelSession { reason: None },
    );

    CapabilityRouter::validate_command(
        &provider_descriptor,
        &channel_descriptor,
        &session,
        &submit,
    )?;
    CapabilityRouter::validate_command(
        &provider_descriptor,
        &channel_descriptor,
        &session,
        &cancel,
    )?;

    let no_text_channel = channel(channel_id, ChannelCapabilities::REMOTE_COMMAND)?;
    assert!(matches!(
        CapabilityRouter::validate_command(
            &provider_descriptor,
            &no_text_channel,
            &session,
            &submit,
        ),
        Err(CapabilityRouteError::MissingChannelCapabilities { .. })
    ));

    let no_cancel_provider = provider(provider_id, ProviderCapabilities::PROMPT_SUBMIT)?;
    assert!(matches!(
        CapabilityRouter::validate_command(
            &no_cancel_provider,
            &channel_descriptor,
            &session,
            &cancel,
        ),
        Err(CapabilityRouteError::MissingProviderCapabilities {
            required: ProviderCapabilities::CANCEL,
            ..
        })
    ));
    Ok(())
}
