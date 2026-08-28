//! Agent session snapshots and state.

use crate::{
    DomainError, ExternalId, NonEmptyText, ProviderId, Revision, SessionId, Timestamp, WorkspaceRef,
};

/// The latest observed execution state of an agent session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AgentState {
    /// The Provider is discovering or initializing the session.
    Initializing,
    /// The session is available but not currently executing work.
    Idle,
    /// The agent is actively executing work.
    Running,
    /// The agent is waiting for a correlated interaction response.
    WaitingForInteraction,
    /// The latest run completed successfully.
    Completed,
    /// The latest run failed.
    Failed,
    /// The latest run was cancelled.
    Cancelled,
}

/// Connectivity between the Bridge and the Provider session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConnectionState {
    /// The Provider session is reachable.
    Connected,
    /// The Bridge is attempting to restore connectivity.
    Reconnecting,
    /// The Provider session is currently unreachable.
    Disconnected,
}

/// A validated snapshot of one AgentPulse agent session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSession {
    id: SessionId,
    provider_id: ProviderId,
    external_id: Option<ExternalId>,
    title: Option<NonEmptyText>,
    workspace: Option<WorkspaceRef>,
    state: AgentState,
    connection_state: ConnectionState,
    revision: Revision,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl AgentSession {
    /// Starts building a session snapshot with safe initial defaults.
    pub const fn builder(
        id: SessionId,
        provider_id: ProviderId,
        created_at: Timestamp,
    ) -> AgentSessionBuilder {
        AgentSessionBuilder {
            id,
            provider_id,
            external_id: None,
            title: None,
            workspace: None,
            state: AgentState::Initializing,
            connection_state: ConnectionState::Connected,
            revision: Revision::FIRST,
            created_at,
            updated_at: created_at,
        }
    }

    /// Returns the AgentPulse session identifier.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Returns the owning Provider instance identifier.
    #[must_use]
    pub const fn provider_id(&self) -> ProviderId {
        self.provider_id
    }

    /// Borrows the optional Provider-native session identifier.
    #[must_use]
    pub const fn external_id(&self) -> Option<&ExternalId> {
        self.external_id.as_ref()
    }

    /// Borrows the optional user-facing title.
    #[must_use]
    pub const fn title(&self) -> Option<&NonEmptyText> {
        self.title.as_ref()
    }

    /// Borrows the optional workspace reference.
    #[must_use]
    pub const fn workspace(&self) -> Option<&WorkspaceRef> {
        self.workspace.as_ref()
    }

    /// Returns the latest execution state.
    #[must_use]
    pub const fn state(&self) -> AgentState {
        self.state
    }

    /// Returns the latest Provider connectivity state.
    #[must_use]
    pub const fn connection_state(&self) -> ConnectionState {
        self.connection_state
    }

    /// Returns the snapshot revision.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Returns when the session was first observed.
    #[must_use]
    pub const fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// Returns when this snapshot was updated.
    #[must_use]
    pub const fn updated_at(&self) -> Timestamp {
        self.updated_at
    }
}

/// A builder that validates an [`AgentSession`] snapshot before construction.
#[derive(Clone, Debug)]
#[must_use]
pub struct AgentSessionBuilder {
    id: SessionId,
    provider_id: ProviderId,
    external_id: Option<ExternalId>,
    title: Option<NonEmptyText>,
    workspace: Option<WorkspaceRef>,
    state: AgentState,
    connection_state: ConnectionState,
    revision: Revision,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl AgentSessionBuilder {
    /// Sets the Provider-native session identifier.
    pub fn external_id(mut self, external_id: ExternalId) -> Self {
        self.external_id = Some(external_id);
        self
    }

    /// Sets the user-facing session title.
    pub fn title(mut self, title: NonEmptyText) -> Self {
        self.title = Some(title);
        self
    }

    /// Sets the session workspace reference.
    pub fn workspace(mut self, workspace: WorkspaceRef) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Sets the latest execution state.
    pub const fn state(mut self, state: AgentState) -> Self {
        self.state = state;
        self
    }

    /// Sets the latest Provider connectivity state.
    pub const fn connection_state(mut self, connection_state: ConnectionState) -> Self {
        self.connection_state = connection_state;
        self
    }

    /// Sets the snapshot revision and update time.
    pub const fn revision(mut self, revision: Revision, updated_at: Timestamp) -> Self {
        self.revision = revision;
        self.updated_at = updated_at;
        self
    }

    /// Validates timestamp ordering and creates the session snapshot.
    pub fn build(self) -> Result<AgentSession, DomainError> {
        if self.updated_at < self.created_at {
            return Err(DomainError::InvalidTimeOrder {
                earlier_field: "session created_at",
                later_field: "session updated_at",
            });
        }

        Ok(AgentSession {
            id: self.id,
            provider_id: self.provider_id,
            external_id: self.external_id,
            title: self.title,
            workspace: self.workspace,
            state: self.state,
            connection_state: self.connection_state,
            revision: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
