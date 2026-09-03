//! Commands sent from a Channel toward a Provider.

use crate::{
    ChannelCapabilities, ChannelId, CommandId, NonEmptyText, ProviderCapabilities, SessionId,
    Timestamp,
};

/// Delivery policy for text submitted while a turn may already be active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptDelivery {
    /// Append to the per-session FIFO and send when the session becomes idle.
    Queue,
    /// Immediately steer the active turn, failing when no turn is active.
    Steer,
}

/// Operations on the bounded in-memory prompt queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueAction {
    /// Stop automatic queue draining while preserving every entry.
    Pause,
    /// Continue automatic queue draining.
    Resume,
    /// Remove every queued prompt.
    Clear,
}

/// The semantic payload of a remote agent command.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AgentCommandPayload {
    /// Submits a new prompt to the session.
    SubmitPrompt {
        /// The non-empty prompt text.
        text: NonEmptyText,
        /// Whether the prompt is queued or immediately steers an active turn.
        delivery: PromptDelivery,
    },
    /// Requests cancellation of the current session work.
    CancelSession {
        /// An optional user-facing cancellation reason.
        reason: Option<NonEmptyText>,
    },
    /// Requests the provider's current model catalog.
    ListModels,
    /// Sets the model and optional reasoning effort for future turns.
    SelectModel {
        /// Provider model identifier.
        model: NonEmptyText,
        /// Optional provider reasoning-effort identifier.
        effort: Option<NonEmptyText>,
    },
    /// Enables or disables Plan collaboration mode for future turns.
    SetPlanMode {
        /// Whether Plan mode is enabled.
        enabled: bool,
    },
    /// Requests a page of resumable threads, newest first.
    ListThreads {
        /// Optional provider pagination cursor.
        cursor: Option<NonEmptyText>,
    },
    /// Resumes an existing provider thread.
    ResumeThread {
        /// Provider thread identifier.
        thread_id: NonEmptyText,
    },
    /// Starts a clean thread in the supplied working directory.
    StartThread {
        /// Working directory for the new thread.
        cwd: NonEmptyText,
    },
    /// Starts provider-side context compaction.
    Compact,
    /// Starts a review, optionally with user instructions.
    Review {
        /// Optional review instructions.
        instructions: Option<NonEmptyText>,
    },
    /// Renames the current thread.
    Rename {
        /// New thread name.
        name: NonEmptyText,
    },
    /// Forks the current thread.
    Fork,
    /// Requests a concise runtime/session status report.
    Status,
    /// Requests the provider's permission-profile catalog.
    ListPermissionProfiles,
    /// Selects a permission profile for future turns.
    SelectPermissionProfile {
        /// Provider permission-profile identifier.
        profile: NonEmptyText,
    },
    /// Pauses, resumes, or clears the in-memory prompt queue.
    Queue {
        /// Queue operation to apply.
        action: QueueAction,
    },
}

impl AgentCommandPayload {
    /// Returns the Provider capability required to execute this command.
    #[must_use]
    pub const fn required_provider_capability(&self) -> ProviderCapabilities {
        match self {
            Self::SubmitPrompt { .. } => ProviderCapabilities::PROMPT_SUBMIT,
            Self::CancelSession { .. } => ProviderCapabilities::CANCEL,
            Self::ListModels
            | Self::SelectModel { .. }
            | Self::SetPlanMode { .. }
            | Self::ListThreads { .. }
            | Self::ResumeThread { .. }
            | Self::StartThread { .. }
            | Self::Compact
            | Self::Review { .. }
            | Self::Rename { .. }
            | Self::Fork
            | Self::Status
            | Self::ListPermissionProfiles
            | Self::SelectPermissionProfile { .. }
            | Self::Queue { .. } => ProviderCapabilities::CONTROL,
        }
    }

    /// Returns the Channel capabilities required to originate this command.
    #[must_use]
    pub const fn required_channel_capabilities(&self) -> ChannelCapabilities {
        match self {
            Self::SubmitPrompt { .. }
            | Self::SelectModel { .. }
            | Self::ListThreads { .. }
            | Self::ResumeThread { .. }
            | Self::StartThread { .. }
            | Self::Review { .. }
            | Self::Rename { .. }
            | Self::SelectPermissionProfile { .. } => {
                ChannelCapabilities::REMOTE_COMMAND.union(ChannelCapabilities::TEXT_INPUT)
            }
            Self::CancelSession { .. }
            | Self::ListModels
            | Self::SetPlanMode { .. }
            | Self::Compact
            | Self::Fork
            | Self::Status
            | Self::ListPermissionProfiles
            | Self::Queue { .. } => ChannelCapabilities::REMOTE_COMMAND,
        }
    }
}

/// A Channel-originated command targeting one agent session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentCommand {
    id: CommandId,
    session_id: SessionId,
    channel_id: ChannelId,
    issued_at: Timestamp,
    payload: AgentCommandPayload,
}

impl AgentCommand {
    /// Creates a remote command.
    #[must_use]
    pub const fn new(
        id: CommandId,
        session_id: SessionId,
        channel_id: ChannelId,
        issued_at: Timestamp,
        payload: AgentCommandPayload,
    ) -> Self {
        Self {
            id,
            session_id,
            channel_id,
            issued_at,
            payload,
        }
    }

    /// Returns the command identifier.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        self.id
    }

    /// Returns the target session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the source Channel identifier.
    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// Returns when the command was issued.
    #[must_use]
    pub const fn issued_at(&self) -> Timestamp {
        self.issued_at
    }

    /// Borrows the command payload.
    #[must_use]
    pub const fn payload(&self) -> &AgentCommandPayload {
        &self.payload
    }

    /// Returns the Provider capability required to execute this command.
    #[must_use]
    pub const fn required_provider_capability(&self) -> ProviderCapabilities {
        self.payload.required_provider_capability()
    }

    /// Returns the Channel capabilities required to originate this command.
    #[must_use]
    pub const fn required_channel_capabilities(&self) -> ChannelCapabilities {
        self.payload.required_channel_capabilities()
    }
}
