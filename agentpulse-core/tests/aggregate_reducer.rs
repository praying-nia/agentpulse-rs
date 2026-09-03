//! Contract tests for deterministic session event reduction.

use std::error::Error;

use agentpulse_core::{
    AgentEvent, AgentEventPayload, AgentSession, AgentState, ApplyOutcome, ApprovalOptionId,
    ApprovalSelection, ChannelId, ConnectionState, DeterminateProgress, DomainError, EventId,
    EventSequence, InteractionId, InteractionRequest, InteractionRequestPayload,
    InteractionResponse, InteractionResponsePayload, NonEmptyText, PlanItem, PlanItemId,
    PlanItemStatus, PlanSnapshot, ProgressSnapshot, ProgressValue, ProviderId, ReduceError,
    Revision, SessionAggregate, SessionAggregateConfig, SessionId, SessionOutcome, SnapshotKind,
    TextInputRequest, Timestamp, ToolActivity, ToolCallId, ToolOutcome,
};

type TestResult = Result<(), Box<dyn Error>>;

fn text(value: &str) -> Result<NonEmptyText, DomainError> {
    NonEmptyText::new(value)
}

fn timestamp(nanoseconds: i128) -> Result<Timestamp, DomainError> {
    Timestamp::from_unix_timestamp_nanos(nanoseconds)
}

fn sequence(value: u64) -> Result<EventSequence, DomainError> {
    EventSequence::new(value)
}

fn event(
    session_id: SessionId,
    sequence_value: u64,
    occurred_at: i128,
    payload: AgentEventPayload,
) -> Result<AgentEvent, DomainError> {
    AgentEvent::new(
        EventId::new(),
        session_id,
        sequence(sequence_value)?,
        timestamp(occurred_at)?,
        payload,
    )
}

fn initial_event(session_id: SessionId, occurred_at: i128) -> Result<AgentEvent, DomainError> {
    let timestamp = timestamp(occurred_at)?;
    let session = AgentSession::builder(session_id, ProviderId::new(), timestamp).build()?;
    AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::FIRST,
        timestamp,
        AgentEventPayload::SessionStarted(session),
    )
}

#[test]
fn aggregate_requires_a_sequence_one_session_start() -> TestResult {
    assert_eq!(
        SessionAggregate::replay(Vec::<AgentEvent>::new()),
        Err(ReduceError::EmptyReplay)
    );

    let session_id = SessionId::new();
    let wrong_payload = event(
        session_id,
        1,
        100,
        AgentEventPayload::StateChanged(AgentState::Running),
    )?;
    assert_eq!(
        SessionAggregate::from_initial_event(wrong_payload),
        Err(ReduceError::InitialEventNotSessionStarted)
    );

    let timestamp = timestamp(100)?;
    let session = AgentSession::builder(session_id, ProviderId::new(), timestamp).build()?;
    let wrong_sequence = event(
        session_id,
        2,
        100,
        AgentEventPayload::SessionStarted(session),
    )?;
    assert_eq!(
        SessionAggregate::from_initial_event(wrong_sequence),
        Err(ReduceError::InvalidInitialSequence {
            actual: sequence(2)?,
        })
    );
    Ok(())
}

#[test]
fn replay_builds_the_same_complete_current_state() -> TestResult {
    let session_id = SessionId::new();
    let tool_id = ToolCallId::new();
    let interaction_id = InteractionId::new();
    let initial = initial_event(session_id, 100)?;
    let plan = PlanSnapshot::new(
        Revision::FIRST,
        vec![PlanItem::new(
            PlanItemId::new(),
            text("Implement reducer")?,
            PlanItemStatus::InProgress,
        )],
    )?;
    let progress = ProgressSnapshot::new(
        Revision::FIRST,
        ProgressValue::Determinate(DeterminateProgress::new(1, 2)?),
    );
    let request = InteractionRequest::new(
        interaction_id,
        session_id,
        timestamp(160)?,
        text("Continue?")?,
        InteractionRequestPayload::Text(TextInputRequest::new(false)),
    );
    let response = InteractionResponse::new(
        interaction_id,
        session_id,
        ChannelId::new(),
        timestamp(170)?,
        InteractionResponsePayload::Text(text("yes")?),
    );

    let events = vec![
        initial,
        event(
            session_id,
            2,
            110,
            AgentEventPayload::ConnectionChanged(ConnectionState::Disconnected),
        )?,
        event(
            session_id,
            3,
            120,
            AgentEventPayload::StateChanged(AgentState::WaitingForInteraction),
        )?,
        event(
            session_id,
            4,
            130,
            AgentEventPayload::PlanUpdated(plan.clone()),
        )?,
        event(
            session_id,
            5,
            140,
            AgentEventPayload::ProgressUpdated(progress.clone()),
        )?,
        event(
            session_id,
            6,
            150,
            AgentEventPayload::ToolActivity(ToolActivity::Started {
                call_id: tool_id,
                name: text("shell")?,
                summary: Some(text("Run checks")?),
            }),
        )?,
        event(
            session_id,
            7,
            160,
            AgentEventPayload::InteractionRequested(request),
        )?,
        event(
            session_id,
            8,
            170,
            AgentEventPayload::InteractionResponded(response),
        )?,
        event(
            session_id,
            9,
            180,
            AgentEventPayload::ToolActivity(ToolActivity::Finished {
                call_id: tool_id,
                outcome: ToolOutcome::Succeeded,
                summary: Some(text("Checks passed")?),
            }),
        )?,
        event(
            session_id,
            10,
            190,
            AgentEventPayload::SessionEnded(SessionOutcome::Completed {
                summary: Some(text("Done")?),
            }),
        )?,
    ];

    let first = SessionAggregate::replay(events.clone())?;
    let second = SessionAggregate::replay(events)?;

    assert_eq!(first, second);
    assert_eq!(first.session().state(), AgentState::Completed);
    assert_eq!(
        first.session().connection_state(),
        ConnectionState::Disconnected
    );
    assert_eq!(first.session().revision().get(), 4);
    assert_eq!(first.plan(), Some(&plan));
    assert_eq!(first.progress(), Some(&progress));
    assert_eq!(first.active_tool_calls().len(), 0);
    assert_eq!(first.pending_interactions().len(), 0);
    assert!(matches!(
        first.latest_outcome(),
        Some(SessionOutcome::Completed { .. })
    ));
    assert_eq!(first.last_sequence().get(), 10);
    assert_eq!(first.recent_events().len(), 10);
    Ok(())
}

#[test]
fn cursor_rules_are_strict_and_exact_last_event_retries_are_idempotent() -> TestResult {
    let session_id = SessionId::new();
    let initial = initial_event(session_id, 100)?;
    let mut aggregate = SessionAggregate::from_initial_event(initial.clone())?;
    let before_retry = aggregate.clone();

    assert_eq!(
        aggregate.apply(initial.clone())?,
        ApplyOutcome::AlreadyApplied
    );
    assert_eq!(aggregate, before_retry);

    let conflicting_initial = AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::FIRST,
        timestamp(100)?,
        initial.payload().clone(),
    )?;
    assert!(matches!(
        aggregate.apply(conflicting_initial),
        Err(ReduceError::SequenceConflict { .. })
    ));

    let second_start_timestamp = timestamp(110)?;
    let second_start_session =
        AgentSession::builder(session_id, ProviderId::new(), second_start_timestamp).build()?;
    let second_start = event(
        session_id,
        2,
        110,
        AgentEventPayload::SessionStarted(second_start_session),
    )?;
    assert_eq!(
        aggregate.apply(second_start),
        Err(ReduceError::UnexpectedSessionStarted)
    );
    assert_eq!(aggregate, before_retry);

    let running = event(
        session_id,
        2,
        110,
        AgentEventPayload::StateChanged(AgentState::Running),
    )?;
    assert_eq!(aggregate.apply(running)?, ApplyOutcome::Applied);

    let before_errors = aggregate.clone();
    assert!(matches!(
        aggregate.apply(initial),
        Err(ReduceError::StaleSequence { .. })
    ));
    assert_eq!(aggregate, before_errors);

    let gap = event(
        session_id,
        4,
        130,
        AgentEventPayload::StateChanged(AgentState::Idle),
    )?;
    assert!(matches!(
        aggregate.apply(gap),
        Err(ReduceError::SequenceGap { .. })
    ));
    assert_eq!(aggregate, before_errors);

    let wrong_session = event(
        SessionId::new(),
        3,
        120,
        AgentEventPayload::StateChanged(AgentState::Idle),
    )?;
    assert!(matches!(
        aggregate.apply(wrong_session),
        Err(ReduceError::SessionMismatch { .. })
    ));
    assert_eq!(aggregate, before_errors);
    Ok(())
}

#[test]
fn plan_and_progress_revisions_must_strictly_advance() -> TestResult {
    let session_id = SessionId::new();
    let mut aggregate = SessionAggregate::from_initial_event(initial_event(session_id, 100)?)?;
    let plan_v2 = PlanSnapshot::new(Revision::new(2)?, Vec::new())?;
    aggregate.apply(event(
        session_id,
        2,
        110,
        AgentEventPayload::PlanUpdated(plan_v2.clone()),
    )?)?;

    let before_stale_plan = aggregate.clone();
    let stale_plan = PlanSnapshot::new(Revision::new(2)?, Vec::new())?;
    assert_eq!(
        aggregate.apply(event(
            session_id,
            3,
            120,
            AgentEventPayload::PlanUpdated(stale_plan),
        )?),
        Err(ReduceError::StaleRevision {
            snapshot: SnapshotKind::Plan,
            current: Revision::new(2)?,
            incoming: Revision::new(2)?,
        })
    );
    assert_eq!(aggregate, before_stale_plan);

    let plan_v3 = PlanSnapshot::new(Revision::new(3)?, Vec::new())?;
    aggregate.apply(event(
        session_id,
        3,
        120,
        AgentEventPayload::PlanUpdated(plan_v3.clone()),
    )?)?;
    assert_eq!(aggregate.plan(), Some(&plan_v3));

    let progress_v1 = ProgressSnapshot::new(Revision::FIRST, ProgressValue::Indeterminate);
    aggregate.apply(event(
        session_id,
        4,
        130,
        AgentEventPayload::ProgressUpdated(progress_v1.clone()),
    )?)?;

    let before_stale_progress = aggregate.clone();
    assert!(matches!(
        aggregate.apply(event(
            session_id,
            5,
            140,
            AgentEventPayload::ProgressUpdated(progress_v1),
        )?),
        Err(ReduceError::StaleRevision {
            snapshot: SnapshotKind::Progress,
            ..
        })
    ));
    assert_eq!(aggregate, before_stale_progress);

    let progress_v2 = ProgressSnapshot::new(Revision::new(2)?, ProgressValue::Indeterminate);
    aggregate.apply(event(
        session_id,
        5,
        140,
        AgentEventPayload::ProgressUpdated(progress_v2.clone()),
    )?)?;
    assert_eq!(aggregate.progress(), Some(&progress_v2));
    Ok(())
}

#[test]
fn tool_calls_are_paired_and_session_end_clears_active_tools() -> TestResult {
    let session_id = SessionId::new();
    let tool_id = ToolCallId::new();
    let another_tool_id = ToolCallId::new();
    let mut aggregate = SessionAggregate::from_initial_event(initial_event(session_id, 100)?)?;
    let started = ToolActivity::Started {
        call_id: tool_id,
        name: text("shell")?,
        summary: None,
    };
    aggregate.apply(event(
        session_id,
        2,
        110,
        AgentEventPayload::ToolActivity(started.clone()),
    )?)?;
    assert_eq!(
        aggregate
            .active_tool_call(tool_id)
            .map(|tool| tool.name().as_str()),
        Some("shell")
    );

    let before_errors = aggregate.clone();
    assert!(matches!(
        aggregate.apply(event(
            session_id,
            3,
            120,
            AgentEventPayload::ToolActivity(started),
        )?),
        Err(ReduceError::ToolAlreadyActive { .. })
    ));
    assert_eq!(aggregate, before_errors);

    assert!(matches!(
        aggregate.apply(event(
            session_id,
            3,
            120,
            AgentEventPayload::ToolActivity(ToolActivity::Finished {
                call_id: another_tool_id,
                outcome: ToolOutcome::Failed,
                summary: None,
            }),
        )?),
        Err(ReduceError::ToolNotActive { .. })
    ));
    assert_eq!(aggregate, before_errors);

    aggregate.apply(event(
        session_id,
        3,
        120,
        AgentEventPayload::ToolActivity(ToolActivity::Finished {
            call_id: tool_id,
            outcome: ToolOutcome::Succeeded,
            summary: None,
        }),
    )?)?;
    assert!(aggregate.active_tool_call(tool_id).is_none());

    aggregate.apply(event(
        session_id,
        4,
        130,
        AgentEventPayload::ToolActivity(ToolActivity::Started {
            call_id: another_tool_id,
            name: text("editor")?,
            summary: None,
        }),
    )?)?;
    aggregate.apply(event(
        session_id,
        5,
        140,
        AgentEventPayload::SessionEnded(SessionOutcome::Cancelled { reason: None }),
    )?)?;
    assert_eq!(aggregate.active_tool_calls().len(), 0);
    assert_eq!(aggregate.session().state(), AgentState::Cancelled);
    Ok(())
}

#[test]
fn interactions_are_validated_paired_and_cleared_on_session_end() -> TestResult {
    let session_id = SessionId::new();
    let interaction_id = InteractionId::new();
    let mut aggregate = SessionAggregate::from_initial_event(initial_event(session_id, 90)?)?;
    let request = InteractionRequest::new(
        interaction_id,
        session_id,
        timestamp(100)?,
        text("Provide input")?,
        InteractionRequestPayload::Text(TextInputRequest::new(false)),
    )
    .with_expiration(timestamp(150)?)?;
    aggregate.apply(event(
        session_id,
        2,
        100,
        AgentEventPayload::InteractionRequested(request.clone()),
    )?)?;
    assert_eq!(
        aggregate.pending_interaction(interaction_id),
        Some(&request)
    );

    let before_errors = aggregate.clone();
    assert!(matches!(
        aggregate.apply(event(
            session_id,
            3,
            110,
            AgentEventPayload::InteractionRequested(request),
        )?),
        Err(ReduceError::InteractionAlreadyPending { .. })
    ));
    assert_eq!(aggregate, before_errors);

    let wrong_type = InteractionResponse::new(
        interaction_id,
        session_id,
        ChannelId::new(),
        timestamp(120)?,
        InteractionResponsePayload::Approval(ApprovalSelection::new(ApprovalOptionId::new())),
    );
    assert!(matches!(
        aggregate.apply(event(
            session_id,
            3,
            120,
            AgentEventPayload::InteractionResponded(wrong_type),
        )?),
        Err(ReduceError::InvalidInteractionResponse {
            source: DomainError::InteractionTypeMismatch,
            ..
        })
    ));
    assert_eq!(aggregate, before_errors);

    let expired = InteractionResponse::new(
        interaction_id,
        session_id,
        ChannelId::new(),
        timestamp(151)?,
        InteractionResponsePayload::Text(text("late")?),
    );
    assert!(matches!(
        aggregate.apply(event(
            session_id,
            3,
            151,
            AgentEventPayload::InteractionResponded(expired),
        )?),
        Err(ReduceError::InvalidInteractionResponse {
            source: DomainError::InteractionExpired,
            ..
        })
    ));
    assert_eq!(aggregate, before_errors);

    let valid = InteractionResponse::new(
        interaction_id,
        session_id,
        ChannelId::new(),
        timestamp(130)?,
        InteractionResponsePayload::Text(text("continue")?),
    );
    aggregate.apply(event(
        session_id,
        3,
        130,
        AgentEventPayload::InteractionResponded(valid.clone()),
    )?)?;
    assert!(aggregate.pending_interaction(interaction_id).is_none());

    let before_duplicate_response = aggregate.clone();
    assert!(matches!(
        aggregate.apply(event(
            session_id,
            4,
            140,
            AgentEventPayload::InteractionResponded(valid),
        )?),
        Err(ReduceError::InteractionNotPending { .. })
    ));
    assert_eq!(aggregate, before_duplicate_response);

    let second_request = InteractionRequest::new(
        InteractionId::new(),
        session_id,
        timestamp(140)?,
        text("One more input")?,
        InteractionRequestPayload::Text(TextInputRequest::new(true)),
    );
    aggregate.apply(event(
        session_id,
        4,
        140,
        AgentEventPayload::InteractionRequested(second_request),
    )?)?;
    aggregate.apply(event(
        session_id,
        5,
        150,
        AgentEventPayload::SessionEnded(SessionOutcome::Failed {
            error: text("Stopped")?,
        }),
    )?)?;
    assert_eq!(aggregate.pending_interactions().len(), 0);
    assert_eq!(aggregate.session().state(), AgentState::Failed);
    Ok(())
}

#[test]
fn recent_event_retention_is_bounded_and_optional() -> TestResult {
    assert_eq!(
        SessionAggregateConfig::default().recent_event_capacity(),
        256
    );

    let session_id = SessionId::new();
    let mut bounded = SessionAggregate::from_initial_event_with_config(
        initial_event(session_id, 100)?,
        SessionAggregateConfig::new(2),
    )?;
    bounded.apply(event(
        session_id,
        2,
        110,
        AgentEventPayload::StateChanged(AgentState::Running),
    )?)?;
    bounded.apply(event(
        session_id,
        3,
        120,
        AgentEventPayload::ConnectionChanged(ConnectionState::Reconnecting),
    )?)?;
    let retained_sequences = bounded
        .recent_events()
        .map(AgentEvent::sequence)
        .map(EventSequence::get)
        .collect::<Vec<_>>();
    assert_eq!(retained_sequences, vec![2, 3]);
    assert_eq!(bounded.session().state(), AgentState::Running);
    assert_eq!(
        bounded.session().connection_state(),
        ConnectionState::Reconnecting
    );

    let session_id = SessionId::new();
    let mut disabled = SessionAggregate::from_initial_event_with_config(
        initial_event(session_id, 100)?,
        SessionAggregateConfig::new(0),
    )?;
    assert_eq!(disabled.recent_events().len(), 0);
    disabled.apply(event(
        session_id,
        2,
        110,
        AgentEventPayload::StateChanged(AgentState::Running),
    )?)?;
    assert_eq!(disabled.recent_events().len(), 0);
    assert_eq!(disabled.last_sequence().get(), 2);
    assert_eq!(disabled.session().state(), AgentState::Running);
    Ok(())
}

#[test]
fn session_revision_advances_without_allowing_updated_at_to_regress() -> TestResult {
    let session_id = SessionId::new();
    let created_at = timestamp(100)?;
    let updated_at = timestamp(200)?;
    let session = AgentSession::builder(session_id, ProviderId::new(), created_at)
        .revision(Revision::FIRST, updated_at)
        .build()?;
    let initial = AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::FIRST,
        updated_at,
        AgentEventPayload::SessionStarted(session),
    )?;
    let mut aggregate = SessionAggregate::from_initial_event(initial)?;

    aggregate.apply(event(
        session_id,
        2,
        150,
        AgentEventPayload::StateChanged(AgentState::Running),
    )?)?;
    assert_eq!(aggregate.session().revision().get(), 2);
    assert_eq!(aggregate.session().updated_at(), updated_at);

    aggregate.apply(event(
        session_id,
        3,
        250,
        AgentEventPayload::ConnectionChanged(ConnectionState::Disconnected),
    )?)?;
    assert_eq!(aggregate.session().revision().get(), 3);
    assert_eq!(aggregate.session().updated_at(), timestamp(250)?);

    aggregate.apply(event(
        session_id,
        4,
        240,
        AgentEventPayload::SessionEnded(SessionOutcome::Completed { summary: None }),
    )?)?;
    assert_eq!(aggregate.session().revision().get(), 4);
    assert_eq!(aggregate.session().updated_at(), timestamp(250)?);
    assert_eq!(aggregate.session().state(), AgentState::Completed);
    Ok(())
}

#[test]
fn exhausted_session_revisions_fail_without_partial_state_changes() -> TestResult {
    let session_id = SessionId::new();
    let observed_at = timestamp(100)?;
    let maximum_revision = Revision::new(u64::MAX)?;
    assert!(maximum_revision.checked_next().is_none());

    let session = AgentSession::builder(session_id, ProviderId::new(), observed_at)
        .revision(maximum_revision, observed_at)
        .build()?;
    let initial = AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::FIRST,
        observed_at,
        AgentEventPayload::SessionStarted(session),
    )?;
    let mut aggregate = SessionAggregate::from_initial_event(initial)?;
    let before = aggregate.clone();

    assert_eq!(
        aggregate.apply(event(
            session_id,
            2,
            110,
            AgentEventPayload::StateChanged(AgentState::Running),
        )?),
        Err(ReduceError::SessionRevisionExhausted)
    );
    assert_eq!(aggregate, before);
    Ok(())
}

#[test]
fn every_session_outcome_maps_to_its_execution_state() -> TestResult {
    let cases = [
        (
            SessionOutcome::Completed { summary: None },
            AgentState::Completed,
        ),
        (
            SessionOutcome::Failed {
                error: text("failed")?,
            },
            AgentState::Failed,
        ),
        (
            SessionOutcome::Cancelled { reason: None },
            AgentState::Cancelled,
        ),
    ];

    for (outcome, expected_state) in cases {
        let session_id = SessionId::new();
        let mut aggregate = SessionAggregate::from_initial_event(initial_event(session_id, 100)?)?;
        aggregate.apply(event(
            session_id,
            2,
            110,
            AgentEventPayload::SessionEnded(outcome.clone()),
        )?)?;
        assert_eq!(aggregate.session().state(), expected_state);
        assert_eq!(aggregate.latest_outcome(), Some(&outcome));
    }
    Ok(())
}
