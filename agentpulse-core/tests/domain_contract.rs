//! Public contract tests for the AgentPulse domain model.

use std::str::FromStr;

use agentpulse_core::{
    AgentCommand, AgentCommandPayload, AgentEvent, AgentEventPayload, AgentSession, AgentState,
    ApprovalDecision, ApprovalRequest, ApprovalScope, ChannelCapabilities, ChannelId, ChoiceOption,
    ChoiceOptionId, ChoiceRequest, ChoiceSelection, CommandId, ConnectionState,
    DeterminateProgress, DomainError, EventId, EventSequence, InteractionId, InteractionRequest,
    InteractionRequestPayload, InteractionResponse, InteractionResponsePayload, NonEmptyText,
    PlanItem, PlanItemId, PlanItemStatus, PlanSnapshot, ProgressSnapshot, ProgressValue,
    ProviderCapabilities, ProviderId, ProviderKind, Revision, SessionId, Timestamp,
};

fn text(value: &str) -> Result<NonEmptyText, DomainError> {
    NonEmptyText::new(value)
}

fn timestamp(nanoseconds: i128) -> Result<Timestamp, DomainError> {
    Timestamp::from_unix_timestamp_nanos(nanoseconds)
}

#[test]
fn typed_ids_generate_and_parse_only_uuid_v7() -> Result<(), DomainError> {
    let session_id = SessionId::new();
    let parsed = SessionId::from_str(&session_id.to_string())?;

    assert_eq!(parsed, session_id);
    assert!(SessionId::from_str("550e8400-e29b-41d4-a716-446655440000").is_err());
    assert!(SessionId::from_str("not-a-uuid").is_err());
    Ok(())
}

#[test]
fn shared_text_and_component_kinds_enforce_local_invariants() -> Result<(), DomainError> {
    assert!(NonEmptyText::new(" \n\t ").is_err());
    assert_eq!(text("  保留原文  ")?.as_str(), "  保留原文  ");
    assert_eq!(ProviderKind::new("claude-code")?.as_str(), "claude-code");
    assert!(ProviderKind::new("Claude Code").is_err());
    assert!(Revision::new(0).is_err());
    assert!(EventSequence::new(0).is_err());
    Ok(())
}

#[test]
fn session_execution_and_connection_states_are_independent() -> Result<(), DomainError> {
    let created_at = timestamp(100)?;
    let updated_at = timestamp(200)?;
    let session = AgentSession::builder(SessionId::new(), ProviderId::new(), created_at)
        .state(AgentState::Running)
        .connection_state(ConnectionState::Disconnected)
        .revision(Revision::new(2)?, updated_at)
        .build()?;

    assert_eq!(session.state(), AgentState::Running);
    assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    assert_eq!(session.revision().get(), 2);

    let invalid = AgentSession::builder(SessionId::new(), ProviderId::new(), created_at)
        .revision(Revision::new(2)?, timestamp(99)?)
        .build();
    assert!(matches!(invalid, Err(DomainError::InvalidTimeOrder { .. })));
    Ok(())
}

#[test]
fn plan_snapshots_are_complete_and_reject_duplicate_items() -> Result<(), DomainError> {
    let item_id = PlanItemId::new();
    let first = PlanItem::new(
        item_id,
        text("Inspect repository")?,
        PlanItemStatus::Completed,
    );
    let duplicate = PlanItem::new(
        item_id,
        text("Implement model")?,
        PlanItemStatus::InProgress,
    );

    assert!(PlanSnapshot::new(Revision::FIRST, vec![first, duplicate]).is_err());
    assert!(
        PlanSnapshot::new(Revision::new(2)?, Vec::new())?
            .items()
            .is_empty()
    );
    Ok(())
}

#[test]
fn progress_supports_indeterminate_and_validated_determinate_values() -> Result<(), DomainError> {
    assert!(DeterminateProgress::new(0, 0).is_err());
    assert!(DeterminateProgress::new(3, 2).is_err());

    let determinate = DeterminateProgress::new(2, 3)?;
    let snapshot = ProgressSnapshot::new(Revision::FIRST, ProgressValue::Determinate(determinate));
    assert_eq!(determinate.completed(), 2);
    assert_eq!(determinate.total(), 3);
    assert_eq!(snapshot.value(), ProgressValue::Determinate(determinate));

    let indeterminate = ProgressSnapshot::new(Revision::new(2)?, ProgressValue::Indeterminate);
    assert_eq!(indeterminate.value(), ProgressValue::Indeterminate);
    Ok(())
}

#[test]
fn approval_responses_must_use_an_offered_scope() -> Result<(), DomainError> {
    assert!(matches!(
        ApprovalRequest::new(vec![ApprovalScope::Once, ApprovalScope::Once]),
        Err(DomainError::DuplicateValue { .. })
    ));

    let session_id = SessionId::new();
    let request = InteractionRequest::new(
        InteractionId::new(),
        session_id,
        timestamp(100)?,
        text("Allow this operation?")?,
        InteractionRequestPayload::Approval(ApprovalRequest::new(vec![ApprovalScope::Once])?),
    );
    let response = InteractionResponse::new(
        request.id(),
        session_id,
        ChannelId::new(),
        timestamp(110)?,
        InteractionResponsePayload::Approval(ApprovalDecision::Approved(ApprovalScope::Session)),
    );

    assert_eq!(
        request.validate_response(&response),
        Err(DomainError::ApprovalScopeNotAllowed)
    );
    assert_eq!(
        request.required_provider_request_capability(),
        ProviderCapabilities::APPROVAL_REQUEST
    );
    assert_eq!(
        request.required_channel_response_capability(),
        ChannelCapabilities::APPROVAL
    );
    Ok(())
}

#[test]
fn choice_responses_are_correlated_and_respect_selection_mode() -> Result<(), DomainError> {
    let first_id = ChoiceOptionId::new();
    let second_id = ChoiceOptionId::new();
    let session_id = SessionId::new();
    let request = InteractionRequest::new(
        InteractionId::new(),
        session_id,
        timestamp(100)?,
        text("Choose one")?,
        InteractionRequestPayload::Choice(ChoiceRequest::new(
            vec![
                ChoiceOption::new(first_id, text("First")?),
                ChoiceOption::new(second_id, text("Second")?),
            ],
            false,
        )?),
    );

    assert!(matches!(
        ChoiceSelection::new(vec![first_id, first_id]),
        Err(DomainError::DuplicateId { .. })
    ));

    let valid = InteractionResponse::new(
        request.id(),
        session_id,
        ChannelId::new(),
        timestamp(110)?,
        InteractionResponsePayload::Choice(ChoiceSelection::new(vec![first_id])?),
    );
    request.validate_response(&valid)?;

    let too_many = InteractionResponse::new(
        request.id(),
        session_id,
        ChannelId::new(),
        timestamp(110)?,
        InteractionResponsePayload::Choice(ChoiceSelection::new(vec![first_id, second_id])?),
    );
    assert!(matches!(
        request.validate_response(&too_many),
        Err(DomainError::InvalidChoiceSelection { .. })
    ));

    let unknown = InteractionResponse::new(
        request.id(),
        session_id,
        ChannelId::new(),
        timestamp(110)?,
        InteractionResponsePayload::Choice(ChoiceSelection::new(vec![ChoiceOptionId::new()])?),
    );
    assert!(matches!(
        request.validate_response(&unknown),
        Err(DomainError::UnknownChoiceOption { .. })
    ));

    let wrong_session = InteractionResponse::new(
        request.id(),
        SessionId::new(),
        ChannelId::new(),
        timestamp(110)?,
        InteractionResponsePayload::Choice(ChoiceSelection::new(vec![first_id])?),
    );
    assert!(matches!(
        request.validate_response(&wrong_session),
        Err(DomainError::CorrelationMismatch { .. })
    ));
    Ok(())
}

#[test]
fn interactions_reject_mismatched_types_and_expired_responses() -> Result<(), DomainError> {
    let session_id = SessionId::new();
    let request = InteractionRequest::new(
        InteractionId::new(),
        session_id,
        timestamp(100)?,
        text("Reply")?,
        InteractionRequestPayload::Text(agentpulse_core::TextInputRequest::new(false)),
    )
    .with_expiration(timestamp(120)?)?;

    let mismatched = InteractionResponse::new(
        request.id(),
        session_id,
        ChannelId::new(),
        timestamp(110)?,
        InteractionResponsePayload::Approval(ApprovalDecision::Rejected { reason: None }),
    );
    assert_eq!(
        request.validate_response(&mismatched),
        Err(DomainError::InteractionTypeMismatch)
    );

    let expired = InteractionResponse::new(
        request.id(),
        session_id,
        ChannelId::new(),
        timestamp(121)?,
        InteractionResponsePayload::Text(text("late")?),
    );
    assert_eq!(
        request.validate_response(&expired),
        Err(DomainError::InteractionExpired)
    );
    Ok(())
}

#[test]
fn commands_expose_route_capability_requirements() -> Result<(), DomainError> {
    let command = AgentCommand::new(
        CommandId::new(),
        SessionId::new(),
        ChannelId::new(),
        timestamp(100)?,
        AgentCommandPayload::SubmitPrompt {
            text: text("Continue")?,
        },
    );

    assert_eq!(
        command.required_provider_capability(),
        ProviderCapabilities::PROMPT_SUBMIT
    );
    assert!(
        command
            .required_channel_capabilities()
            .contains(ChannelCapabilities::REMOTE_COMMAND | ChannelCapabilities::TEXT_INPUT)
    );
    Ok(())
}

#[test]
fn events_reject_embedded_objects_from_another_session() -> Result<(), DomainError> {
    let event_session_id = SessionId::new();
    let embedded =
        AgentSession::builder(SessionId::new(), ProviderId::new(), timestamp(100)?).build()?;

    let event = AgentEvent::new(
        EventId::new(),
        event_session_id,
        EventSequence::FIRST,
        timestamp(100)?,
        AgentEventPayload::SessionStarted(embedded),
    );

    assert!(matches!(
        event,
        Err(DomainError::CorrelationMismatch { .. })
    ));
    Ok(())
}
