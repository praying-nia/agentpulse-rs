//! Codex thread and turn mapping into AgentPulse domain events.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use agentpulse_bridge::{ProviderEventHandle, ProviderEventIngressError};
use agentpulse_core::{
    AgentEvent, AgentEventPayload, AgentMessage, AgentMessageLevel, AgentSession, AgentState,
    ConnectionState, EventId, EventSequence, NonEmptyText, ProviderId, Revision, SessionId,
    SessionOutcome, Timestamp, WorkspaceRef,
};
use serde_json::Value;

use crate::{
    CodexProviderSourceError,
    config::ConfiguredThread,
    status::{SharedStatus, lock_status},
};

const RECENT_NOTIFICATION_LIMIT: usize = 2_048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MappingDisposition {
    Mapped,
    ValidatedUnmapped,
}

#[derive(Debug)]
struct ThreadMapping {
    session_id: SessionId,
    next_sequence: EventSequence,
    state: AgentState,
    connection: ConnectionState,
    active_turn_id: Option<String>,
    last_message: Option<NonEmptyText>,
    last_final_message: Option<NonEmptyText>,
}

pub(crate) struct CodexEventMapper {
    provider_id: ProviderId,
    configured: BTreeMap<String, SessionId>,
    threads: BTreeMap<String, ThreadMapping>,
    recent_keys: BTreeSet<String>,
    recent_order: VecDeque<String>,
}

impl CodexEventMapper {
    pub(crate) fn new(provider_id: ProviderId, threads: &[ConfiguredThread]) -> Self {
        Self {
            provider_id,
            configured: threads
                .iter()
                .map(|thread| (thread.external_id.as_str().to_owned(), thread.session_id))
                .collect(),
            threads: BTreeMap::new(),
            recent_keys: BTreeSet::new(),
            recent_order: VecDeque::new(),
        }
    }

    pub(crate) fn resume_thread(
        &mut self,
        result: &Value,
        events: &ProviderEventHandle,
        status: &SharedStatus,
    ) -> Result<(), CodexProviderSourceError> {
        let thread = object_field(result, "thread")?;
        let thread_id = string_field(thread, "id")?;
        let configured_session = self.configured.get(thread_id).copied().ok_or_else(|| {
            CodexProviderSourceError::protocol(format!(
                "thread/resume returned unconfigured thread {thread_id}"
            ))
        })?;

        if self.threads.contains_key(thread_id) {
            let observed_at = timestamp_seconds(optional_i64_field(thread, "updatedAt")?)?;
            let (observed_state, observed_connection) =
                initial_status(object_field(thread, "status")?)?;
            let (current_state, current_connection) = self
                .threads
                .get(thread_id)
                .map(|mapping| (mapping.state, mapping.connection))
                .ok_or_else(|| CodexProviderSourceError::protocol("tracked thread disappeared"))?;
            if current_connection != observed_connection {
                self.publish_payload(
                    thread_id,
                    observed_at,
                    AgentEventPayload::ConnectionChanged(observed_connection),
                    events,
                    status,
                )?;
            }
            if current_state != observed_state {
                self.publish_payload(
                    thread_id,
                    observed_at,
                    AgentEventPayload::StateChanged(observed_state),
                    events,
                    status,
                )?;
            }
            if let Some(mapping) = self.threads.get_mut(thread_id) {
                mapping.active_turn_id = latest_in_progress_turn(thread)?;
                if mapping.active_turn_id.is_none() {
                    mapping.last_message = None;
                    mapping.last_final_message = None;
                }
            }
            return Ok(());
        }

        let created_at = timestamp_seconds(Some(i64_field(thread, "createdAt")?))?;
        let updated_at = timestamp_seconds(Some(i64_field(thread, "updatedAt")?))?;
        let (state, connection) = initial_status(object_field(thread, "status")?)?;
        let mut builder = AgentSession::builder(configured_session, self.provider_id, created_at)
            .external_id(agentpulse_core::ExternalId::new(thread_id.to_owned())?)
            .state(state)
            .connection_state(connection)
            .revision(Revision::FIRST, updated_at);

        if let Some(title) = thread_title(thread)? {
            builder = builder.title(title);
        }
        if let Some(cwd) = optional_non_empty_string(thread, "cwd")? {
            builder = builder.workspace(WorkspaceRef::new(cwd));
        }
        let session = builder.build()?;
        let event = AgentEvent::new(
            EventId::new(),
            configured_session,
            EventSequence::FIRST,
            updated_at,
            AgentEventPayload::SessionStarted(session),
        )?;
        publish_committed(events, event, status)?;
        let next_sequence = EventSequence::FIRST.checked_next().ok_or_else(|| {
            CodexProviderSourceError::protocol("event sequence exhausted after SessionStarted")
        })?;
        self.threads.insert(
            thread_id.to_owned(),
            ThreadMapping {
                session_id: configured_session,
                next_sequence,
                state,
                connection,
                active_turn_id: latest_in_progress_turn(thread)?,
                last_message: None,
                last_final_message: None,
            },
        );
        Ok(())
    }

    pub(crate) fn begin_reconnect(
        &mut self,
        events: &ProviderEventHandle,
        status: &SharedStatus,
    ) -> Result<(), CodexProviderSourceError> {
        let thread_ids = self.threads.keys().cloned().collect::<Vec<_>>();
        for thread_id in thread_ids {
            let needs_event = self
                .threads
                .get(&thread_id)
                .is_some_and(|mapping| mapping.connection != ConnectionState::Reconnecting);
            if needs_event {
                self.publish_payload(
                    &thread_id,
                    Timestamp::now_utc(),
                    AgentEventPayload::ConnectionChanged(ConnectionState::Reconnecting),
                    events,
                    status,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn disconnect_all(&mut self, events: &ProviderEventHandle, status: &SharedStatus) {
        let thread_ids = self.threads.keys().cloned().collect::<Vec<_>>();
        for thread_id in thread_ids {
            let needs_event = self
                .threads
                .get(&thread_id)
                .is_some_and(|mapping| mapping.connection != ConnectionState::Disconnected);
            if needs_event {
                let _ = self.publish_payload(
                    &thread_id,
                    Timestamp::now_utc(),
                    AgentEventPayload::ConnectionChanged(ConnectionState::Disconnected),
                    events,
                    status,
                );
            }
        }
    }

    pub(crate) fn notification(
        &mut self,
        method: &str,
        params: &Value,
        events: &ProviderEventHandle,
        status: &SharedStatus,
    ) -> Result<MappingDisposition, CodexProviderSourceError> {
        let Some(thread_id) = params.get("threadId").and_then(Value::as_str).or_else(|| {
            params
                .get("thread")
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
        }) else {
            return Ok(MappingDisposition::ValidatedUnmapped);
        };
        if !self.configured.contains_key(thread_id) || !self.threads.contains_key(thread_id) {
            return Ok(MappingDisposition::ValidatedUnmapped);
        }

        let key = notification_key(method, params, thread_id)?;
        if key
            .as_ref()
            .is_some_and(|key| self.recent_keys.contains(key))
        {
            return Ok(MappingDisposition::ValidatedUnmapped);
        }

        let disposition = match method {
            "thread/status/changed" => {
                self.status_changed(thread_id, object_field(params, "status")?, events, status)?
            }
            "thread/closed" => {
                let already_disconnected = self
                    .threads
                    .get(thread_id)
                    .is_some_and(|mapping| mapping.connection == ConnectionState::Disconnected);
                if already_disconnected {
                    MappingDisposition::ValidatedUnmapped
                } else {
                    self.publish_payload(
                        thread_id,
                        Timestamp::now_utc(),
                        AgentEventPayload::ConnectionChanged(ConnectionState::Disconnected),
                        events,
                        status,
                    )?;
                    MappingDisposition::Mapped
                }
            }
            "turn/started" => self.turn_started(thread_id, params, events, status)?,
            "item/completed" => self.item_completed(thread_id, params, events, status)?,
            "turn/completed" => self.turn_completed(thread_id, params, events, status)?,
            _ => MappingDisposition::ValidatedUnmapped,
        };

        if disposition == MappingDisposition::Mapped
            && let Some(key) = key
        {
            self.remember_key(key);
        }
        Ok(disposition)
    }

    fn status_changed(
        &mut self,
        thread_id: &str,
        status_value: &Value,
        events: &ProviderEventHandle,
        status: &SharedStatus,
    ) -> Result<MappingDisposition, CodexProviderSourceError> {
        let status_type = string_field(status_value, "type")?;
        if status_type == "notLoaded" {
            let already_disconnected = self
                .threads
                .get(thread_id)
                .is_some_and(|mapping| mapping.connection == ConnectionState::Disconnected);
            if already_disconnected {
                return Ok(MappingDisposition::ValidatedUnmapped);
            }
            self.publish_payload(
                thread_id,
                Timestamp::now_utc(),
                AgentEventPayload::ConnectionChanged(ConnectionState::Disconnected),
                events,
                status,
            )?;
            return Ok(MappingDisposition::Mapped);
        }

        let state = map_loaded_state(status_value)?;
        let (connection, current_state) = self
            .threads
            .get(thread_id)
            .map(|mapping| (mapping.connection, mapping.state))
            .ok_or_else(|| CodexProviderSourceError::protocol("tracked thread disappeared"))?;
        let mut mapped = false;
        if connection != ConnectionState::Connected {
            self.publish_payload(
                thread_id,
                Timestamp::now_utc(),
                AgentEventPayload::ConnectionChanged(ConnectionState::Connected),
                events,
                status,
            )?;
            mapped = true;
        }
        if current_state != state {
            self.publish_payload(
                thread_id,
                Timestamp::now_utc(),
                AgentEventPayload::StateChanged(state),
                events,
                status,
            )?;
            mapped = true;
        }
        Ok(if mapped {
            MappingDisposition::Mapped
        } else {
            MappingDisposition::ValidatedUnmapped
        })
    }

    fn turn_started(
        &mut self,
        thread_id: &str,
        params: &Value,
        events: &ProviderEventHandle,
        status: &SharedStatus,
    ) -> Result<MappingDisposition, CodexProviderSourceError> {
        let turn = object_field(params, "turn")?;
        let turn_id = string_field(turn, "id")?.to_owned();
        let timestamp = timestamp_seconds(optional_i64_field(turn, "startedAt")?)?;
        let mapping = self
            .threads
            .get_mut(thread_id)
            .ok_or_else(|| CodexProviderSourceError::protocol("tracked thread disappeared"))?;
        if mapping.active_turn_id.as_deref() == Some(&turn_id) {
            return Ok(MappingDisposition::ValidatedUnmapped);
        }
        if let Some(active_turn) = &mapping.active_turn_id {
            return Err(CodexProviderSourceError::protocol(format!(
                "turn {turn_id} started while {active_turn} is still active"
            )));
        }
        mapping.active_turn_id = Some(turn_id);
        mapping.last_message = None;
        mapping.last_final_message = None;
        let needs_state = mapping.state != AgentState::Running;
        if needs_state {
            self.publish_payload(
                thread_id,
                timestamp,
                AgentEventPayload::StateChanged(AgentState::Running),
                events,
                status,
            )?;
        }
        Ok(if needs_state {
            MappingDisposition::Mapped
        } else {
            MappingDisposition::ValidatedUnmapped
        })
    }

    fn item_completed(
        &mut self,
        thread_id: &str,
        params: &Value,
        events: &ProviderEventHandle,
        status: &SharedStatus,
    ) -> Result<MappingDisposition, CodexProviderSourceError> {
        let turn_id = string_field(params, "turnId")?;
        self.ensure_active_turn(thread_id, turn_id)?;
        let item = object_field(params, "item")?;
        if string_field(item, "type")? != "agentMessage" {
            return Ok(MappingDisposition::ValidatedUnmapped);
        }
        let text = string_field(item, "text")?;
        if text.trim().is_empty() {
            return Ok(MappingDisposition::ValidatedUnmapped);
        }
        let content = NonEmptyText::new(text.to_owned())?;
        let phase = item.get("phase").and_then(Value::as_str);
        let mapping = self
            .threads
            .get_mut(thread_id)
            .ok_or_else(|| CodexProviderSourceError::protocol("tracked thread disappeared"))?;
        mapping.last_message = Some(content.clone());
        if phase == Some("final_answer") {
            mapping.last_final_message = Some(content.clone());
        }
        let timestamp = timestamp_milliseconds(Some(i64_field(params, "completedAtMs")?))?;
        self.publish_payload(
            thread_id,
            timestamp,
            AgentEventPayload::Message(AgentMessage::new(AgentMessageLevel::Info, content)),
            events,
            status,
        )?;
        Ok(MappingDisposition::Mapped)
    }

    fn turn_completed(
        &mut self,
        thread_id: &str,
        params: &Value,
        events: &ProviderEventHandle,
        status: &SharedStatus,
    ) -> Result<MappingDisposition, CodexProviderSourceError> {
        let turn = object_field(params, "turn")?;
        let turn_id = string_field(turn, "id")?;
        self.ensure_active_turn(thread_id, turn_id)?;
        let turn_status = string_field(turn, "status")?;
        let timestamp = timestamp_seconds(optional_i64_field(turn, "completedAt")?)?;
        let mapping = self
            .threads
            .get(thread_id)
            .ok_or_else(|| CodexProviderSourceError::protocol("tracked thread disappeared"))?;
        let summary = mapping
            .last_final_message
            .clone()
            .or_else(|| mapping.last_message.clone());
        let outcome = match turn_status {
            "completed" => SessionOutcome::Completed { summary },
            "interrupted" => SessionOutcome::Cancelled { reason: None },
            "failed" => {
                let message = turn
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .filter(|message| !message.trim().is_empty())
                    .unwrap_or("Codex turn failed");
                SessionOutcome::Failed {
                    error: NonEmptyText::new(message.to_owned())?,
                }
            }
            "inProgress" => {
                return Err(CodexProviderSourceError::protocol(
                    "turn/completed carried inProgress status",
                ));
            }
            other => {
                return Err(CodexProviderSourceError::protocol(format!(
                    "unsupported turn status {other}"
                )));
            }
        };
        self.publish_payload(
            thread_id,
            timestamp,
            AgentEventPayload::SessionEnded(outcome),
            events,
            status,
        )?;
        if let Some(mapping) = self.threads.get_mut(thread_id) {
            mapping.active_turn_id = None;
        }
        Ok(MappingDisposition::Mapped)
    }

    fn ensure_active_turn(
        &mut self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), CodexProviderSourceError> {
        let mapping = self
            .threads
            .get_mut(thread_id)
            .ok_or_else(|| CodexProviderSourceError::protocol("tracked thread disappeared"))?;
        match mapping.active_turn_id.as_deref() {
            Some(active) if active != turn_id => Err(CodexProviderSourceError::protocol(format!(
                "notification for turn {turn_id} arrived while {active} is active"
            ))),
            Some(_) => Ok(()),
            None => {
                mapping.active_turn_id = Some(turn_id.to_owned());
                Ok(())
            }
        }
    }

    fn publish_payload(
        &mut self,
        thread_id: &str,
        occurred_at: Timestamp,
        payload: AgentEventPayload,
        events: &ProviderEventHandle,
        status: &SharedStatus,
    ) -> Result<(), CodexProviderSourceError> {
        let mapping = self
            .threads
            .get(thread_id)
            .ok_or_else(|| CodexProviderSourceError::protocol("tracked thread disappeared"))?;
        let payload_state = payload_state(&payload);
        let event = AgentEvent::new(
            EventId::new(),
            mapping.session_id,
            mapping.next_sequence,
            occurred_at,
            payload,
        )?;
        publish_committed(events, event, status)?;

        let mapping = self
            .threads
            .get_mut(thread_id)
            .ok_or_else(|| CodexProviderSourceError::protocol("tracked thread disappeared"))?;
        mapping.next_sequence = mapping.next_sequence.checked_next().ok_or_else(|| {
            CodexProviderSourceError::protocol(format!(
                "event sequence exhausted for thread {thread_id}"
            ))
        })?;
        match payload_state {
            PayloadState::Execution(state) => mapping.state = state,
            PayloadState::Connection(connection) => mapping.connection = connection,
            PayloadState::None => {}
        }
        Ok(())
    }

    fn remember_key(&mut self, key: String) {
        self.recent_keys.insert(key.clone());
        self.recent_order.push_back(key);
        if self.recent_order.len() > RECENT_NOTIFICATION_LIMIT
            && let Some(expired) = self.recent_order.pop_front()
        {
            self.recent_keys.remove(&expired);
        }
    }
}

enum PayloadState {
    Execution(AgentState),
    Connection(ConnectionState),
    None,
}

fn payload_state(payload: &AgentEventPayload) -> PayloadState {
    match payload {
        AgentEventPayload::StateChanged(state) => PayloadState::Execution(*state),
        AgentEventPayload::ConnectionChanged(connection) => PayloadState::Connection(*connection),
        AgentEventPayload::SessionEnded(SessionOutcome::Completed { .. }) => {
            PayloadState::Execution(AgentState::Completed)
        }
        AgentEventPayload::SessionEnded(SessionOutcome::Failed { .. }) => {
            PayloadState::Execution(AgentState::Failed)
        }
        AgentEventPayload::SessionEnded(SessionOutcome::Cancelled { .. }) => {
            PayloadState::Execution(AgentState::Cancelled)
        }
        _ => PayloadState::None,
    }
}

fn publish_committed(
    events: &ProviderEventHandle,
    event: AgentEvent,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    match events.publish_event(event) {
        Ok(_) => {
            lock_status(status).mapped_events += 1;
            Ok(())
        }
        Err(error) => {
            let committed_failures = match &error {
                ProviderEventIngressError::Bridge(bridge_error) => {
                    bridge_error.report().map(|report| {
                        report
                            .deliveries()
                            .iter()
                            .filter(|delivery| !delivery.is_delivered())
                            .count() as u64
                    })
                }
                _ => None,
            };
            if let Some(failures) = committed_failures {
                let mut status = lock_status(status);
                status.mapped_events += 1;
                status.channel_delivery_failures += failures;
                Ok(())
            } else {
                Err(CodexProviderSourceError::EventIngress {
                    message: error.to_string(),
                })
            }
        }
    }
}

fn initial_status(
    status: &Value,
) -> Result<(AgentState, ConnectionState), CodexProviderSourceError> {
    match string_field(status, "type")? {
        "notLoaded" => Ok((AgentState::Initializing, ConnectionState::Disconnected)),
        _ => Ok((map_loaded_state(status)?, ConnectionState::Connected)),
    }
}

fn map_loaded_state(status: &Value) -> Result<AgentState, CodexProviderSourceError> {
    match string_field(status, "type")? {
        "idle" => Ok(AgentState::Idle),
        "systemError" => Ok(AgentState::Failed),
        "active" => {
            let waiting = status
                .get("activeFlags")
                .and_then(Value::as_array)
                .is_some_and(|flags| {
                    flags.iter().any(|flag| {
                        matches!(
                            flag.as_str(),
                            Some("waitingOnApproval" | "waitingOnUserInput")
                        )
                    })
                });
            Ok(if waiting {
                AgentState::WaitingForInteraction
            } else {
                AgentState::Running
            })
        }
        "notLoaded" => Err(CodexProviderSourceError::protocol(
            "notLoaded status has no connected execution state",
        )),
        other => Err(CodexProviderSourceError::protocol(format!(
            "unsupported thread status {other}"
        ))),
    }
}

fn thread_title(thread: &Value) -> Result<Option<NonEmptyText>, CodexProviderSourceError> {
    optional_non_empty_string(thread, "name").and_then(|name| match name {
        Some(name) => Ok(Some(name)),
        None => optional_non_empty_string(thread, "preview"),
    })
}

fn latest_in_progress_turn(thread: &Value) -> Result<Option<String>, CodexProviderSourceError> {
    let turns = thread
        .get("turns")
        .and_then(Value::as_array)
        .ok_or_else(|| CodexProviderSourceError::protocol("turns must be an array"))?;
    Ok(turns.iter().rev().find_map(|turn| {
        (turn.get("status").and_then(Value::as_str) == Some("inProgress"))
            .then(|| {
                turn.get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .flatten()
    }))
}

fn optional_non_empty_string(
    value: &Value,
    field: &'static str,
) -> Result<Option<NonEmptyText>, CodexProviderSourceError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => NonEmptyText::new(text.clone())
            .map(Some)
            .map_err(Into::into),
        Some(_) => Err(CodexProviderSourceError::protocol(format!(
            "{field} must be a string or null"
        ))),
    }
}

fn notification_key(
    method: &str,
    params: &Value,
    thread_id: &str,
) -> Result<Option<String>, CodexProviderSourceError> {
    let id = match method {
        "turn/started" | "turn/completed" => params
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str),
        "item/completed" => params
            .get("item")
            .and_then(|item| item.get("id"))
            .and_then(Value::as_str),
        _ => None,
    };
    match id {
        Some(id) => Ok(Some(format!("{method}:{thread_id}:{id}"))),
        None if matches!(method, "turn/started" | "turn/completed" | "item/completed") => {
            Err(CodexProviderSourceError::protocol(format!(
                "{method} notification has no item or turn ID"
            )))
        }
        None => Ok(None),
    }
}

fn object_field<'a>(
    value: &'a Value,
    field: &'static str,
) -> Result<&'a Value, CodexProviderSourceError> {
    value
        .get(field)
        .filter(|value| value.is_object())
        .ok_or_else(|| CodexProviderSourceError::protocol(format!("{field} must be an object")))
}

fn string_field<'a>(
    value: &'a Value,
    field: &'static str,
) -> Result<&'a str, CodexProviderSourceError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| CodexProviderSourceError::protocol(format!("{field} must be a string")))
}

fn i64_field(value: &Value, field: &'static str) -> Result<i64, CodexProviderSourceError> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| CodexProviderSourceError::protocol(format!("{field} must be an int64")))
}

fn optional_i64_field(
    value: &Value,
    field: &'static str,
) -> Result<Option<i64>, CodexProviderSourceError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or_else(|| {
            CodexProviderSourceError::protocol(format!("{field} must be an int64 or null"))
        }),
    }
}

fn timestamp_seconds(value: Option<i64>) -> Result<Timestamp, CodexProviderSourceError> {
    timestamp_with_multiplier(value, 1_000_000_000)
}

fn timestamp_milliseconds(value: Option<i64>) -> Result<Timestamp, CodexProviderSourceError> {
    timestamp_with_multiplier(value, 1_000_000)
}

fn timestamp_with_multiplier(
    value: Option<i64>,
    multiplier: i128,
) -> Result<Timestamp, CodexProviderSourceError> {
    let Some(value) = value else {
        return Ok(Timestamp::now_utc());
    };
    let nanos = i128::from(value).checked_mul(multiplier).ok_or_else(|| {
        CodexProviderSourceError::protocol("Codex timestamp overflowed Unix nanoseconds")
    })?;
    Timestamp::from_unix_timestamp_nanos(nanos).map_err(Into::into)
}

impl From<agentpulse_core::DomainError> for CodexProviderSourceError {
    fn from(error: agentpulse_core::DomainError) -> Self {
        Self::protocol(format!("domain mapping rejected Codex data: {error}"))
    }
}
