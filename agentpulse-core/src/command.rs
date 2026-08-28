//! Commands sent from a Channel toward a Provider.

use crate::{
    ChannelCapabilities, ChannelId, CommandId, NonEmptyText, ProviderCapabilities, SessionId,
    Timestamp,
};

/// The semantic payload of a remote agent command.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AgentCommandPayload {
    /// Submits a new prompt to the session.
    SubmitPrompt {
        /// The non-empty prompt text.
        text: NonEmptyText,
    },
    /// Requests cancellation of the current session work.
    CancelSession {
        /// An optional user-facing cancellation reason.
        reason: Option<NonEmptyText>,
    },
}

impl AgentCommandPayload {
    /// Returns the Provider capability required to execute this command.
    #[must_use]
    pub const fn required_provider_capability(&self) -> ProviderCapabilities {
        match self {
            Self::SubmitPrompt { .. } => ProviderCapabilities::PROMPT_SUBMIT,
            Self::CancelSession { .. } => ProviderCapabilities::CANCEL,
        }
    }

    /// Returns the Channel capabilities required to originate this command.
    #[must_use]
    pub const fn required_channel_capabilities(&self) -> ChannelCapabilities {
        match self {
            Self::SubmitPrompt { .. } => {
                ChannelCapabilities::REMOTE_COMMAND.union(ChannelCapabilities::TEXT_INPUT)
            }
            Self::CancelSession { .. } => ChannelCapabilities::REMOTE_COMMAND,
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
