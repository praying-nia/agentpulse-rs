//! Normalized agent events.

use crate::{
    AgentCommand, AgentSession, AgentState, ConnectionState, DomainError, EventId, EventSequence,
    InteractionClosed, InteractionRequest, InteractionResponse, NonEmptyText, PlanSnapshot,
    ProgressSnapshot, ProviderCapabilities, SessionId, Timestamp, ToolCallId,
};

/// Severity of a normalized user-facing agent message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AgentMessageLevel {
    /// Informational activity.
    Info,
    /// Activity requiring user attention.
    Warning,
    /// An error that may affect the current run.
    Error,
}

/// A normalized user-facing agent message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentMessage {
    level: AgentMessageLevel,
    content: NonEmptyText,
}

impl AgentMessage {
    /// Creates a normalized agent message.
    #[must_use]
    pub const fn new(level: AgentMessageLevel, content: NonEmptyText) -> Self {
        Self { level, content }
    }

    /// Returns the message severity.
    #[must_use]
    pub const fn level(&self) -> AgentMessageLevel {
        self.level
    }

    /// Borrows the message content.
    #[must_use]
    pub const fn content(&self) -> &NonEmptyText {
        &self.content
    }
}

/// The normalized outcome of a tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolOutcome {
    /// The tool completed successfully.
    Succeeded,
    /// The tool failed.
    Failed,
    /// The tool call was cancelled.
    Cancelled,
}

/// Sanitized tool activity without raw arguments, outputs, or secrets.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolActivity {
    /// A tool call started.
    Started {
        /// The normalized tool call identifier.
        call_id: ToolCallId,
        /// The normalized tool name.
        name: NonEmptyText,
        /// An optional sanitized summary.
        summary: Option<NonEmptyText>,
    },
    /// A tool call finished.
    Finished {
        /// The normalized tool call identifier.
        call_id: ToolCallId,
        /// The normalized outcome.
        outcome: ToolOutcome,
        /// An optional sanitized summary.
        summary: Option<NonEmptyText>,
    },
}

/// The result of the latest session run.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SessionOutcome {
    /// The run completed successfully.
    Completed {
        /// An optional sanitized completion summary.
        summary: Option<NonEmptyText>,
    },
    /// The run failed.
    Failed {
        /// A normalized, user-facing failure message.
        error: NonEmptyText,
    },
    /// The run was cancelled.
    Cancelled {
        /// An optional user-facing reason.
        reason: Option<NonEmptyText>,
    },
}

/// The normalized semantic payload of an agent event.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AgentEventPayload {
    /// Announces a complete initial session snapshot.
    SessionStarted(AgentSession),
    /// Updates the latest observed execution state.
    StateChanged(AgentState),
    /// Updates Provider connectivity independently of execution state.
    ConnectionChanged(ConnectionState),
    /// Publishes a normalized user-facing message.
    Message(AgentMessage),
    /// Publishes sanitized tool activity.
    ToolActivity(ToolActivity),
    /// Replaces the current plan with a complete snapshot.
    PlanUpdated(PlanSnapshot),
    /// Replaces the current progress with a complete snapshot.
    ProgressUpdated(ProgressSnapshot),
    /// Publishes a correlated interaction request.
    InteractionRequested(InteractionRequest),
    /// Records a correlated interaction response.
    InteractionResponded(InteractionResponse),
    /// Closes a pending interaction without a Channel response.
    InteractionClosed(InteractionClosed),
    /// Records a remote command.
    CommandIssued(AgentCommand),
    /// Records the latest run outcome.
    SessionEnded(SessionOutcome),
}

impl AgentEventPayload {
    /// Returns the Provider capability required to publish this event.
    ///
    /// User-facing messages are part of the baseline normalized event stream
    /// and therefore require no additional declared capability.
    #[must_use]
    pub const fn required_provider_capability(&self) -> ProviderCapabilities {
        match self {
            Self::SessionStarted(_)
            | Self::StateChanged(_)
            | Self::ConnectionChanged(_)
            | Self::SessionEnded(_) => ProviderCapabilities::SESSION_STATE,
            Self::Message(_) => ProviderCapabilities::NONE,
            Self::ToolActivity(_) => ProviderCapabilities::TOOL_EVENTS,
            Self::PlanUpdated(_) => ProviderCapabilities::PLAN,
            Self::ProgressUpdated(_) => ProviderCapabilities::PROGRESS,
            Self::InteractionRequested(request) => request.required_provider_request_capability(),
            Self::InteractionResponded(response) => response.required_provider_capability(),
            Self::InteractionClosed(_) => ProviderCapabilities::APPROVAL_REQUEST,
            Self::CommandIssued(command) => command.required_provider_capability(),
        }
    }
}

/// A normalized event ordered within one AgentPulse session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentEvent {
    id: EventId,
    session_id: SessionId,
    sequence: EventSequence,
    occurred_at: Timestamp,
    payload: AgentEventPayload,
}

impl AgentEvent {
    /// Creates an event and validates identifiers embedded in correlated payloads.
    pub fn new(
        id: EventId,
        session_id: SessionId,
        sequence: EventSequence,
        occurred_at: Timestamp,
        payload: AgentEventPayload,
    ) -> Result<Self, DomainError> {
        let embedded_session_id = match &payload {
            AgentEventPayload::SessionStarted(session) => Some(session.id()),
            AgentEventPayload::InteractionRequested(request) => Some(request.session_id()),
            AgentEventPayload::InteractionResponded(response) => Some(response.session_id()),
            AgentEventPayload::InteractionClosed(closed) => Some(closed.session_id()),
            AgentEventPayload::CommandIssued(command) => Some(command.session_id()),
            _ => None,
        };

        if let Some(embedded_session_id) = embedded_session_id
            && embedded_session_id != session_id
        {
            return Err(DomainError::CorrelationMismatch {
                field: "event session ID",
                expected: session_id.to_string(),
                actual: embedded_session_id.to_string(),
            });
        }

        Ok(Self {
            id,
            session_id,
            sequence,
            occurred_at,
            payload,
        })
    }

    /// Returns the event identifier.
    #[must_use]
    pub const fn id(&self) -> EventId {
        self.id
    }

    /// Returns the owning session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the per-session event sequence.
    #[must_use]
    pub const fn sequence(&self) -> EventSequence {
        self.sequence
    }

    /// Returns when the normalized event occurred.
    #[must_use]
    pub const fn occurred_at(&self) -> Timestamp {
        self.occurred_at
    }

    /// Borrows the event payload.
    #[must_use]
    pub const fn payload(&self) -> &AgentEventPayload {
        &self.payload
    }

    /// Returns the Provider capability required to publish this event.
    #[must_use]
    pub const fn required_provider_capability(&self) -> ProviderCapabilities {
        self.payload.required_provider_capability()
    }
}
