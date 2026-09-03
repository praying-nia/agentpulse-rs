//! Strict Serde DTOs for the JSON v2 contract.

use serde::{Deserialize, Serialize};

use super::scalar::DecimalU64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvelopeDto {
    pub(super) protocol_version: u16,
    pub(super) message: MessageDto,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum MessageDto {
    ProviderDescriptor(ProviderDescriptorDto),
    ChannelDescriptor(ChannelDescriptorDto),
    AgentSession(AgentSessionDto),
    AgentEvent(AgentEventDto),
    InteractionRequest(InteractionRequestDto),
    InteractionResponse(InteractionResponseDto),
    AgentCommand(AgentCommandDto),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderDescriptorDto {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) version: Option<String>,
    pub(super) capabilities: Vec<ProviderCapabilityDto>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProviderCapabilityDto {
    SessionState,
    ToolEvents,
    Plan,
    Progress,
    ApprovalRequest,
    ApprovalResponse,
    UserInputRequest,
    UserInputResponse,
    PromptSubmit,
    Cancel,
    Control,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChannelDescriptorDto {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) version: Option<String>,
    pub(super) capabilities: Vec<ChannelCapabilityDto>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ChannelCapabilityDto {
    Notification,
    SessionView,
    ToolView,
    PlanView,
    ProgressView,
    RichMessage,
    Approval,
    ChoiceInput,
    TextInput,
    FormInput,
    RealtimeSync,
    RemoteCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentSessionDto {
    pub(super) id: String,
    pub(super) provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) workspace: Option<WorkspaceRefDto>,
    pub(super) state: AgentStateDto,
    pub(super) connection_state: ConnectionStateDto,
    pub(super) revision: DecimalU64,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceRefDto {
    pub(super) path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) display_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentStateDto {
    Initializing,
    Idle,
    Running,
    WaitingForInteraction,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ConnectionStateDto {
    Connected,
    Reconnecting,
    Disconnected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentEventDto {
    pub(super) id: String,
    pub(super) session_id: String,
    pub(super) sequence: DecimalU64,
    pub(super) occurred_at: String,
    pub(super) payload: AgentEventPayloadDto,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum AgentEventPayloadDto {
    SessionStarted {
        session: AgentSessionDto,
    },
    StateChanged {
        state: AgentStateDto,
    },
    ConnectionChanged {
        connection_state: ConnectionStateDto,
    },
    Message {
        message: AgentMessageDto,
    },
    ToolActivity {
        activity: ToolActivityDto,
    },
    PlanUpdated {
        plan: PlanSnapshotDto,
    },
    ProgressUpdated {
        progress: ProgressSnapshotDto,
    },
    InteractionRequested {
        request: InteractionRequestDto,
    },
    InteractionResponded {
        response: InteractionResponseDto,
    },
    InteractionClosed {
        interaction: InteractionClosedDto,
    },
    CommandIssued {
        command: AgentCommandDto,
    },
    SessionEnded {
        outcome: SessionOutcomeDto,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentMessageDto {
    pub(super) role: AgentMessageRoleDto,
    pub(super) level: AgentMessageLevelDto,
    pub(super) content: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentMessageRoleDto {
    User,
    Assistant,
    System,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentMessageLevelDto {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ToolActivityDto {
    Started {
        call_id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Finished {
        call_id: String,
        outcome: ToolOutcomeDto,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ToolOutcomeDto {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum SessionOutcomeDto {
    Completed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Failed {
        error: String,
    },
    Cancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanSnapshotDto {
    pub(super) revision: DecimalU64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) explanation: Option<String>,
    pub(super) items: Vec<PlanItemDto>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanItemDto {
    pub(super) id: String,
    pub(super) content: String,
    pub(super) status: PlanItemStatusDto,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PlanItemStatusDto {
    Pending,
    InProgress,
    Completed,
    Blocked,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProgressSnapshotDto {
    pub(super) revision: DecimalU64,
    pub(super) value: ProgressValueDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) message: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ProgressValueDto {
    Indeterminate,
    Determinate {
        completed: DecimalU64,
        total: DecimalU64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InteractionRequestDto {
    pub(super) id: String,
    pub(super) session_id: String,
    pub(super) requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) expires_at: Option<String>,
    pub(super) prompt: String,
    pub(super) payload: InteractionRequestPayloadDto,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum InteractionRequestPayloadDto {
    Approval {
        subject: ApprovalSubjectDto,
        options: Vec<ApprovalOptionDto>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unavailable_reason: Option<String>,
    },
    Choice {
        options: Vec<ChoiceOptionDto>,
        multiple: bool,
    },
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        multiline: bool,
    },
    Form {
        fields: Vec<FormFieldDto>,
        blocking: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FormFieldDto {
    pub(super) id: String,
    pub(super) header: String,
    pub(super) prompt: String,
    pub(super) options: Vec<ChoiceOptionDto>,
    pub(super) allows_other: bool,
    pub(super) sensitive: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ApprovalSubjectDto {
    Command {
        kind: ApprovalCommandKindDto,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<ApprovalNetworkContextDto>,
    },
    FileChange {
        changes: Vec<ApprovalFileChangeDto>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grant_root: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApprovalCommandKindDto {
    Command,
    WriteStdin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApprovalNetworkContextDto {
    pub(super) host: String,
    pub(super) protocol: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApprovalFileChangeDto {
    pub(super) path: String,
    pub(super) kind: ApprovalFileChangeKindDto,
    pub(super) diff: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApprovalFileChangeKindDto {
    Add,
    Delete,
    Update,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApprovalOptionDto {
    pub(super) id: String,
    pub(super) disposition: ApprovalDispositionDto,
    pub(super) label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) description: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApprovalDispositionDto {
    Approve,
    Reject,
    Cancel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChoiceOptionDto {
    pub(super) id: String,
    pub(super) label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InteractionResponseDto {
    pub(super) request_id: String,
    pub(super) session_id: String,
    pub(super) channel_id: String,
    pub(super) responded_at: String,
    pub(super) payload: InteractionResponsePayloadDto,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum InteractionResponsePayloadDto {
    Approval { option_id: String },
    Choice { option_ids: Vec<String> },
    Text { text: String },
    Form { answers: Vec<FormAnswerDto> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FormAnswerDto {
    pub(super) field_id: String,
    pub(super) value: FormAnswerValueDto,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum FormAnswerValueDto {
    Choice { option_id: String },
    Text { text: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InteractionClosedDto {
    pub(super) request_id: String,
    pub(super) session_id: String,
    pub(super) reason: InteractionCloseReasonDto,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum InteractionCloseReasonDto {
    ResolvedElsewhere,
    ProviderCancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentCommandDto {
    pub(super) id: String,
    pub(super) session_id: String,
    pub(super) channel_id: String,
    pub(super) issued_at: String,
    pub(super) payload: AgentCommandPayloadDto,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum AgentCommandPayloadDto {
    SubmitPrompt {
        text: String,
        delivery: PromptDeliveryDto,
    },
    CancelSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    ListModels,
    SelectModel {
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
    SetPlanMode {
        enabled: bool,
    },
    ListThreads {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    ResumeThread {
        thread_id: String,
    },
    StartThread {
        cwd: String,
    },
    Compact,
    Review {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    Rename {
        name: String,
    },
    Fork,
    Status,
    ListPermissionProfiles,
    SelectPermissionProfile {
        profile: String,
    },
    Queue {
        action: QueueActionDto,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PromptDeliveryDto {
    Queue,
    Steer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum QueueActionDto {
    Pause,
    Resume,
    Clear,
}
