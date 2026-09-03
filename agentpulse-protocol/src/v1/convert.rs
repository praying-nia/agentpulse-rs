//! Explicit conversions between private JSON DTOs and validated Core values.

use std::{collections::HashSet, str::FromStr};

use agentpulse_core::{
    AgentCommand, AgentCommandPayload, AgentEvent, AgentEventPayload, AgentMessage,
    AgentMessageLevel, AgentSession, AgentState, ApprovalCommandKind, ApprovalDisposition,
    ApprovalFileChange, ApprovalFileChangeKind, ApprovalNetworkContext, ApprovalOption,
    ApprovalRequest, ApprovalSelection, ApprovalSubject, ChannelCapabilities, ChannelDescriptor,
    ChannelKind, ChoiceOption, ChoiceOptionId, ChoiceRequest, ChoiceSelection, ConnectionState,
    DeterminateProgress, DomainError, EventSequence, ExternalId, InteractionCloseReason,
    InteractionClosed, InteractionRequest, InteractionRequestPayload, InteractionResponse,
    InteractionResponsePayload, NonEmptyText, PlanItem, PlanItemStatus, PlanSnapshot,
    ProgressSnapshot, ProgressValue, ProviderCapabilities, ProviderDescriptor, ProviderKind,
    Revision, SessionOutcome, TextInputRequest, Timestamp, ToolActivity, ToolOutcome, WorkspaceRef,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{ProtocolError, ProtocolMessage, V1_PROTOCOL_VERSION};

use super::{dto::*, scalar::DecimalU64};

pub(crate) fn encode_envelope(message: &ProtocolMessage) -> Result<EnvelopeDto, ProtocolError> {
    let message = match message {
        ProtocolMessage::ProviderDescriptor(descriptor) => {
            MessageDto::ProviderDescriptor(encode_provider_descriptor(descriptor))
        }
        ProtocolMessage::ChannelDescriptor(descriptor) => {
            MessageDto::ChannelDescriptor(encode_channel_descriptor(descriptor))
        }
        ProtocolMessage::AgentSession(session) => {
            MessageDto::AgentSession(encode_session(session)?)
        }
        ProtocolMessage::AgentEvent(event) => MessageDto::AgentEvent(encode_event(event)?),
        ProtocolMessage::InteractionRequest(request) => {
            MessageDto::InteractionRequest(encode_interaction_request(request)?)
        }
        ProtocolMessage::InteractionResponse(response) => {
            MessageDto::InteractionResponse(encode_interaction_response(response)?)
        }
        ProtocolMessage::AgentCommand(command) => {
            MessageDto::AgentCommand(encode_command(command)?)
        }
    };

    Ok(EnvelopeDto {
        protocol_version: V1_PROTOCOL_VERSION,
        message,
    })
}

pub(crate) fn decode_envelope(envelope: EnvelopeDto) -> Result<ProtocolMessage, ProtocolError> {
    if envelope.protocol_version != V1_PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedProtocolVersion {
            received: u64::from(envelope.protocol_version),
            supported: V1_PROTOCOL_VERSION,
        });
    }

    match envelope.message {
        MessageDto::ProviderDescriptor(descriptor) => {
            decode_provider_descriptor(descriptor).map(ProtocolMessage::ProviderDescriptor)
        }
        MessageDto::ChannelDescriptor(descriptor) => {
            decode_channel_descriptor(descriptor).map(ProtocolMessage::ChannelDescriptor)
        }
        MessageDto::AgentSession(session) => {
            decode_session(session).map(ProtocolMessage::AgentSession)
        }
        MessageDto::AgentEvent(event) => decode_event(event).map(ProtocolMessage::AgentEvent),
        MessageDto::InteractionRequest(request) => {
            decode_interaction_request(request).map(ProtocolMessage::InteractionRequest)
        }
        MessageDto::InteractionResponse(response) => {
            decode_interaction_response(response).map(ProtocolMessage::InteractionResponse)
        }
        MessageDto::AgentCommand(command) => {
            decode_command(command).map(ProtocolMessage::AgentCommand)
        }
    }
}

fn encode_provider_descriptor(descriptor: &ProviderDescriptor) -> ProviderDescriptorDto {
    ProviderDescriptorDto {
        id: descriptor.id().to_string(),
        kind: descriptor.kind().as_str().to_owned(),
        display_name: descriptor.display_name().as_str().to_owned(),
        version: descriptor.version().map(ToString::to_string),
        capabilities: encode_provider_capabilities(descriptor.capabilities()),
    }
}

fn decode_provider_descriptor(
    dto: ProviderDescriptorDto,
) -> Result<ProviderDescriptor, ProtocolError> {
    let descriptor = ProviderDescriptor::new(
        parse_id(dto.id)?,
        ProviderKind::new(dto.kind)?,
        NonEmptyText::new(dto.display_name)?,
        decode_provider_capabilities(dto.capabilities)?,
    );
    match dto.version {
        Some(version) => Ok(descriptor.with_version(NonEmptyText::new(version)?)),
        None => Ok(descriptor),
    }
}

fn encode_channel_descriptor(descriptor: &ChannelDescriptor) -> ChannelDescriptorDto {
    ChannelDescriptorDto {
        id: descriptor.id().to_string(),
        kind: descriptor.kind().as_str().to_owned(),
        display_name: descriptor.display_name().as_str().to_owned(),
        version: descriptor.version().map(ToString::to_string),
        capabilities: encode_channel_capabilities(descriptor.capabilities()),
    }
}

fn decode_channel_descriptor(
    dto: ChannelDescriptorDto,
) -> Result<ChannelDescriptor, ProtocolError> {
    let descriptor = ChannelDescriptor::new(
        parse_id(dto.id)?,
        ChannelKind::new(dto.kind)?,
        NonEmptyText::new(dto.display_name)?,
        decode_channel_capabilities(dto.capabilities)?,
    );
    match dto.version {
        Some(version) => Ok(descriptor.with_version(NonEmptyText::new(version)?)),
        None => Ok(descriptor),
    }
}

fn encode_session(session: &AgentSession) -> Result<AgentSessionDto, ProtocolError> {
    Ok(AgentSessionDto {
        id: session.id().to_string(),
        provider_id: session.provider_id().to_string(),
        external_id: session.external_id().map(ToString::to_string),
        title: session.title().map(ToString::to_string),
        workspace: session.workspace().map(encode_workspace),
        state: encode_agent_state(session.state())?,
        connection_state: encode_connection_state(session.connection_state())?,
        revision: DecimalU64::new(session.revision().get()),
        created_at: encode_timestamp(session.created_at(), "session created_at")?,
        updated_at: encode_timestamp(session.updated_at(), "session updated_at")?,
    })
}

fn decode_session(dto: AgentSessionDto) -> Result<AgentSession, ProtocolError> {
    let created_at = decode_timestamp(dto.created_at, "session created_at")?;
    let updated_at = decode_timestamp(dto.updated_at, "session updated_at")?;
    let mut builder =
        AgentSession::builder(parse_id(dto.id)?, parse_id(dto.provider_id)?, created_at)
            .state(decode_agent_state(dto.state))
            .connection_state(decode_connection_state(dto.connection_state))
            .revision(Revision::new(dto.revision.get())?, updated_at);

    if let Some(external_id) = dto.external_id {
        builder = builder.external_id(ExternalId::new(external_id)?);
    }
    if let Some(title) = dto.title {
        builder = builder.title(NonEmptyText::new(title)?);
    }
    if let Some(workspace) = dto.workspace {
        builder = builder.workspace(decode_workspace(workspace)?);
    }
    builder.build().map_err(ProtocolError::from)
}

fn encode_workspace(workspace: &WorkspaceRef) -> WorkspaceRefDto {
    WorkspaceRefDto {
        path: workspace.path().as_str().to_owned(),
        display_name: workspace.display_name().map(ToString::to_string),
    }
}

fn decode_workspace(dto: WorkspaceRefDto) -> Result<WorkspaceRef, ProtocolError> {
    let workspace = WorkspaceRef::new(NonEmptyText::new(dto.path)?);
    match dto.display_name {
        Some(display_name) => Ok(workspace.with_display_name(NonEmptyText::new(display_name)?)),
        None => Ok(workspace),
    }
}

fn encode_event(event: &AgentEvent) -> Result<AgentEventDto, ProtocolError> {
    Ok(AgentEventDto {
        id: event.id().to_string(),
        session_id: event.session_id().to_string(),
        sequence: DecimalU64::new(event.sequence().get()),
        occurred_at: encode_timestamp(event.occurred_at(), "event occurred_at")?,
        payload: encode_event_payload(event.payload())?,
    })
}

fn decode_event(dto: AgentEventDto) -> Result<AgentEvent, ProtocolError> {
    AgentEvent::new(
        parse_id(dto.id)?,
        parse_id(dto.session_id)?,
        EventSequence::new(dto.sequence.get())?,
        decode_timestamp(dto.occurred_at, "event occurred_at")?,
        decode_event_payload(dto.payload)?,
    )
    .map_err(ProtocolError::from)
}

fn encode_event_payload(
    payload: &AgentEventPayload,
) -> Result<AgentEventPayloadDto, ProtocolError> {
    match payload {
        AgentEventPayload::SessionStarted(session) => Ok(AgentEventPayloadDto::SessionStarted {
            session: encode_session(session)?,
        }),
        AgentEventPayload::StateChanged(state) => Ok(AgentEventPayloadDto::StateChanged {
            state: encode_agent_state(*state)?,
        }),
        AgentEventPayload::ConnectionChanged(connection_state) => {
            Ok(AgentEventPayloadDto::ConnectionChanged {
                connection_state: encode_connection_state(*connection_state)?,
            })
        }
        AgentEventPayload::Message(message) => Ok(AgentEventPayloadDto::Message {
            message: encode_message(message)?,
        }),
        AgentEventPayload::ToolActivity(activity) => Ok(AgentEventPayloadDto::ToolActivity {
            activity: encode_tool_activity(activity)?,
        }),
        AgentEventPayload::PlanUpdated(plan) => Ok(AgentEventPayloadDto::PlanUpdated {
            plan: encode_plan(plan)?,
        }),
        AgentEventPayload::ProgressUpdated(progress) => Ok(AgentEventPayloadDto::ProgressUpdated {
            progress: encode_progress(progress)?,
        }),
        AgentEventPayload::InteractionRequested(request) => {
            Ok(AgentEventPayloadDto::InteractionRequested {
                request: encode_interaction_request(request)?,
            })
        }
        AgentEventPayload::InteractionResponded(response) => {
            Ok(AgentEventPayloadDto::InteractionResponded {
                response: encode_interaction_response(response)?,
            })
        }
        AgentEventPayload::InteractionClosed(interaction) => {
            Ok(AgentEventPayloadDto::InteractionClosed {
                interaction: encode_interaction_closed(interaction)?,
            })
        }
        AgentEventPayload::CommandIssued(command) => Ok(AgentEventPayloadDto::CommandIssued {
            command: encode_command(command)?,
        }),
        AgentEventPayload::SessionEnded(outcome) => Ok(AgentEventPayloadDto::SessionEnded {
            outcome: encode_session_outcome(outcome)?,
        }),
        _ => Err(unsupported("AgentEventPayload")),
    }
}

fn decode_event_payload(dto: AgentEventPayloadDto) -> Result<AgentEventPayload, ProtocolError> {
    match dto {
        AgentEventPayloadDto::SessionStarted { session } => {
            decode_session(session).map(AgentEventPayload::SessionStarted)
        }
        AgentEventPayloadDto::StateChanged { state } => {
            Ok(AgentEventPayload::StateChanged(decode_agent_state(state)))
        }
        AgentEventPayloadDto::ConnectionChanged { connection_state } => Ok(
            AgentEventPayload::ConnectionChanged(decode_connection_state(connection_state)),
        ),
        AgentEventPayloadDto::Message { message } => {
            decode_message(message).map(AgentEventPayload::Message)
        }
        AgentEventPayloadDto::ToolActivity { activity } => {
            decode_tool_activity(activity).map(AgentEventPayload::ToolActivity)
        }
        AgentEventPayloadDto::PlanUpdated { plan } => {
            decode_plan(plan).map(AgentEventPayload::PlanUpdated)
        }
        AgentEventPayloadDto::ProgressUpdated { progress } => {
            decode_progress(progress).map(AgentEventPayload::ProgressUpdated)
        }
        AgentEventPayloadDto::InteractionRequested { request } => {
            decode_interaction_request(request).map(AgentEventPayload::InteractionRequested)
        }
        AgentEventPayloadDto::InteractionResponded { response } => {
            decode_interaction_response(response).map(AgentEventPayload::InteractionResponded)
        }
        AgentEventPayloadDto::InteractionClosed { interaction } => {
            decode_interaction_closed(interaction).map(AgentEventPayload::InteractionClosed)
        }
        AgentEventPayloadDto::CommandIssued { command } => {
            decode_command(command).map(AgentEventPayload::CommandIssued)
        }
        AgentEventPayloadDto::SessionEnded { outcome } => {
            decode_session_outcome(outcome).map(AgentEventPayload::SessionEnded)
        }
    }
}

fn encode_message(message: &AgentMessage) -> Result<AgentMessageDto, ProtocolError> {
    Ok(AgentMessageDto {
        level: encode_message_level(message.level())?,
        content: message.content().as_str().to_owned(),
    })
}

fn decode_message(dto: AgentMessageDto) -> Result<AgentMessage, ProtocolError> {
    Ok(AgentMessage::new(
        decode_message_level(dto.level),
        NonEmptyText::new(dto.content)?,
    ))
}

fn encode_tool_activity(activity: &ToolActivity) -> Result<ToolActivityDto, ProtocolError> {
    match activity {
        ToolActivity::Started {
            call_id,
            name,
            summary,
        } => Ok(ToolActivityDto::Started {
            call_id: call_id.to_string(),
            name: name.as_str().to_owned(),
            summary: summary.as_ref().map(ToString::to_string),
        }),
        ToolActivity::Finished {
            call_id,
            outcome,
            summary,
        } => Ok(ToolActivityDto::Finished {
            call_id: call_id.to_string(),
            outcome: encode_tool_outcome(*outcome)?,
            summary: summary.as_ref().map(ToString::to_string),
        }),
        _ => Err(unsupported("ToolActivity")),
    }
}

fn decode_tool_activity(dto: ToolActivityDto) -> Result<ToolActivity, ProtocolError> {
    match dto {
        ToolActivityDto::Started {
            call_id,
            name,
            summary,
        } => Ok(ToolActivity::Started {
            call_id: parse_id(call_id)?,
            name: NonEmptyText::new(name)?,
            summary: decode_optional_text(summary)?,
        }),
        ToolActivityDto::Finished {
            call_id,
            outcome,
            summary,
        } => Ok(ToolActivity::Finished {
            call_id: parse_id(call_id)?,
            outcome: decode_tool_outcome(outcome),
            summary: decode_optional_text(summary)?,
        }),
    }
}

fn encode_session_outcome(outcome: &SessionOutcome) -> Result<SessionOutcomeDto, ProtocolError> {
    match outcome {
        SessionOutcome::Completed { summary } => Ok(SessionOutcomeDto::Completed {
            summary: summary.as_ref().map(ToString::to_string),
        }),
        SessionOutcome::Failed { error } => Ok(SessionOutcomeDto::Failed {
            error: error.as_str().to_owned(),
        }),
        SessionOutcome::Cancelled { reason } => Ok(SessionOutcomeDto::Cancelled {
            reason: reason.as_ref().map(ToString::to_string),
        }),
        _ => Err(unsupported("SessionOutcome")),
    }
}

fn decode_session_outcome(dto: SessionOutcomeDto) -> Result<SessionOutcome, ProtocolError> {
    match dto {
        SessionOutcomeDto::Completed { summary } => Ok(SessionOutcome::Completed {
            summary: decode_optional_text(summary)?,
        }),
        SessionOutcomeDto::Failed { error } => Ok(SessionOutcome::Failed {
            error: NonEmptyText::new(error)?,
        }),
        SessionOutcomeDto::Cancelled { reason } => Ok(SessionOutcome::Cancelled {
            reason: decode_optional_text(reason)?,
        }),
    }
}

fn encode_plan(plan: &PlanSnapshot) -> Result<PlanSnapshotDto, ProtocolError> {
    Ok(PlanSnapshotDto {
        revision: DecimalU64::new(plan.revision().get()),
        explanation: plan.explanation().map(ToString::to_string),
        items: plan
            .items()
            .iter()
            .map(encode_plan_item)
            .collect::<Result<_, _>>()?,
    })
}

fn decode_plan(dto: PlanSnapshotDto) -> Result<PlanSnapshot, ProtocolError> {
    let items = dto
        .items
        .into_iter()
        .map(decode_plan_item)
        .collect::<Result<Vec<_>, _>>()?;
    let plan = PlanSnapshot::new(Revision::new(dto.revision.get())?, items)?;
    match dto.explanation {
        Some(explanation) => Ok(plan.with_explanation(NonEmptyText::new(explanation)?)),
        None => Ok(plan),
    }
}

fn encode_plan_item(item: &PlanItem) -> Result<PlanItemDto, ProtocolError> {
    Ok(PlanItemDto {
        id: item.id().to_string(),
        content: item.content().as_str().to_owned(),
        status: encode_plan_item_status(item.status())?,
    })
}

fn decode_plan_item(dto: PlanItemDto) -> Result<PlanItem, ProtocolError> {
    Ok(PlanItem::new(
        parse_id(dto.id)?,
        NonEmptyText::new(dto.content)?,
        decode_plan_item_status(dto.status),
    ))
}

fn encode_progress(progress: &ProgressSnapshot) -> Result<ProgressSnapshotDto, ProtocolError> {
    Ok(ProgressSnapshotDto {
        revision: DecimalU64::new(progress.revision().get()),
        value: encode_progress_value(progress.value())?,
        message: progress.message().map(ToString::to_string),
    })
}

fn decode_progress(dto: ProgressSnapshotDto) -> Result<ProgressSnapshot, ProtocolError> {
    let progress = ProgressSnapshot::new(
        Revision::new(dto.revision.get())?,
        decode_progress_value(dto.value)?,
    );
    match dto.message {
        Some(message) => Ok(progress.with_message(NonEmptyText::new(message)?)),
        None => Ok(progress),
    }
}

fn encode_progress_value(value: ProgressValue) -> Result<ProgressValueDto, ProtocolError> {
    match value {
        ProgressValue::Indeterminate => Ok(ProgressValueDto::Indeterminate),
        ProgressValue::Determinate(progress) => Ok(ProgressValueDto::Determinate {
            completed: DecimalU64::new(progress.completed()),
            total: DecimalU64::new(progress.total()),
        }),
        _ => Err(unsupported("ProgressValue")),
    }
}

fn decode_progress_value(dto: ProgressValueDto) -> Result<ProgressValue, ProtocolError> {
    match dto {
        ProgressValueDto::Indeterminate => Ok(ProgressValue::Indeterminate),
        ProgressValueDto::Determinate { completed, total } => {
            DeterminateProgress::new(completed.get(), total.get())
                .map(ProgressValue::Determinate)
                .map_err(ProtocolError::from)
        }
    }
}

fn encode_interaction_request(
    request: &InteractionRequest,
) -> Result<InteractionRequestDto, ProtocolError> {
    Ok(InteractionRequestDto {
        id: request.id().to_string(),
        session_id: request.session_id().to_string(),
        requested_at: encode_timestamp(request.requested_at(), "interaction requested_at")?,
        expires_at: request
            .expires_at()
            .map(|value| encode_timestamp(value, "interaction expires_at"))
            .transpose()?,
        prompt: request.prompt().as_str().to_owned(),
        payload: encode_interaction_request_payload(request.payload())?,
    })
}

fn decode_interaction_request(
    dto: InteractionRequestDto,
) -> Result<InteractionRequest, ProtocolError> {
    let request = InteractionRequest::new(
        parse_id(dto.id)?,
        parse_id(dto.session_id)?,
        decode_timestamp(dto.requested_at, "interaction requested_at")?,
        NonEmptyText::new(dto.prompt)?,
        decode_interaction_request_payload(dto.payload)?,
    );
    match dto.expires_at {
        Some(expires_at) => request
            .with_expiration(decode_timestamp(expires_at, "interaction expires_at")?)
            .map_err(ProtocolError::from),
        None => Ok(request),
    }
}

fn encode_interaction_request_payload(
    payload: &InteractionRequestPayload,
) -> Result<InteractionRequestPayloadDto, ProtocolError> {
    match payload {
        InteractionRequestPayload::Approval(request) => {
            Ok(InteractionRequestPayloadDto::Approval {
                subject: encode_approval_subject(request.subject())?,
                options: request
                    .options()
                    .iter()
                    .map(encode_approval_option)
                    .collect::<Result<_, _>>()?,
                unavailable_reason: request.unavailable_reason().map(ToString::to_string),
            })
        }
        InteractionRequestPayload::Choice(request) => {
            let options = request
                .options()
                .iter()
                .map(encode_choice_option)
                .collect::<Result<_, _>>()?;
            Ok(InteractionRequestPayloadDto::Choice {
                options,
                multiple: request.allows_multiple(),
            })
        }
        InteractionRequestPayload::Text(request) => Ok(InteractionRequestPayloadDto::Text {
            placeholder: request.placeholder().map(ToString::to_string),
            multiline: request.multiline(),
        }),
        _ => Err(unsupported("InteractionRequestPayload")),
    }
}

fn decode_interaction_request_payload(
    dto: InteractionRequestPayloadDto,
) -> Result<InteractionRequestPayload, ProtocolError> {
    match dto {
        InteractionRequestPayloadDto::Approval {
            subject,
            options,
            unavailable_reason,
        } => {
            let subject = decode_approval_subject(subject)?;
            let options = options
                .into_iter()
                .map(decode_approval_option)
                .collect::<Result<Vec<_>, _>>()?;
            let request = match (options.is_empty(), unavailable_reason) {
                (false, None) => ApprovalRequest::actionable(subject, options)?,
                (true, Some(reason)) => {
                    ApprovalRequest::unavailable(subject, NonEmptyText::new(reason)?)
                }
                (true, None) => {
                    return Err(ProtocolError::InvalidWireValue {
                        field: "approval",
                        reason: "an approval without options requires unavailable_reason"
                            .to_owned(),
                    });
                }
                (false, Some(_)) => {
                    return Err(ProtocolError::InvalidWireValue {
                        field: "approval",
                        reason: "an actionable approval cannot carry unavailable_reason".to_owned(),
                    });
                }
            };
            Ok(InteractionRequestPayload::Approval(request))
        }
        InteractionRequestPayloadDto::Choice { options, multiple } => {
            let options = options
                .into_iter()
                .map(decode_choice_option)
                .collect::<Result<Vec<_>, _>>()?;
            ChoiceRequest::new(options, multiple)
                .map(InteractionRequestPayload::Choice)
                .map_err(ProtocolError::from)
        }
        InteractionRequestPayloadDto::Text {
            placeholder,
            multiline,
        } => {
            let request = TextInputRequest::new(multiline);
            match placeholder {
                Some(placeholder) => Ok(InteractionRequestPayload::Text(
                    request.with_placeholder(NonEmptyText::new(placeholder)?),
                )),
                None => Ok(InteractionRequestPayload::Text(request)),
            }
        }
    }
}

fn encode_choice_option(option: &ChoiceOption) -> Result<ChoiceOptionDto, ProtocolError> {
    Ok(ChoiceOptionDto {
        id: option.id().to_string(),
        label: option.label().as_str().to_owned(),
        description: option.description().map(ToString::to_string),
    })
}

fn decode_choice_option(dto: ChoiceOptionDto) -> Result<ChoiceOption, ProtocolError> {
    let option = ChoiceOption::new(parse_id(dto.id)?, NonEmptyText::new(dto.label)?);
    match dto.description {
        Some(description) => Ok(option.with_description(NonEmptyText::new(description)?)),
        None => Ok(option),
    }
}

fn encode_approval_subject(subject: &ApprovalSubject) -> Result<ApprovalSubjectDto, ProtocolError> {
    match subject {
        ApprovalSubject::Command {
            kind,
            command,
            cwd,
            reason,
            network,
        } => Ok(ApprovalSubjectDto::Command {
            kind: encode_approval_command_kind(*kind)?,
            command: command.as_ref().map(ToString::to_string),
            cwd: cwd.as_ref().map(ToString::to_string),
            reason: reason.as_ref().map(ToString::to_string),
            network: network.as_ref().map(|context| ApprovalNetworkContextDto {
                host: context.host().to_string(),
                protocol: context.protocol().to_string(),
            }),
        }),
        ApprovalSubject::FileChange {
            changes,
            grant_root,
            reason,
        } => Ok(ApprovalSubjectDto::FileChange {
            changes: changes
                .iter()
                .map(|change| {
                    Ok(ApprovalFileChangeDto {
                        path: change.path().to_string(),
                        kind: encode_approval_file_change_kind(change.kind())?,
                        diff: change.diff().to_owned(),
                    })
                })
                .collect::<Result<_, ProtocolError>>()?,
            grant_root: grant_root.as_ref().map(ToString::to_string),
            reason: reason.as_ref().map(ToString::to_string),
        }),
        _ => Err(unsupported("ApprovalSubject")),
    }
}

fn decode_approval_subject(dto: ApprovalSubjectDto) -> Result<ApprovalSubject, ProtocolError> {
    match dto {
        ApprovalSubjectDto::Command {
            kind,
            command,
            cwd,
            reason,
            network,
        } => Ok(ApprovalSubject::Command {
            kind: decode_approval_command_kind(kind),
            command: decode_optional_text(command)?,
            cwd: decode_optional_text(cwd)?,
            reason: decode_optional_text(reason)?,
            network: network
                .map(|context| {
                    Ok::<ApprovalNetworkContext, ProtocolError>(ApprovalNetworkContext::new(
                        NonEmptyText::new(context.host)?,
                        NonEmptyText::new(context.protocol)?,
                    ))
                })
                .transpose()?,
        }),
        ApprovalSubjectDto::FileChange {
            changes,
            grant_root,
            reason,
        } => Ok(ApprovalSubject::FileChange {
            changes: changes
                .into_iter()
                .map(|change| {
                    Ok(ApprovalFileChange::new(
                        NonEmptyText::new(change.path)?,
                        decode_approval_file_change_kind(change.kind),
                        change.diff,
                    ))
                })
                .collect::<Result<_, ProtocolError>>()?,
            grant_root: decode_optional_text(grant_root)?,
            reason: decode_optional_text(reason)?,
        }),
    }
}

fn encode_approval_option(option: &ApprovalOption) -> Result<ApprovalOptionDto, ProtocolError> {
    Ok(ApprovalOptionDto {
        id: option.id().to_string(),
        disposition: encode_approval_disposition(option.disposition())?,
        label: option.label().to_string(),
        description: option.description().map(ToString::to_string),
    })
}

fn decode_approval_option(dto: ApprovalOptionDto) -> Result<ApprovalOption, ProtocolError> {
    let option = ApprovalOption::new(
        parse_id(dto.id)?,
        decode_approval_disposition(dto.disposition),
        NonEmptyText::new(dto.label)?,
    );
    match dto.description {
        Some(description) => Ok(option.with_description(NonEmptyText::new(description)?)),
        None => Ok(option),
    }
}

fn encode_interaction_response(
    response: &InteractionResponse,
) -> Result<InteractionResponseDto, ProtocolError> {
    Ok(InteractionResponseDto {
        request_id: response.request_id().to_string(),
        session_id: response.session_id().to_string(),
        channel_id: response.channel_id().to_string(),
        responded_at: encode_timestamp(response.responded_at(), "interaction responded_at")?,
        payload: encode_interaction_response_payload(response.payload())?,
    })
}

fn decode_interaction_response(
    dto: InteractionResponseDto,
) -> Result<InteractionResponse, ProtocolError> {
    Ok(InteractionResponse::new(
        parse_id(dto.request_id)?,
        parse_id(dto.session_id)?,
        parse_id(dto.channel_id)?,
        decode_timestamp(dto.responded_at, "interaction responded_at")?,
        decode_interaction_response_payload(dto.payload)?,
    ))
}

fn encode_interaction_response_payload(
    payload: &InteractionResponsePayload,
) -> Result<InteractionResponsePayloadDto, ProtocolError> {
    match payload {
        InteractionResponsePayload::Approval(selection) => {
            Ok(InteractionResponsePayloadDto::Approval {
                option_id: selection.option_id().to_string(),
            })
        }
        InteractionResponsePayload::Choice(selection) => {
            Ok(InteractionResponsePayloadDto::Choice {
                option_ids: selection
                    .option_ids()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })
        }
        InteractionResponsePayload::Text(text) => Ok(InteractionResponsePayloadDto::Text {
            text: text.as_str().to_owned(),
        }),
        _ => Err(unsupported("InteractionResponsePayload")),
    }
}

fn decode_interaction_response_payload(
    dto: InteractionResponsePayloadDto,
) -> Result<InteractionResponsePayload, ProtocolError> {
    match dto {
        InteractionResponsePayloadDto::Approval { option_id } => Ok(
            InteractionResponsePayload::Approval(ApprovalSelection::new(parse_id(option_id)?)),
        ),
        InteractionResponsePayloadDto::Choice { option_ids } => option_ids
            .into_iter()
            .map(parse_id::<ChoiceOptionId>)
            .collect::<Result<Vec<_>, _>>()
            .and_then(|ids| ChoiceSelection::new(ids).map_err(ProtocolError::from))
            .map(InteractionResponsePayload::Choice),
        InteractionResponsePayloadDto::Text { text } => NonEmptyText::new(text)
            .map(InteractionResponsePayload::Text)
            .map_err(ProtocolError::from),
    }
}

fn encode_interaction_closed(
    interaction: &InteractionClosed,
) -> Result<InteractionClosedDto, ProtocolError> {
    Ok(InteractionClosedDto {
        request_id: interaction.request_id().to_string(),
        session_id: interaction.session_id().to_string(),
        reason: match interaction.reason() {
            InteractionCloseReason::ResolvedElsewhere => {
                InteractionCloseReasonDto::ResolvedElsewhere
            }
            InteractionCloseReason::ProviderCancelled => {
                InteractionCloseReasonDto::ProviderCancelled
            }
            _ => return Err(unsupported("InteractionCloseReason")),
        },
    })
}

fn decode_interaction_closed(
    dto: InteractionClosedDto,
) -> Result<InteractionClosed, ProtocolError> {
    Ok(InteractionClosed::new(
        parse_id(dto.request_id)?,
        parse_id(dto.session_id)?,
        match dto.reason {
            InteractionCloseReasonDto::ResolvedElsewhere => {
                InteractionCloseReason::ResolvedElsewhere
            }
            InteractionCloseReasonDto::ProviderCancelled => {
                InteractionCloseReason::ProviderCancelled
            }
        },
    ))
}

fn encode_command(command: &AgentCommand) -> Result<AgentCommandDto, ProtocolError> {
    Ok(AgentCommandDto {
        id: command.id().to_string(),
        session_id: command.session_id().to_string(),
        channel_id: command.channel_id().to_string(),
        issued_at: encode_timestamp(command.issued_at(), "command issued_at")?,
        payload: encode_command_payload(command.payload())?,
    })
}

fn decode_command(dto: AgentCommandDto) -> Result<AgentCommand, ProtocolError> {
    Ok(AgentCommand::new(
        parse_id(dto.id)?,
        parse_id(dto.session_id)?,
        parse_id(dto.channel_id)?,
        decode_timestamp(dto.issued_at, "command issued_at")?,
        decode_command_payload(dto.payload)?,
    ))
}

fn encode_command_payload(
    payload: &AgentCommandPayload,
) -> Result<AgentCommandPayloadDto, ProtocolError> {
    match payload {
        AgentCommandPayload::SubmitPrompt { text } => Ok(AgentCommandPayloadDto::SubmitPrompt {
            text: text.as_str().to_owned(),
        }),
        AgentCommandPayload::CancelSession { reason } => {
            Ok(AgentCommandPayloadDto::CancelSession {
                reason: reason.as_ref().map(ToString::to_string),
            })
        }
        _ => Err(unsupported("AgentCommandPayload")),
    }
}

fn decode_command_payload(
    dto: AgentCommandPayloadDto,
) -> Result<AgentCommandPayload, ProtocolError> {
    match dto {
        AgentCommandPayloadDto::SubmitPrompt { text } => Ok(AgentCommandPayload::SubmitPrompt {
            text: NonEmptyText::new(text)?,
        }),
        AgentCommandPayloadDto::CancelSession { reason } => {
            Ok(AgentCommandPayload::CancelSession {
                reason: decode_optional_text(reason)?,
            })
        }
    }
}

fn encode_provider_capabilities(value: ProviderCapabilities) -> Vec<ProviderCapabilityDto> {
    let known = [
        (
            ProviderCapabilities::SESSION_STATE,
            ProviderCapabilityDto::SessionState,
        ),
        (
            ProviderCapabilities::TOOL_EVENTS,
            ProviderCapabilityDto::ToolEvents,
        ),
        (ProviderCapabilities::PLAN, ProviderCapabilityDto::Plan),
        (
            ProviderCapabilities::PROGRESS,
            ProviderCapabilityDto::Progress,
        ),
        (
            ProviderCapabilities::APPROVAL_REQUEST,
            ProviderCapabilityDto::ApprovalRequest,
        ),
        (
            ProviderCapabilities::APPROVAL_RESPONSE,
            ProviderCapabilityDto::ApprovalResponse,
        ),
        (
            ProviderCapabilities::USER_INPUT_REQUEST,
            ProviderCapabilityDto::UserInputRequest,
        ),
        (
            ProviderCapabilities::USER_INPUT_RESPONSE,
            ProviderCapabilityDto::UserInputResponse,
        ),
        (
            ProviderCapabilities::PROMPT_SUBMIT,
            ProviderCapabilityDto::PromptSubmit,
        ),
        (ProviderCapabilities::CANCEL, ProviderCapabilityDto::Cancel),
    ];
    known
        .into_iter()
        .filter_map(|(capability, dto)| value.contains(capability).then_some(dto))
        .collect()
}

fn decode_provider_capabilities(
    values: Vec<ProviderCapabilityDto>,
) -> Result<ProviderCapabilities, ProtocolError> {
    let mut seen = HashSet::with_capacity(values.len());
    let mut capabilities = ProviderCapabilities::NONE;
    for value in values {
        if !seen.insert(value) {
            return Err(duplicate_capability(
                "provider capabilities",
                provider_capability_name(value),
            ));
        }
        capabilities |= match value {
            ProviderCapabilityDto::SessionState => ProviderCapabilities::SESSION_STATE,
            ProviderCapabilityDto::ToolEvents => ProviderCapabilities::TOOL_EVENTS,
            ProviderCapabilityDto::Plan => ProviderCapabilities::PLAN,
            ProviderCapabilityDto::Progress => ProviderCapabilities::PROGRESS,
            ProviderCapabilityDto::ApprovalRequest => ProviderCapabilities::APPROVAL_REQUEST,
            ProviderCapabilityDto::ApprovalResponse => ProviderCapabilities::APPROVAL_RESPONSE,
            ProviderCapabilityDto::UserInputRequest => ProviderCapabilities::USER_INPUT_REQUEST,
            ProviderCapabilityDto::UserInputResponse => ProviderCapabilities::USER_INPUT_RESPONSE,
            ProviderCapabilityDto::PromptSubmit => ProviderCapabilities::PROMPT_SUBMIT,
            ProviderCapabilityDto::Cancel => ProviderCapabilities::CANCEL,
        };
    }
    Ok(capabilities)
}

fn encode_channel_capabilities(value: ChannelCapabilities) -> Vec<ChannelCapabilityDto> {
    let known = [
        (
            ChannelCapabilities::NOTIFICATION,
            ChannelCapabilityDto::Notification,
        ),
        (
            ChannelCapabilities::SESSION_VIEW,
            ChannelCapabilityDto::SessionView,
        ),
        (
            ChannelCapabilities::TOOL_VIEW,
            ChannelCapabilityDto::ToolView,
        ),
        (
            ChannelCapabilities::PLAN_VIEW,
            ChannelCapabilityDto::PlanView,
        ),
        (
            ChannelCapabilities::PROGRESS_VIEW,
            ChannelCapabilityDto::ProgressView,
        ),
        (
            ChannelCapabilities::RICH_MESSAGE,
            ChannelCapabilityDto::RichMessage,
        ),
        (
            ChannelCapabilities::APPROVAL,
            ChannelCapabilityDto::Approval,
        ),
        (
            ChannelCapabilities::CHOICE_INPUT,
            ChannelCapabilityDto::ChoiceInput,
        ),
        (
            ChannelCapabilities::TEXT_INPUT,
            ChannelCapabilityDto::TextInput,
        ),
        (
            ChannelCapabilities::FORM_INPUT,
            ChannelCapabilityDto::FormInput,
        ),
        (
            ChannelCapabilities::REALTIME_SYNC,
            ChannelCapabilityDto::RealtimeSync,
        ),
        (
            ChannelCapabilities::REMOTE_COMMAND,
            ChannelCapabilityDto::RemoteCommand,
        ),
    ];
    known
        .into_iter()
        .filter_map(|(capability, dto)| value.contains(capability).then_some(dto))
        .collect()
}

fn decode_channel_capabilities(
    values: Vec<ChannelCapabilityDto>,
) -> Result<ChannelCapabilities, ProtocolError> {
    let mut seen = HashSet::with_capacity(values.len());
    let mut capabilities = ChannelCapabilities::NONE;
    for value in values {
        if !seen.insert(value) {
            return Err(duplicate_capability(
                "channel capabilities",
                channel_capability_name(value),
            ));
        }
        capabilities |= match value {
            ChannelCapabilityDto::Notification => ChannelCapabilities::NOTIFICATION,
            ChannelCapabilityDto::SessionView => ChannelCapabilities::SESSION_VIEW,
            ChannelCapabilityDto::ToolView => ChannelCapabilities::TOOL_VIEW,
            ChannelCapabilityDto::PlanView => ChannelCapabilities::PLAN_VIEW,
            ChannelCapabilityDto::ProgressView => ChannelCapabilities::PROGRESS_VIEW,
            ChannelCapabilityDto::RichMessage => ChannelCapabilities::RICH_MESSAGE,
            ChannelCapabilityDto::Approval => ChannelCapabilities::APPROVAL,
            ChannelCapabilityDto::ChoiceInput => ChannelCapabilities::CHOICE_INPUT,
            ChannelCapabilityDto::TextInput => ChannelCapabilities::TEXT_INPUT,
            ChannelCapabilityDto::FormInput => ChannelCapabilities::FORM_INPUT,
            ChannelCapabilityDto::RealtimeSync => ChannelCapabilities::REALTIME_SYNC,
            ChannelCapabilityDto::RemoteCommand => ChannelCapabilities::REMOTE_COMMAND,
        };
    }
    Ok(capabilities)
}

fn encode_agent_state(value: AgentState) -> Result<AgentStateDto, ProtocolError> {
    match value {
        AgentState::Initializing => Ok(AgentStateDto::Initializing),
        AgentState::Idle => Ok(AgentStateDto::Idle),
        AgentState::Running => Ok(AgentStateDto::Running),
        AgentState::WaitingForInteraction => Ok(AgentStateDto::WaitingForInteraction),
        AgentState::Completed => Ok(AgentStateDto::Completed),
        AgentState::Failed => Ok(AgentStateDto::Failed),
        AgentState::Cancelled => Ok(AgentStateDto::Cancelled),
        _ => Err(unsupported("AgentState")),
    }
}

const fn decode_agent_state(value: AgentStateDto) -> AgentState {
    match value {
        AgentStateDto::Initializing => AgentState::Initializing,
        AgentStateDto::Idle => AgentState::Idle,
        AgentStateDto::Running => AgentState::Running,
        AgentStateDto::WaitingForInteraction => AgentState::WaitingForInteraction,
        AgentStateDto::Completed => AgentState::Completed,
        AgentStateDto::Failed => AgentState::Failed,
        AgentStateDto::Cancelled => AgentState::Cancelled,
    }
}

fn encode_connection_state(value: ConnectionState) -> Result<ConnectionStateDto, ProtocolError> {
    match value {
        ConnectionState::Connected => Ok(ConnectionStateDto::Connected),
        ConnectionState::Reconnecting => Ok(ConnectionStateDto::Reconnecting),
        ConnectionState::Disconnected => Ok(ConnectionStateDto::Disconnected),
        _ => Err(unsupported("ConnectionState")),
    }
}

const fn decode_connection_state(value: ConnectionStateDto) -> ConnectionState {
    match value {
        ConnectionStateDto::Connected => ConnectionState::Connected,
        ConnectionStateDto::Reconnecting => ConnectionState::Reconnecting,
        ConnectionStateDto::Disconnected => ConnectionState::Disconnected,
    }
}

fn encode_message_level(value: AgentMessageLevel) -> Result<AgentMessageLevelDto, ProtocolError> {
    match value {
        AgentMessageLevel::Info => Ok(AgentMessageLevelDto::Info),
        AgentMessageLevel::Warning => Ok(AgentMessageLevelDto::Warning),
        AgentMessageLevel::Error => Ok(AgentMessageLevelDto::Error),
        _ => Err(unsupported("AgentMessageLevel")),
    }
}

const fn decode_message_level(value: AgentMessageLevelDto) -> AgentMessageLevel {
    match value {
        AgentMessageLevelDto::Info => AgentMessageLevel::Info,
        AgentMessageLevelDto::Warning => AgentMessageLevel::Warning,
        AgentMessageLevelDto::Error => AgentMessageLevel::Error,
    }
}

fn encode_tool_outcome(value: ToolOutcome) -> Result<ToolOutcomeDto, ProtocolError> {
    match value {
        ToolOutcome::Succeeded => Ok(ToolOutcomeDto::Succeeded),
        ToolOutcome::Failed => Ok(ToolOutcomeDto::Failed),
        ToolOutcome::Cancelled => Ok(ToolOutcomeDto::Cancelled),
        _ => Err(unsupported("ToolOutcome")),
    }
}

const fn decode_tool_outcome(value: ToolOutcomeDto) -> ToolOutcome {
    match value {
        ToolOutcomeDto::Succeeded => ToolOutcome::Succeeded,
        ToolOutcomeDto::Failed => ToolOutcome::Failed,
        ToolOutcomeDto::Cancelled => ToolOutcome::Cancelled,
    }
}

fn encode_plan_item_status(value: PlanItemStatus) -> Result<PlanItemStatusDto, ProtocolError> {
    match value {
        PlanItemStatus::Pending => Ok(PlanItemStatusDto::Pending),
        PlanItemStatus::InProgress => Ok(PlanItemStatusDto::InProgress),
        PlanItemStatus::Completed => Ok(PlanItemStatusDto::Completed),
        PlanItemStatus::Blocked => Ok(PlanItemStatusDto::Blocked),
        PlanItemStatus::Skipped => Ok(PlanItemStatusDto::Skipped),
        _ => Err(unsupported("PlanItemStatus")),
    }
}

const fn decode_plan_item_status(value: PlanItemStatusDto) -> PlanItemStatus {
    match value {
        PlanItemStatusDto::Pending => PlanItemStatus::Pending,
        PlanItemStatusDto::InProgress => PlanItemStatus::InProgress,
        PlanItemStatusDto::Completed => PlanItemStatus::Completed,
        PlanItemStatusDto::Blocked => PlanItemStatus::Blocked,
        PlanItemStatusDto::Skipped => PlanItemStatus::Skipped,
    }
}

fn encode_approval_command_kind(
    value: ApprovalCommandKind,
) -> Result<ApprovalCommandKindDto, ProtocolError> {
    match value {
        ApprovalCommandKind::Command => Ok(ApprovalCommandKindDto::Command),
        ApprovalCommandKind::WriteStdin => Ok(ApprovalCommandKindDto::WriteStdin),
        _ => Err(unsupported("ApprovalCommandKind")),
    }
}

const fn decode_approval_command_kind(value: ApprovalCommandKindDto) -> ApprovalCommandKind {
    match value {
        ApprovalCommandKindDto::Command => ApprovalCommandKind::Command,
        ApprovalCommandKindDto::WriteStdin => ApprovalCommandKind::WriteStdin,
    }
}

fn encode_approval_file_change_kind(
    value: ApprovalFileChangeKind,
) -> Result<ApprovalFileChangeKindDto, ProtocolError> {
    match value {
        ApprovalFileChangeKind::Add => Ok(ApprovalFileChangeKindDto::Add),
        ApprovalFileChangeKind::Delete => Ok(ApprovalFileChangeKindDto::Delete),
        ApprovalFileChangeKind::Update => Ok(ApprovalFileChangeKindDto::Update),
        _ => Err(unsupported("ApprovalFileChangeKind")),
    }
}

const fn decode_approval_file_change_kind(
    value: ApprovalFileChangeKindDto,
) -> ApprovalFileChangeKind {
    match value {
        ApprovalFileChangeKindDto::Add => ApprovalFileChangeKind::Add,
        ApprovalFileChangeKindDto::Delete => ApprovalFileChangeKind::Delete,
        ApprovalFileChangeKindDto::Update => ApprovalFileChangeKind::Update,
    }
}

fn encode_approval_disposition(
    value: ApprovalDisposition,
) -> Result<ApprovalDispositionDto, ProtocolError> {
    match value {
        ApprovalDisposition::Approve => Ok(ApprovalDispositionDto::Approve),
        ApprovalDisposition::Reject => Ok(ApprovalDispositionDto::Reject),
        ApprovalDisposition::Cancel => Ok(ApprovalDispositionDto::Cancel),
        _ => Err(unsupported("ApprovalDisposition")),
    }
}

const fn decode_approval_disposition(value: ApprovalDispositionDto) -> ApprovalDisposition {
    match value {
        ApprovalDispositionDto::Approve => ApprovalDisposition::Approve,
        ApprovalDispositionDto::Reject => ApprovalDisposition::Reject,
        ApprovalDispositionDto::Cancel => ApprovalDisposition::Cancel,
    }
}

fn encode_timestamp(value: Timestamp, field: &'static str) -> Result<String, ProtocolError> {
    value
        .as_offset_date_time()
        .format(&Rfc3339)
        .map_err(|error| ProtocolError::InvalidWireValue {
            field,
            reason: error.to_string(),
        })
}

fn decode_timestamp(value: String, field: &'static str) -> Result<Timestamp, ProtocolError> {
    OffsetDateTime::parse(&value, &Rfc3339)
        .map(Timestamp::from_offset_date_time)
        .map_err(|error| ProtocolError::InvalidWireValue {
            field,
            reason: error.to_string(),
        })
}

fn parse_id<T>(value: String) -> Result<T, ProtocolError>
where
    T: FromStr<Err = DomainError>,
{
    T::from_str(&value).map_err(ProtocolError::from)
}

fn decode_optional_text(value: Option<String>) -> Result<Option<NonEmptyText>, ProtocolError> {
    value
        .map(NonEmptyText::new)
        .transpose()
        .map_err(ProtocolError::from)
}

const fn unsupported(type_name: &'static str) -> ProtocolError {
    ProtocolError::UnsupportedDomainVariant { type_name }
}

fn duplicate_capability(field: &'static str, capability: &'static str) -> ProtocolError {
    ProtocolError::InvalidWireValue {
        field,
        reason: format!("duplicate capability `{capability}`"),
    }
}

const fn provider_capability_name(value: ProviderCapabilityDto) -> &'static str {
    match value {
        ProviderCapabilityDto::SessionState => "session_state",
        ProviderCapabilityDto::ToolEvents => "tool_events",
        ProviderCapabilityDto::Plan => "plan",
        ProviderCapabilityDto::Progress => "progress",
        ProviderCapabilityDto::ApprovalRequest => "approval_request",
        ProviderCapabilityDto::ApprovalResponse => "approval_response",
        ProviderCapabilityDto::UserInputRequest => "user_input_request",
        ProviderCapabilityDto::UserInputResponse => "user_input_response",
        ProviderCapabilityDto::PromptSubmit => "prompt_submit",
        ProviderCapabilityDto::Cancel => "cancel",
    }
}

const fn channel_capability_name(value: ChannelCapabilityDto) -> &'static str {
    match value {
        ChannelCapabilityDto::Notification => "notification",
        ChannelCapabilityDto::SessionView => "session_view",
        ChannelCapabilityDto::ToolView => "tool_view",
        ChannelCapabilityDto::PlanView => "plan_view",
        ChannelCapabilityDto::ProgressView => "progress_view",
        ChannelCapabilityDto::RichMessage => "rich_message",
        ChannelCapabilityDto::Approval => "approval",
        ChannelCapabilityDto::ChoiceInput => "choice_input",
        ChannelCapabilityDto::TextInput => "text_input",
        ChannelCapabilityDto::FormInput => "form_input",
        ChannelCapabilityDto::RealtimeSync => "realtime_sync",
        ChannelCapabilityDto::RemoteCommand => "remote_command",
    }
}
