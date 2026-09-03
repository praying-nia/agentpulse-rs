//! Public contract tests for the versioned AgentPulse JSON protocol.

use std::{error::Error, fs, path::Path, str::FromStr};

use agentpulse_core::{
    AgentCommand, AgentCommandPayload, AgentEvent, AgentEventPayload, AgentMessage,
    AgentMessageLevel, AgentSession, AgentState, ApprovalCommandKind, ApprovalDisposition,
    ApprovalOption, ApprovalOptionId, ApprovalRequest, ApprovalSelection, ApprovalSubject,
    ChannelCapabilities, ChannelDescriptor, ChannelId, ChannelKind, ChoiceOption, ChoiceOptionId,
    ChoiceRequest, ChoiceSelection, CommandId, ConnectionState, DeterminateProgress, DomainError,
    EventId, EventSequence, ExternalId, InteractionId, InteractionRequest,
    InteractionRequestPayload, InteractionResponse, InteractionResponsePayload, NonEmptyText,
    PlanItem, PlanItemId, PlanItemStatus, PlanSnapshot, ProgressSnapshot, ProgressValue,
    PromptDelivery, ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind,
    QueueAction, Revision, SessionId, SessionOutcome, TextInputRequest, Timestamp, ToolActivity,
    ToolCallId, ToolOutcome, WorkspaceRef,
};
use agentpulse_protocol::{
    ProtocolError, ProtocolMessage, V2_PROTOCOL_VERSION, decode_json, encode_json,
};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

type TestResult = Result<(), Box<dyn Error>>;

const FIXTURES: [&str; 9] = [
    "provider_descriptor.json",
    "channel_descriptor.json",
    "agent_session.json",
    "agent_event.json",
    "approval_request.json",
    "approval_response.json",
    "interaction_closed_event.json",
    "interaction_response.json",
    "agent_command.json",
];

fn fixed_id<T>(suffix: u64) -> Result<T, DomainError>
where
    T: FromStr<Err = DomainError>,
{
    T::from_str(&format!("01890f47-7c00-7000-8000-{suffix:012x}"))
}

fn text(value: &str) -> Result<NonEmptyText, DomainError> {
    NonEmptyText::new(value)
}

fn timestamp(value: &str) -> Result<Timestamp, time::error::Parse> {
    OffsetDateTime::parse(value, &Rfc3339).map(Timestamp::from_offset_date_time)
}

fn all_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities::SESSION_STATE
        | ProviderCapabilities::TOOL_EVENTS
        | ProviderCapabilities::PLAN
        | ProviderCapabilities::PROGRESS
        | ProviderCapabilities::APPROVAL_REQUEST
        | ProviderCapabilities::APPROVAL_RESPONSE
        | ProviderCapabilities::USER_INPUT_REQUEST
        | ProviderCapabilities::USER_INPUT_RESPONSE
        | ProviderCapabilities::PROMPT_SUBMIT
        | ProviderCapabilities::CANCEL
        | ProviderCapabilities::CONTROL
}

fn all_channel_capabilities() -> ChannelCapabilities {
    ChannelCapabilities::NOTIFICATION
        | ChannelCapabilities::SESSION_VIEW
        | ChannelCapabilities::TOOL_VIEW
        | ChannelCapabilities::PLAN_VIEW
        | ChannelCapabilities::PROGRESS_VIEW
        | ChannelCapabilities::RICH_MESSAGE
        | ChannelCapabilities::APPROVAL
        | ChannelCapabilities::CHOICE_INPUT
        | ChannelCapabilities::TEXT_INPUT
        | ChannelCapabilities::FORM_INPUT
        | ChannelCapabilities::REALTIME_SYNC
        | ChannelCapabilities::REMOTE_COMMAND
}

fn complete_session() -> Result<AgentSession, Box<dyn Error>> {
    let workspace =
        WorkspaceRef::new(text("/workspace/agentpulse")?).with_display_name(text("AgentPulse")?);
    Ok(AgentSession::builder(
        fixed_id::<SessionId>(3)?,
        fixed_id::<ProviderId>(1)?,
        timestamp("2026-08-29T00:00:00Z")?,
    )
    .external_id(ExternalId::new("codex-session-42")?)
    .title(text("Implement JSON protocol")?)
    .workspace(workspace)
    .state(AgentState::Running)
    .connection_state(ConnectionState::Connected)
    .revision(Revision::new(3)?, timestamp("2026-08-29T00:02:00Z")?)
    .build()?)
}

fn round_trip(message: ProtocolMessage) -> Result<(), ProtocolError> {
    let encoded = encode_json(&message)?;
    let decoded = decode_json(&encoded)?;
    assert_eq!(decoded, message);
    Ok(())
}

#[test]
fn every_top_level_message_round_trips() -> TestResult {
    let provider = ProviderDescriptor::new(
        fixed_id::<ProviderId>(1)?,
        ProviderKind::new("codex")?,
        text("Codex Provider")?,
        all_provider_capabilities(),
    )
    .with_version(text("1.0.0")?);
    round_trip(ProtocolMessage::ProviderDescriptor(provider))?;

    let channel = ChannelDescriptor::new(
        fixed_id::<ChannelId>(2)?,
        ChannelKind::new("native")?,
        text("Native Channel")?,
        all_channel_capabilities(),
    )
    .with_version(text("1.0.0")?);
    round_trip(ProtocolMessage::ChannelDescriptor(channel))?;
    round_trip(ProtocolMessage::AgentSession(complete_session()?))?;

    let session_id = fixed_id::<SessionId>(3)?;
    let event = AgentEvent::new(
        fixed_id::<EventId>(4)?,
        session_id,
        EventSequence::new(2)?,
        timestamp("2026-08-29T00:03:00Z")?,
        AgentEventPayload::Message(AgentMessage::new(
            AgentMessageLevel::Info,
            text("Protocol initialized")?,
        )),
    )?;
    round_trip(ProtocolMessage::AgentEvent(event))?;

    let request = InteractionRequest::new(
        fixed_id::<InteractionId>(5)?,
        session_id,
        timestamp("2026-08-29T00:03:00Z")?,
        text("Approve command execution")?,
        InteractionRequestPayload::Approval(ApprovalRequest::actionable(
            ApprovalSubject::Command {
                kind: ApprovalCommandKind::Command,
                command: Some(text("cargo test --workspace")?),
                cwd: Some(text("/workspace/agentpulse")?),
                reason: Some(text("Run the workspace checks")?),
                network: None,
            },
            vec![ApprovalOption::new(
                fixed_id::<ApprovalOptionId>(7)?,
                ApprovalDisposition::Approve,
                text("Approve once")?,
            )],
        )?),
    );
    round_trip(ProtocolMessage::InteractionRequest(request))?;

    let response = InteractionResponse::new(
        fixed_id::<InteractionId>(5)?,
        session_id,
        fixed_id::<ChannelId>(2)?,
        timestamp("2026-08-29T00:04:00Z")?,
        InteractionResponsePayload::Text(text("continue")?),
    );
    round_trip(ProtocolMessage::InteractionResponse(response))?;

    let command = AgentCommand::new(
        fixed_id::<CommandId>(6)?,
        session_id,
        fixed_id::<ChannelId>(2)?,
        timestamp("2026-08-29T00:05:00Z")?,
        AgentCommandPayload::SubmitPrompt {
            text: text("Continue")?,
            delivery: agentpulse_core::PromptDelivery::Queue,
        },
    );
    round_trip(ProtocolMessage::AgentCommand(command))?;
    Ok(())
}

#[test]
fn every_remote_command_payload_round_trips() -> TestResult {
    let payloads = vec![
        AgentCommandPayload::SubmitPrompt {
            text: text("continue")?,
            delivery: PromptDelivery::Queue,
        },
        AgentCommandPayload::SubmitPrompt {
            text: text("adjust")?,
            delivery: PromptDelivery::Steer,
        },
        AgentCommandPayload::CancelSession { reason: None },
        AgentCommandPayload::ListModels,
        AgentCommandPayload::SelectModel {
            model: text("gpt-5")?,
            effort: Some(text("high")?),
        },
        AgentCommandPayload::SetPlanMode { enabled: true },
        AgentCommandPayload::ListThreads {
            cursor: Some(text("next")?),
        },
        AgentCommandPayload::ResumeThread {
            thread_id: text("thread-1")?,
        },
        AgentCommandPayload::StartThread {
            cwd: text("/workspace")?,
        },
        AgentCommandPayload::Compact,
        AgentCommandPayload::Review {
            instructions: Some(text("security")?),
        },
        AgentCommandPayload::Rename {
            name: text("Remote control")?,
        },
        AgentCommandPayload::Fork,
        AgentCommandPayload::Status,
        AgentCommandPayload::ListPermissionProfiles,
        AgentCommandPayload::SelectPermissionProfile {
            profile: text("workspace-write")?,
        },
        AgentCommandPayload::Queue {
            action: QueueAction::Pause,
        },
        AgentCommandPayload::Queue {
            action: QueueAction::Resume,
        },
        AgentCommandPayload::Queue {
            action: QueueAction::Clear,
        },
    ];
    for (index, payload) in payloads.into_iter().enumerate() {
        let command = AgentCommand::new(
            fixed_id::<CommandId>(100 + index as u64)?,
            fixed_id::<SessionId>(3)?,
            fixed_id::<ChannelId>(2)?,
            timestamp("2026-09-03T00:05:00Z")?,
            payload,
        );
        round_trip(ProtocolMessage::AgentCommand(command))?;
    }
    Ok(())
}

#[test]
fn every_nested_event_variant_round_trips() -> TestResult {
    let session_id = fixed_id::<SessionId>(3)?;
    let channel_id = fixed_id::<ChannelId>(2)?;
    let interaction_id = fixed_id::<InteractionId>(5)?;
    let option_one = fixed_id::<ChoiceOptionId>(7)?;
    let option_two = fixed_id::<ChoiceOptionId>(8)?;
    let approval_once = fixed_id::<ApprovalOptionId>(9)?;
    let approval_reject = fixed_id::<ApprovalOptionId>(12)?;
    let tool_id = fixed_id::<ToolCallId>(10)?;
    let occurred_at = timestamp("2026-08-29T00:03:00Z")?;

    let mut payloads = vec![AgentEventPayload::SessionStarted(complete_session()?)];
    for state in [
        AgentState::Initializing,
        AgentState::Idle,
        AgentState::Running,
        AgentState::WaitingForInteraction,
        AgentState::Completed,
        AgentState::Failed,
        AgentState::Cancelled,
    ] {
        payloads.push(AgentEventPayload::StateChanged(state));
    }
    for state in [
        ConnectionState::Connected,
        ConnectionState::Reconnecting,
        ConnectionState::Disconnected,
    ] {
        payloads.push(AgentEventPayload::ConnectionChanged(state));
    }
    for level in [
        AgentMessageLevel::Info,
        AgentMessageLevel::Warning,
        AgentMessageLevel::Error,
    ] {
        payloads.push(AgentEventPayload::Message(AgentMessage::new(
            level,
            text("Message")?,
        )));
    }
    payloads.push(AgentEventPayload::ToolActivity(ToolActivity::Started {
        call_id: tool_id,
        name: text("shell")?,
        summary: Some(text("Run checks")?),
    }));
    for outcome in [
        ToolOutcome::Succeeded,
        ToolOutcome::Failed,
        ToolOutcome::Cancelled,
    ] {
        payloads.push(AgentEventPayload::ToolActivity(ToolActivity::Finished {
            call_id: tool_id,
            outcome,
            summary: None,
        }));
    }

    let statuses = [
        PlanItemStatus::Pending,
        PlanItemStatus::InProgress,
        PlanItemStatus::Completed,
        PlanItemStatus::Blocked,
        PlanItemStatus::Skipped,
    ];
    let plan_items = statuses
        .into_iter()
        .enumerate()
        .map(|(index, status)| {
            Ok(PlanItem::new(
                fixed_id::<PlanItemId>(100 + index as u64)?,
                text("Plan item")?,
                status,
            ))
        })
        .collect::<Result<Vec<_>, DomainError>>()?;
    payloads.push(AgentEventPayload::PlanUpdated(
        PlanSnapshot::new(Revision::new(2)?, plan_items)?.with_explanation(text("Current plan")?),
    ));
    payloads.push(AgentEventPayload::ProgressUpdated(
        ProgressSnapshot::new(Revision::new(2)?, ProgressValue::Indeterminate)
            .with_message(text("Discovering")?),
    ));
    payloads.push(AgentEventPayload::ProgressUpdated(ProgressSnapshot::new(
        Revision::new(3)?,
        ProgressValue::Determinate(DeterminateProgress::new(2, 5)?),
    )));

    let approval = InteractionRequest::new(
        interaction_id,
        session_id,
        occurred_at,
        text("Approve?")?,
        InteractionRequestPayload::Approval(ApprovalRequest::actionable(
            ApprovalSubject::Command {
                kind: ApprovalCommandKind::Command,
                command: Some(text("cargo test --workspace")?),
                cwd: Some(text("/workspace")?),
                reason: Some(text("Run the test suite")?),
                network: None,
            },
            vec![
                ApprovalOption::new(
                    approval_once,
                    ApprovalDisposition::Approve,
                    text("Approve once")?,
                ),
                ApprovalOption::new(
                    approval_reject,
                    ApprovalDisposition::Reject,
                    text("Reject")?,
                ),
            ],
        )?),
    );
    payloads.push(AgentEventPayload::InteractionRequested(approval));

    let choice = InteractionRequest::new(
        interaction_id,
        session_id,
        occurred_at,
        text("Choose")?,
        InteractionRequestPayload::Choice(ChoiceRequest::new(
            vec![
                ChoiceOption::new(option_one, text("One")?).with_description(text("First option")?),
                ChoiceOption::new(option_two, text("Two")?),
            ],
            true,
        )?),
    )
    .with_expiration(timestamp("2026-08-29T00:08:00Z")?)?;
    payloads.push(AgentEventPayload::InteractionRequested(choice));

    let text_request = InteractionRequest::new(
        interaction_id,
        session_id,
        occurred_at,
        text("Explain")?,
        InteractionRequestPayload::Text(
            TextInputRequest::new(true).with_placeholder(text("Details")?),
        ),
    );
    payloads.push(AgentEventPayload::InteractionRequested(text_request));

    for selection in [approval_once, approval_reject] {
        payloads.push(AgentEventPayload::InteractionResponded(
            InteractionResponse::new(
                interaction_id,
                session_id,
                channel_id,
                occurred_at,
                InteractionResponsePayload::Approval(ApprovalSelection::new(selection)),
            ),
        ));
    }
    payloads.push(AgentEventPayload::InteractionResponded(
        InteractionResponse::new(
            interaction_id,
            session_id,
            channel_id,
            occurred_at,
            InteractionResponsePayload::Choice(ChoiceSelection::new(vec![option_one, option_two])?),
        ),
    ));
    payloads.push(AgentEventPayload::InteractionResponded(
        InteractionResponse::new(
            interaction_id,
            session_id,
            channel_id,
            occurred_at,
            InteractionResponsePayload::Text(text("Answer")?),
        ),
    ));

    payloads.push(AgentEventPayload::CommandIssued(AgentCommand::new(
        fixed_id::<CommandId>(6)?,
        session_id,
        channel_id,
        occurred_at,
        AgentCommandPayload::SubmitPrompt {
            text: text("Continue")?,
            delivery: agentpulse_core::PromptDelivery::Queue,
        },
    )));
    payloads.push(AgentEventPayload::CommandIssued(AgentCommand::new(
        fixed_id::<CommandId>(11)?,
        session_id,
        channel_id,
        occurred_at,
        AgentCommandPayload::CancelSession {
            reason: Some(text("User requested")?),
        },
    )));
    payloads.push(AgentEventPayload::SessionEnded(SessionOutcome::Completed {
        summary: Some(text("Done")?),
    }));
    payloads.push(AgentEventPayload::SessionEnded(SessionOutcome::Failed {
        error: text("Failed")?,
    }));
    payloads.push(AgentEventPayload::SessionEnded(SessionOutcome::Cancelled {
        reason: None,
    }));

    for (index, payload) in payloads.into_iter().enumerate() {
        let event = AgentEvent::new(
            fixed_id::<EventId>(1_000 + index as u64)?,
            session_id,
            EventSequence::new(index as u64 + 1)?,
            occurred_at,
            payload,
        )?;
        round_trip(ProtocolMessage::AgentEvent(event))?;
    }
    Ok(())
}

#[test]
fn golden_fixtures_decode_and_reencode_without_semantic_drift() -> TestResult {
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2");
    let canonical =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agentpulse-protocol/fixtures/v2");

    for name in FIXTURES {
        let bytes = fs::read(local.join(name))?;
        let expected: Value = serde_json::from_slice(&bytes)?;
        let message = decode_json(&bytes)?;
        let encoded = encode_json(&message)?;
        let actual: Value = serde_json::from_slice(&encoded)?;
        assert_eq!(actual, expected, "fixture changed semantically: {name}");

        if canonical.is_dir() {
            let canonical_bytes = fs::read(canonical.join(name))?;
            assert_eq!(bytes, canonical_bytes, "fixture mirror drifted: {name}");
        }
    }
    Ok(())
}

#[test]
fn protocol_version_and_structure_are_strict() -> TestResult {
    let fixture = include_bytes!("fixtures/v2/provider_descriptor.json");
    let mut value: Value = serde_json::from_slice(fixture)?;
    value["protocol_version"] = json!(1);
    let encoded = serde_json::to_vec(&value)?;
    assert!(matches!(
        decode_json(&encoded),
        Err(ProtocolError::UnsupportedProtocolVersion {
            received: 1,
            supported: V2_PROTOCOL_VERSION,
        })
    ));

    let mut unknown_root: Value = serde_json::from_slice(fixture)?;
    unknown_root["unexpected"] = json!(true);
    assert!(matches!(
        decode_json(&serde_json::to_vec(&unknown_root)?),
        Err(ProtocolError::JsonDecode { .. })
    ));

    let mut unknown_message: Value = serde_json::from_slice(fixture)?;
    unknown_message["message"]["unexpected"] = json!(true);
    assert!(matches!(
        decode_json(&serde_json::to_vec(&unknown_message)?),
        Err(ProtocolError::JsonDecode { .. })
    ));

    let mut unknown_field: Value = serde_json::from_slice(fixture)?;
    unknown_field["message"]["payload"]["unexpected"] = json!(true);
    assert!(matches!(
        decode_json(&serde_json::to_vec(&unknown_field)?),
        Err(ProtocolError::JsonDecode { .. })
    ));

    let mut unknown_type: Value = serde_json::from_slice(fixture)?;
    unknown_type["message"]["type"] = json!("future_message");
    assert!(matches!(
        decode_json(&serde_json::to_vec(&unknown_type)?),
        Err(ProtocolError::JsonDecode { .. })
    ));

    let mut unknown_capability: Value = serde_json::from_slice(fixture)?;
    unknown_capability["message"]["payload"]["capabilities"][0] = json!("future_capability");
    assert!(matches!(
        decode_json(&serde_json::to_vec(&unknown_capability)?),
        Err(ProtocolError::JsonDecode { .. })
    ));
    Ok(())
}

#[test]
fn capability_duplicates_are_rejected_and_encoding_order_is_stable() -> TestResult {
    let fixture = include_bytes!("fixtures/v2/provider_descriptor.json");
    let message = decode_json(fixture)?;
    let encoded: Value = serde_json::from_slice(&encode_json(&message)?)?;
    assert_eq!(
        encoded["message"]["payload"]["capabilities"],
        json!([
            "session_state",
            "tool_events",
            "plan",
            "progress",
            "approval_request",
            "approval_response",
            "user_input_request",
            "user_input_response",
            "prompt_submit",
            "cancel",
            "control"
        ])
    );

    let mut duplicate: Value = serde_json::from_slice(fixture)?;
    duplicate["message"]["payload"]["capabilities"] = json!(["session_state", "session_state"]);
    assert!(matches!(
        decode_json(&serde_json::to_vec(&duplicate)?),
        Err(ProtocolError::InvalidWireValue { .. })
    ));
    Ok(())
}

#[test]
fn decimal_u64_values_are_strings_and_canonical() -> TestResult {
    let fixture = include_bytes!("fixtures/v2/agent_session.json");
    let message = decode_json(fixture)?;
    let encoded: Value = serde_json::from_slice(&encode_json(&message)?)?;
    assert_eq!(encoded["message"]["payload"]["revision"], json!("3"));

    for invalid in [
        json!(3),
        json!("03"),
        json!("+3"),
        json!(" 3"),
        json!("18446744073709551616"),
    ] {
        let mut value: Value = serde_json::from_slice(fixture)?;
        value["message"]["payload"]["revision"] = invalid;
        assert!(matches!(
            decode_json(&serde_json::to_vec(&value)?),
            Err(ProtocolError::JsonDecode { .. })
        ));
    }

    let mut zero: Value = serde_json::from_slice(fixture)?;
    zero["message"]["payload"]["revision"] = json!("0");
    assert!(matches!(
        decode_json(&serde_json::to_vec(&zero)?),
        Err(ProtocolError::Domain { .. })
    ));

    let mut maximum: Value = serde_json::from_slice(fixture)?;
    maximum["message"]["payload"]["revision"] = json!("18446744073709551615");
    let maximum_message = decode_json(&serde_json::to_vec(&maximum)?)?;
    let maximum_encoded: Value = serde_json::from_slice(&encode_json(&maximum_message)?)?;
    assert_eq!(
        maximum_encoded["message"]["payload"]["revision"],
        json!("18446744073709551615")
    );
    Ok(())
}

#[test]
fn optional_null_is_accepted_but_omitted_when_reencoded() -> TestResult {
    let fixture = include_bytes!("fixtures/v2/agent_session.json");
    let mut value: Value = serde_json::from_slice(fixture)?;
    value["message"]["payload"]["title"] = Value::Null;
    value["message"]["payload"]["external_id"] = Value::Null;
    let message = decode_json(&serde_json::to_vec(&value)?)?;
    let encoded: Value = serde_json::from_slice(&encode_json(&message)?)?;
    let payload = &encoded["message"]["payload"];
    assert!(payload.get("title").is_none());
    assert!(payload.get("external_id").is_none());
    Ok(())
}

#[test]
fn timestamps_normalize_to_utc_and_domain_invariants_survive_decoding() -> TestResult {
    let fixture = include_bytes!("fixtures/v2/agent_session.json");
    let mut offset: Value = serde_json::from_slice(fixture)?;
    offset["message"]["payload"]["created_at"] = json!("2026-08-29T08:00:00+08:00");
    offset["message"]["payload"]["updated_at"] = json!("2026-08-29T08:02:00+08:00");
    let message = decode_json(&serde_json::to_vec(&offset)?)?;
    let encoded: Value = serde_json::from_slice(&encode_json(&message)?)?;
    assert_eq!(
        encoded["message"]["payload"]["created_at"],
        json!("2026-08-29T00:00:00Z")
    );

    let mut wrong_id: Value = serde_json::from_slice(fixture)?;
    wrong_id["message"]["payload"]["id"] = json!("550e8400-e29b-41d4-a716-446655440000");
    assert!(matches!(
        decode_json(&serde_json::to_vec(&wrong_id)?),
        Err(ProtocolError::Domain { .. })
    ));

    let mut wrong_time: Value = serde_json::from_slice(fixture)?;
    wrong_time["message"]["payload"]["updated_at"] = json!("2026-08-28T23:59:59Z");
    assert!(matches!(
        decode_json(&serde_json::to_vec(&wrong_time)?),
        Err(ProtocolError::Domain { .. })
    ));

    let event_fixture = include_bytes!("fixtures/v2/agent_event.json");
    let mut mismatch: Value = serde_json::from_slice(event_fixture)?;
    mismatch["message"]["payload"]["payload"]["request"]["session_id"] =
        json!("01890f47-7c00-7000-8000-000000000099");
    assert!(matches!(
        decode_json(&serde_json::to_vec(&mismatch)?),
        Err(ProtocolError::Domain { .. })
    ));

    let mut duplicate_choice: Value = serde_json::from_slice(event_fixture)?;
    let first = duplicate_choice["message"]["payload"]["payload"]["request"]["payload"]["options"]
        [0]
    .clone();
    duplicate_choice["message"]["payload"]["payload"]["request"]["payload"]["options"] =
        json!([first.clone(), first]);
    assert!(matches!(
        decode_json(&serde_json::to_vec(&duplicate_choice)?),
        Err(ProtocolError::Domain { .. })
    ));
    Ok(())
}

#[test]
fn decoded_payloads_reapply_core_collection_and_value_invariants() -> TestResult {
    let event_id = "01890f47-7c00-7000-8000-000000000004";
    let session_id = "01890f47-7c00-7000-8000-000000000003";
    let interaction_id = "01890f47-7c00-7000-8000-000000000005";
    let option_id = "01890f47-7c00-7000-8000-000000000007";
    let plan_item_id = "01890f47-7c00-7000-8000-000000000009";

    let event = |payload: Value| {
        json!({
            "protocol_version": 2,
            "message": {
                "type": "agent_event",
                "payload": {
                    "id": event_id,
                    "session_id": session_id,
                    "sequence": "1",
                    "occurred_at": "2026-08-29T00:03:00Z",
                    "payload": payload
                }
            }
        })
    };

    let invalid_values = [
        event(json!({
            "type": "progress_updated",
            "progress": {
                "revision": "1",
                "value": { "type": "determinate", "completed": "3", "total": "2" }
            }
        })),
        event(json!({
            "type": "plan_updated",
            "plan": {
                "revision": "1",
                "items": [
                    { "id": plan_item_id, "content": "First", "status": "pending" },
                    { "id": plan_item_id, "content": "Duplicate", "status": "blocked" }
                ]
            }
        })),
        event(json!({
            "type": "interaction_requested",
            "request": {
                "id": interaction_id,
                "session_id": session_id,
                "requested_at": "2026-08-29T00:03:00Z",
                "expires_at": "2026-08-29T00:02:00Z",
                "prompt": "Approve?",
                "payload": {
                    "type": "approval",
                    "subject": {
                        "type": "command",
                        "kind": "command",
                        "command": "cargo test"
                    },
                    "options": [{
                        "id": option_id,
                        "disposition": "approve",
                        "label": "Approve"
                    }]
                }
            }
        })),
        event(json!({
            "type": "interaction_requested",
            "request": {
                "id": interaction_id,
                "session_id": session_id,
                "requested_at": "2026-08-29T00:03:00Z",
                "prompt": "Approve?",
                "payload": {
                    "type": "approval",
                    "subject": {
                        "type": "command",
                        "kind": "command",
                        "command": "cargo test"
                    },
                    "options": [
                        { "id": option_id, "disposition": "approve", "label": "Approve" },
                        { "id": option_id, "disposition": "reject", "label": "Reject" }
                    ]
                }
            }
        })),
        json!({
            "protocol_version": 2,
            "message": {
                "type": "interaction_response",
                "payload": {
                    "request_id": interaction_id,
                    "session_id": session_id,
                    "channel_id": "01890f47-7c00-7000-8000-000000000002",
                    "responded_at": "2026-08-29T00:04:00Z",
                    "payload": { "type": "choice", "option_ids": [option_id, option_id] }
                }
            }
        }),
        event(json!({
            "type": "message",
            "message": { "role": "assistant", "level": "info", "content": "   " }
        })),
    ];

    for invalid in invalid_values {
        assert!(matches!(
            decode_json(&serde_json::to_vec(&invalid)?),
            Err(ProtocolError::Domain { .. })
        ));
    }

    let invalid_timestamp = event(json!({
        "type": "state_changed",
        "state": "running"
    }));
    let mut invalid_timestamp = invalid_timestamp;
    invalid_timestamp["message"]["payload"]["occurred_at"] = json!("not-a-timestamp");
    assert!(matches!(
        decode_json(&serde_json::to_vec(&invalid_timestamp)?),
        Err(ProtocolError::InvalidWireValue { .. })
    ));
    Ok(())
}
