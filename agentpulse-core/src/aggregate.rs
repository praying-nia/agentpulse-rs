//! Deterministic reduction of session events into current state.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::Arc,
};

use thiserror::Error;

use crate::{
    AgentEvent, AgentEventPayload, AgentSession, AgentState, DomainError, EventId, EventSequence,
    InteractionId, InteractionRequest, NonEmptyText, PlanSnapshot, ProgressSnapshot, Revision,
    SessionId, SessionOutcome, Timestamp, ToolActivity, ToolCallId,
};

/// Identifies a revisioned snapshot stream in a reduction error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SnapshotKind {
    /// The complete plan snapshot stream.
    Plan,
    /// The complete progress snapshot stream.
    Progress,
}

impl fmt::Display for SnapshotKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan => formatter.write_str("plan"),
            Self::Progress => formatter.write_str("progress"),
        }
    }
}

/// An error raised when an event cannot be safely applied to a session aggregate.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum ReduceError {
    /// Replay was requested without any events.
    #[error("cannot replay an empty session event stream")]
    EmptyReplay,

    /// The first event did not use the first valid per-session sequence.
    #[error("initial event sequence must be 1, got {actual}")]
    InvalidInitialSequence {
        /// The rejected sequence.
        actual: EventSequence,
    },

    /// The first event did not announce a complete initial session snapshot.
    #[error("initial event must contain SessionStarted")]
    InitialEventNotSessionStarted,

    /// An event belongs to a different session.
    #[error("event session mismatch: expected {expected}, got {actual}")]
    SessionMismatch {
        /// The aggregate session identifier.
        expected: SessionId,
        /// The rejected event session identifier.
        actual: SessionId,
    },

    /// An event sequence is older than the current aggregate cursor.
    #[error("stale event sequence: last applied {last}, got {actual}")]
    StaleSequence {
        /// The last successfully applied sequence.
        last: EventSequence,
        /// The rejected older sequence.
        actual: EventSequence,
    },

    /// A different event attempted to reuse the last applied sequence.
    #[error(
        "event sequence {sequence} conflict: expected event {expected_event_id}, got {actual_event_id}"
    )]
    SequenceConflict {
        /// The reused sequence.
        sequence: EventSequence,
        /// The last successfully applied event identifier.
        expected_event_id: EventId,
        /// The conflicting event identifier.
        actual_event_id: EventId,
    },

    /// An event skipped one or more required sequences.
    #[error("event sequence gap: expected {expected}, got {actual}")]
    SequenceGap {
        /// The next required sequence.
        expected: EventSequence,
        /// The rejected sequence.
        actual: EventSequence,
    },

    /// No sequence exists after the current cursor.
    #[error("event sequence is exhausted")]
    SequenceExhausted,

    /// A second initial-session event appeared after aggregate construction.
    #[error("SessionStarted can only be the first event")]
    UnexpectedSessionStarted,

    /// A complete snapshot did not advance its own revision stream.
    #[error("stale {snapshot} revision: current {current}, incoming {incoming}")]
    StaleRevision {
        /// The rejected snapshot stream.
        snapshot: SnapshotKind,
        /// The currently applied revision.
        current: Revision,
        /// The rejected revision.
        incoming: Revision,
    },

    /// The current session snapshot cannot advance to another revision.
    #[error("session revision is exhausted")]
    SessionRevisionExhausted,

    /// A tool call started while the same identifier was already active.
    #[error("tool call is already active: {call_id}")]
    ToolAlreadyActive {
        /// The duplicated tool call identifier.
        call_id: ToolCallId,
    },

    /// A tool completion did not match an active tool call.
    #[error("tool call is not active: {call_id}")]
    ToolNotActive {
        /// The unmatched tool call identifier.
        call_id: ToolCallId,
    },

    /// An interaction request reused an identifier that is still pending.
    #[error("interaction is already pending: {interaction_id}")]
    InteractionAlreadyPending {
        /// The duplicated interaction identifier.
        interaction_id: InteractionId,
    },

    /// An interaction response did not match a pending request.
    #[error("interaction is not pending: {interaction_id}")]
    InteractionNotPending {
        /// The unmatched interaction identifier.
        interaction_id: InteractionId,
    },

    /// A correlated interaction response violated the request contract.
    #[error("invalid response for interaction {interaction_id}: {source}")]
    InvalidInteractionResponse {
        /// The correlated interaction identifier.
        interaction_id: InteractionId,
        /// The existing domain validation error.
        #[source]
        source: DomainError,
    },
}

/// The result of successfully attempting to apply one event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ApplyOutcome {
    /// The event advanced the aggregate.
    Applied,
    /// The event was an exact retry of the last successfully applied event.
    AlreadyApplied,
}

/// In-memory retention settings for a session aggregate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionAggregateConfig {
    recent_event_capacity: usize,
}

impl SessionAggregateConfig {
    /// The default number of recent events retained for lightweight inspection.
    pub const DEFAULT_RECENT_EVENT_CAPACITY: usize = 256;

    /// Creates a configuration with the requested recent-event capacity.
    ///
    /// A capacity of zero disables recent-event retention. Current aggregate
    /// state and the last event cursor are retained independently.
    #[must_use]
    pub const fn new(recent_event_capacity: usize) -> Self {
        Self {
            recent_event_capacity,
        }
    }

    /// Returns the maximum number of recent events retained in memory.
    #[must_use]
    pub const fn recent_event_capacity(self) -> usize {
        self.recent_event_capacity
    }
}

impl Default for SessionAggregateConfig {
    fn default() -> Self {
        Self::new(Self::DEFAULT_RECENT_EVENT_CAPACITY)
    }
}

/// The current state of one tool call that has started but not finished.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveToolCall {
    id: ToolCallId,
    name: NonEmptyText,
    summary: Option<NonEmptyText>,
    started_at: Timestamp,
}

impl ActiveToolCall {
    fn new(
        id: ToolCallId,
        name: NonEmptyText,
        summary: Option<NonEmptyText>,
        started_at: Timestamp,
    ) -> Self {
        Self {
            id,
            name,
            summary,
            started_at,
        }
    }

    /// Returns the normalized tool call identifier.
    #[must_use]
    pub const fn id(&self) -> ToolCallId {
        self.id
    }

    /// Borrows the normalized tool name.
    #[must_use]
    pub const fn name(&self) -> &NonEmptyText {
        &self.name
    }

    /// Borrows the optional sanitized start summary.
    #[must_use]
    pub const fn summary(&self) -> Option<&NonEmptyText> {
        self.summary.as_ref()
    }

    /// Returns when the tool call started.
    #[must_use]
    pub const fn started_at(&self) -> Timestamp {
        self.started_at
    }
}

/// The deterministic current-state projection of one AgentPulse session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionAggregate {
    config: SessionAggregateConfig,
    session: AgentSession,
    plan: Option<PlanSnapshot>,
    progress: Option<ProgressSnapshot>,
    active_tool_calls: BTreeMap<ToolCallId, ActiveToolCall>,
    pending_interactions: BTreeMap<InteractionId, InteractionRequest>,
    latest_outcome: Option<SessionOutcome>,
    last_event: Arc<AgentEvent>,
    recent_events: VecDeque<Arc<AgentEvent>>,
}

impl SessionAggregate {
    /// Creates an aggregate from a sequence-one `SessionStarted` event.
    pub fn from_initial_event(initial_event: AgentEvent) -> Result<Self, ReduceError> {
        Self::from_initial_event_with_config(initial_event, SessionAggregateConfig::default())
    }

    /// Creates an aggregate using explicit in-memory retention settings.
    pub fn from_initial_event_with_config(
        initial_event: AgentEvent,
        config: SessionAggregateConfig,
    ) -> Result<Self, ReduceError> {
        if initial_event.sequence() != EventSequence::FIRST {
            return Err(ReduceError::InvalidInitialSequence {
                actual: initial_event.sequence(),
            });
        }

        let AgentEventPayload::SessionStarted(session) = initial_event.payload() else {
            return Err(ReduceError::InitialEventNotSessionStarted);
        };
        let session = session.clone();

        let initial_event = Arc::new(initial_event);
        let mut recent_events = VecDeque::new();
        if config.recent_event_capacity() > 0 {
            recent_events.push_back(Arc::clone(&initial_event));
        }

        Ok(Self {
            config,
            session,
            plan: None,
            progress: None,
            active_tool_calls: BTreeMap::new(),
            pending_interactions: BTreeMap::new(),
            latest_outcome: None,
            last_event: initial_event,
            recent_events,
        })
    }

    /// Rebuilds an aggregate from an owned event stream using default settings.
    pub fn replay<I>(events: I) -> Result<Self, ReduceError>
    where
        I: IntoIterator<Item = AgentEvent>,
    {
        Self::replay_with_config(events, SessionAggregateConfig::default())
    }

    /// Rebuilds an aggregate from an owned event stream using explicit settings.
    pub fn replay_with_config<I>(
        events: I,
        config: SessionAggregateConfig,
    ) -> Result<Self, ReduceError>
    where
        I: IntoIterator<Item = AgentEvent>,
    {
        let mut events = events.into_iter();
        let initial_event = events.next().ok_or(ReduceError::EmptyReplay)?;
        let mut aggregate = Self::from_initial_event_with_config(initial_event, config)?;
        for event in events {
            let _ = aggregate.apply(event)?;
        }
        Ok(aggregate)
    }

    /// Applies one event atomically according to the per-session stream rules.
    pub fn apply(&mut self, event: AgentEvent) -> Result<ApplyOutcome, ReduceError> {
        if self.validate_cursor(&event)? {
            return Ok(ApplyOutcome::AlreadyApplied);
        }

        match event.payload() {
            AgentEventPayload::SessionStarted(_) => {
                return Err(ReduceError::UnexpectedSessionStarted);
            }
            AgentEventPayload::StateChanged(state) => {
                let revision = self.next_session_revision()?;
                self.session
                    .observe_state(*state, revision, event.occurred_at());
            }
            AgentEventPayload::ConnectionChanged(connection_state) => {
                let revision = self.next_session_revision()?;
                self.session.observe_connection_state(
                    *connection_state,
                    revision,
                    event.occurred_at(),
                );
            }
            AgentEventPayload::Message(_) | AgentEventPayload::CommandIssued(_) => {}
            AgentEventPayload::ToolActivity(activity) => {
                self.apply_tool_activity(activity, event.occurred_at())?;
            }
            AgentEventPayload::PlanUpdated(plan) => {
                Self::validate_snapshot_revision(
                    SnapshotKind::Plan,
                    self.plan.as_ref().map(PlanSnapshot::revision),
                    plan.revision(),
                )?;
                self.plan = Some(plan.clone());
            }
            AgentEventPayload::ProgressUpdated(progress) => {
                Self::validate_snapshot_revision(
                    SnapshotKind::Progress,
                    self.progress.as_ref().map(ProgressSnapshot::revision),
                    progress.revision(),
                )?;
                self.progress = Some(progress.clone());
            }
            AgentEventPayload::InteractionRequested(request) => {
                if self.pending_interactions.contains_key(&request.id()) {
                    return Err(ReduceError::InteractionAlreadyPending {
                        interaction_id: request.id(),
                    });
                }
                self.pending_interactions
                    .insert(request.id(), request.clone());
            }
            AgentEventPayload::InteractionResponded(response) => {
                let interaction_id = response.request_id();
                let request = self
                    .pending_interactions
                    .get(&interaction_id)
                    .ok_or(ReduceError::InteractionNotPending { interaction_id })?;
                request.validate_response(response).map_err(|source| {
                    ReduceError::InvalidInteractionResponse {
                        interaction_id,
                        source,
                    }
                })?;
                let _ = self.pending_interactions.remove(&interaction_id);
            }
            AgentEventPayload::SessionEnded(outcome) => {
                let revision = self.next_session_revision()?;
                self.session.observe_state(
                    outcome_agent_state(outcome),
                    revision,
                    event.occurred_at(),
                );
                self.latest_outcome = Some(outcome.clone());
                self.active_tool_calls.clear();
                self.pending_interactions.clear();
            }
        }

        self.record_event(event);
        Ok(ApplyOutcome::Applied)
    }

    /// Returns the aggregate retention settings.
    #[must_use]
    pub const fn config(&self) -> SessionAggregateConfig {
        self.config
    }

    /// Borrows the current complete session snapshot.
    #[must_use]
    pub const fn session(&self) -> &AgentSession {
        &self.session
    }

    /// Borrows the latest complete plan snapshot.
    #[must_use]
    pub const fn plan(&self) -> Option<&PlanSnapshot> {
        self.plan.as_ref()
    }

    /// Borrows the latest complete progress snapshot.
    #[must_use]
    pub const fn progress(&self) -> Option<&ProgressSnapshot> {
        self.progress.as_ref()
    }

    /// Borrows an active tool call by identifier.
    #[must_use]
    pub fn active_tool_call(&self, call_id: ToolCallId) -> Option<&ActiveToolCall> {
        self.active_tool_calls.get(&call_id)
    }

    /// Iterates over active tool calls in stable identifier order.
    pub fn active_tool_calls(&self) -> impl ExactSizeIterator<Item = &ActiveToolCall> {
        self.active_tool_calls.values()
    }

    /// Borrows a pending interaction by identifier.
    #[must_use]
    pub fn pending_interaction(
        &self,
        interaction_id: InteractionId,
    ) -> Option<&InteractionRequest> {
        self.pending_interactions.get(&interaction_id)
    }

    /// Iterates over pending interactions in stable identifier order.
    pub fn pending_interactions(&self) -> impl ExactSizeIterator<Item = &InteractionRequest> {
        self.pending_interactions.values()
    }

    /// Borrows the latest completed, failed, or cancelled run outcome.
    #[must_use]
    pub const fn latest_outcome(&self) -> Option<&SessionOutcome> {
        self.latest_outcome.as_ref()
    }

    /// Borrows the last successfully applied event.
    #[must_use]
    pub fn last_event(&self) -> &AgentEvent {
        self.last_event.as_ref()
    }

    /// Returns the last successfully applied event identifier.
    #[must_use]
    pub fn last_event_id(&self) -> EventId {
        self.last_event.id()
    }

    /// Returns the last successfully applied event sequence.
    #[must_use]
    pub fn last_sequence(&self) -> EventSequence {
        self.last_event.sequence()
    }

    /// Returns when the last successfully applied event occurred.
    #[must_use]
    pub fn last_event_at(&self) -> Timestamp {
        self.last_event.occurred_at()
    }

    /// Iterates over the bounded recent-event window from oldest to newest.
    pub fn recent_events(&self) -> impl ExactSizeIterator<Item = &AgentEvent> {
        self.recent_events.iter().map(Arc::as_ref)
    }

    fn validate_cursor(&self, event: &AgentEvent) -> Result<bool, ReduceError> {
        if event.session_id() != self.session.id() {
            return Err(ReduceError::SessionMismatch {
                expected: self.session.id(),
                actual: event.session_id(),
            });
        }

        if event.sequence() < self.last_sequence() {
            return Err(ReduceError::StaleSequence {
                last: self.last_sequence(),
                actual: event.sequence(),
            });
        }

        if event.sequence() == self.last_sequence() {
            if event == self.last_event.as_ref() {
                return Ok(true);
            }
            return Err(ReduceError::SequenceConflict {
                sequence: event.sequence(),
                expected_event_id: self.last_event_id(),
                actual_event_id: event.id(),
            });
        }

        let expected = self
            .last_sequence()
            .checked_next()
            .ok_or(ReduceError::SequenceExhausted)?;
        if event.sequence() != expected {
            return Err(ReduceError::SequenceGap {
                expected,
                actual: event.sequence(),
            });
        }

        Ok(false)
    }

    fn next_session_revision(&self) -> Result<Revision, ReduceError> {
        self.session
            .revision()
            .checked_next()
            .ok_or(ReduceError::SessionRevisionExhausted)
    }

    fn validate_snapshot_revision(
        snapshot: SnapshotKind,
        current: Option<Revision>,
        incoming: Revision,
    ) -> Result<(), ReduceError> {
        if let Some(current) = current
            && incoming <= current
        {
            return Err(ReduceError::StaleRevision {
                snapshot,
                current,
                incoming,
            });
        }
        Ok(())
    }

    fn apply_tool_activity(
        &mut self,
        activity: &ToolActivity,
        occurred_at: Timestamp,
    ) -> Result<(), ReduceError> {
        match activity {
            ToolActivity::Started {
                call_id,
                name,
                summary,
            } => {
                if self.active_tool_calls.contains_key(call_id) {
                    return Err(ReduceError::ToolAlreadyActive { call_id: *call_id });
                }
                self.active_tool_calls.insert(
                    *call_id,
                    ActiveToolCall::new(*call_id, name.clone(), summary.clone(), occurred_at),
                );
            }
            ToolActivity::Finished { call_id, .. } => {
                if !self.active_tool_calls.contains_key(call_id) {
                    return Err(ReduceError::ToolNotActive { call_id: *call_id });
                }
                let _ = self.active_tool_calls.remove(call_id);
            }
        }
        Ok(())
    }

    fn record_event(&mut self, event: AgentEvent) {
        let event = Arc::new(event);
        self.last_event = Arc::clone(&event);
        if self.config.recent_event_capacity() == 0 {
            return;
        }

        self.recent_events.push_back(event);
        while self.recent_events.len() > self.config.recent_event_capacity() {
            let _ = self.recent_events.pop_front();
        }
    }
}

fn outcome_agent_state(outcome: &SessionOutcome) -> AgentState {
    match outcome {
        SessionOutcome::Completed { .. } => AgentState::Completed,
        SessionOutcome::Failed { .. } => AgentState::Failed,
        SessionOutcome::Cancelled { .. } => AgentState::Cancelled,
    }
}
