//! Runtime Host lifecycle and controlled-ingress contracts.

use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use agentpulse_bridge::{
    AdapterLifecyclePhase, AdapterLifecycleState, ChannelActionHandle, ChannelActionIngressError,
    ChannelActionSink, ChannelActionSource, ChannelPort, ChannelSubscriptionScope,
    EndpointRegistrationError, ProviderEventHandle, ProviderEventIngressError,
    ProviderEventOutcome, ProviderEventSink, ProviderEventSource, ProviderPort, RuntimeEndpointId,
    RuntimeHost, RuntimeHostState, RuntimeLifecycleError, RuntimeLifecycleOperation,
    RuntimeLifecycleOutcome, RuntimeRegistrationError, SubscribeOutcome,
};
use agentpulse_core::{
    AgentCommand, AgentCommandPayload, AgentEvent, AgentEventPayload, AgentMessage,
    AgentMessageLevel, AgentSession, ChannelCapabilities, ChannelDescriptor, ChannelEventRoute,
    ChannelId, ChannelKind, CommandId, DomainError, EventId, EventSequence, InteractionResponse,
    NonEmptyText, ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind, SessionId,
    Timestamp,
};

type TestResult = Result<(), Box<dyn Error>>;
type LifecycleLog = Arc<Mutex<Vec<&'static str>>>;
type ProviderFixture = (
    FakeProviderPort,
    Arc<Mutex<ProviderPortState>>,
    FakeProviderSource,
    Arc<Mutex<ProviderSourceState>>,
);
type ChannelFixture = (
    FakeChannelPort,
    Arc<Mutex<ChannelPortState>>,
    FakeChannelSource,
    Arc<Mutex<ChannelSourceState>>,
);

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn text(value: &str) -> Result<NonEmptyText, DomainError> {
    NonEmptyText::new(value)
}

fn timestamp(value: i128) -> Result<Timestamp, DomainError> {
    Timestamp::from_unix_timestamp_nanos(value)
}

fn provider_descriptor(provider_id: ProviderId) -> Result<ProviderDescriptor, DomainError> {
    Ok(ProviderDescriptor::new(
        provider_id,
        ProviderKind::new("runtime-test")?,
        text("Runtime Provider")?,
        ProviderCapabilities::SESSION_STATE | ProviderCapabilities::CANCEL,
    ))
}

fn channel_descriptor(channel_id: ChannelId) -> Result<ChannelDescriptor, DomainError> {
    Ok(ChannelDescriptor::new(
        channel_id,
        ChannelKind::new("runtime-test")?,
        text("Runtime Channel")?,
        ChannelCapabilities::SESSION_VIEW | ChannelCapabilities::REMOTE_COMMAND,
    ))
}

fn session(provider_id: ProviderId, session_id: SessionId) -> Result<AgentSession, DomainError> {
    AgentSession::builder(session_id, provider_id, timestamp(100)?).build()
}

fn event(
    session_id: SessionId,
    sequence: u64,
    payload: AgentEventPayload,
) -> Result<AgentEvent, DomainError> {
    AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::new(sequence)?,
        timestamp(100 + i128::from(sequence))?,
        payload,
    )
}

fn initial_event(session: AgentSession) -> Result<AgentEvent, DomainError> {
    AgentEvent::new(
        EventId::new(),
        session.id(),
        EventSequence::FIRST,
        session.created_at(),
        AgentEventPayload::SessionStarted(session),
    )
}

fn message_event(
    session_id: SessionId,
    sequence: u64,
    value: &str,
) -> Result<AgentEvent, DomainError> {
    event(
        session_id,
        sequence,
        AgentEventPayload::Message(AgentMessage::new(AgentMessageLevel::Info, text(value)?)),
    )
}

fn cancel_command(
    session_id: SessionId,
    channel_id: ChannelId,
) -> Result<AgentCommand, DomainError> {
    Ok(AgentCommand::new(
        CommandId::new(),
        session_id,
        channel_id,
        timestamp(150)?,
        AgentCommandPayload::CancelSession { reason: None },
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProviderRejected(&'static str);

impl fmt::Display for ProviderRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for ProviderRejected {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChannelRejected(&'static str);

impl fmt::Display for ChannelRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for ChannelRejected {}

#[derive(Default)]
struct ProviderPortState {
    responses: Vec<InteractionResponse>,
    commands: Vec<AgentCommand>,
}

struct FakeProviderPort {
    descriptor: ProviderDescriptor,
    state: Arc<Mutex<ProviderPortState>>,
    log: LifecycleLog,
}

impl ProviderPort for FakeProviderPort {
    type Error = ProviderRejected;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        locked(&self.state).responses.push(response);
        Ok(())
    }

    fn accept_command(&mut self, command: AgentCommand) -> Result<(), Self::Error> {
        locked(&self.state).commands.push(command);
        Ok(())
    }
}

impl Drop for FakeProviderPort {
    fn drop(&mut self) {
        locked(&self.log).push("drop provider port");
    }
}

#[derive(Default)]
struct ChannelPortState {
    events: Vec<(AgentEvent, ChannelEventRoute)>,
    sessions: Vec<AgentSession>,
    reentrant_command: Option<AgentCommand>,
    reentrant_was_rejected: Option<bool>,
}

struct FakeChannelPort {
    descriptor: ChannelDescriptor,
    state: Arc<Mutex<ChannelPortState>>,
    source_state: Arc<Mutex<ChannelSourceState>>,
    log: LifecycleLog,
}

impl ChannelPort for FakeChannelPort {
    type Error = ChannelRejected;

    fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), Self::Error> {
        let reentrant_command = {
            let mut state = locked(&self.state);
            state.events.push((event, route));
            state.reentrant_command.take()
        };

        if let Some(command) = reentrant_command {
            let handle = locked(&self.source_state).handles.last().cloned();
            let rejected = handle.is_some_and(|handle| {
                matches!(
                    handle.submit_command(command),
                    Err(ChannelActionIngressError::BridgeAccess(_))
                )
            });
            locked(&self.state).reentrant_was_rejected = Some(rejected);
        }
        Ok(())
    }

    fn deliver_session(&mut self, session: AgentSession) -> Result<(), Self::Error> {
        locked(&self.state).sessions.push(session);
        Ok(())
    }
}

impl Drop for FakeChannelPort {
    fn drop(&mut self) {
        locked(&self.log).push("drop channel port");
    }
}

#[derive(Default)]
struct ProviderSourceState {
    handles: Vec<ProviderEventHandle>,
    starts: usize,
    stops: usize,
    fail_starts_remaining: usize,
    fail_stops_remaining: usize,
    event_on_start: Option<AgentEvent>,
    start_event_applied: bool,
    stop_probe: Option<AgentEvent>,
    stop_saw_inactive: Vec<bool>,
}

struct FakeProviderSource {
    state: Arc<Mutex<ProviderSourceState>>,
    log: LifecycleLog,
}

impl ProviderEventSource for FakeProviderSource {
    type Error = ProviderRejected;

    fn start(&mut self, events: ProviderEventHandle) -> Result<(), Self::Error> {
        locked(&self.log).push("start provider");
        let (event_on_start, should_fail) = {
            let mut state = locked(&self.state);
            state.starts += 1;
            state.handles.push(events.clone());
            let event = state.event_on_start.take();
            let should_fail = state.fail_starts_remaining > 0;
            state.fail_starts_remaining = state.fail_starts_remaining.saturating_sub(1);
            (event, should_fail)
        };

        if let Some(event) = event_on_start {
            let report = events
                .publish_event(event)
                .map_err(|_| ProviderRejected("start Event ingress failed"))?;
            locked(&self.state).start_event_applied =
                report.outcome() == ProviderEventOutcome::Applied;
        }
        if should_fail {
            Err(ProviderRejected("Provider Source start rejected"))
        } else {
            Ok(())
        }
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        locked(&self.log).push("stop provider");
        let (handle, probe, should_fail) = {
            let mut state = locked(&self.state);
            state.stops += 1;
            let handle = state.handles.last().cloned();
            let probe = state.stop_probe.clone();
            let should_fail = state.fail_stops_remaining > 0;
            state.fail_stops_remaining = state.fail_stops_remaining.saturating_sub(1);
            (handle, probe, should_fail)
        };
        if let (Some(handle), Some(probe)) = (handle, probe) {
            let inactive = matches!(
                handle.publish_event(probe),
                Err(ProviderEventIngressError::Inactive { .. })
            );
            locked(&self.state).stop_saw_inactive.push(inactive);
        }
        if should_fail {
            Err(ProviderRejected("Provider Source stop rejected"))
        } else {
            Ok(())
        }
    }
}

impl Drop for FakeProviderSource {
    fn drop(&mut self) {
        locked(&self.log).push("drop provider source");
    }
}

#[derive(Default)]
struct ChannelSourceState {
    handles: Vec<ChannelActionHandle>,
    starts: usize,
    stops: usize,
    fail_starts_remaining: usize,
    fail_stops_remaining: usize,
    stop_probe: Option<AgentCommand>,
    stop_saw_inactive: Vec<bool>,
}

struct FakeChannelSource {
    state: Arc<Mutex<ChannelSourceState>>,
    log: LifecycleLog,
    scope: ChannelSubscriptionScope,
}

impl ChannelActionSource for FakeChannelSource {
    type Error = ChannelRejected;

    fn start(&mut self, actions: ChannelActionHandle) -> Result<(), Self::Error> {
        locked(&self.log).push("start channel");
        let should_fail = {
            let mut state = locked(&self.state);
            state.starts += 1;
            state.handles.push(actions);
            let should_fail = state.fail_starts_remaining > 0;
            state.fail_starts_remaining = state.fail_starts_remaining.saturating_sub(1);
            should_fail
        };
        if should_fail {
            Err(ChannelRejected("Channel Source start rejected"))
        } else {
            Ok(())
        }
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        locked(&self.log).push("stop channel");
        let (handle, probe, should_fail) = {
            let mut state = locked(&self.state);
            state.stops += 1;
            let handle = state.handles.last().cloned();
            let probe = state.stop_probe.clone();
            let should_fail = state.fail_stops_remaining > 0;
            state.fail_stops_remaining = state.fail_stops_remaining.saturating_sub(1);
            (handle, probe, should_fail)
        };
        if let (Some(handle), Some(probe)) = (handle, probe) {
            let inactive = matches!(
                handle.submit_command(probe),
                Err(ChannelActionIngressError::Inactive { .. })
            );
            locked(&self.state).stop_saw_inactive.push(inactive);
        }
        if should_fail {
            Err(ChannelRejected("Channel Source stop rejected"))
        } else {
            Ok(())
        }
    }

    fn subscription_scope(&self) -> ChannelSubscriptionScope {
        self.scope
    }
}

impl Drop for FakeChannelSource {
    fn drop(&mut self) {
        locked(&self.log).push("drop channel source");
    }
}

fn fake_provider(
    provider_id: ProviderId,
    log: &LifecycleLog,
) -> Result<ProviderFixture, DomainError> {
    let port_state = Arc::new(Mutex::new(ProviderPortState::default()));
    let source_state = Arc::new(Mutex::new(ProviderSourceState::default()));
    Ok((
        FakeProviderPort {
            descriptor: provider_descriptor(provider_id)?,
            state: Arc::clone(&port_state),
            log: Arc::clone(log),
        },
        port_state,
        FakeProviderSource {
            state: Arc::clone(&source_state),
            log: Arc::clone(log),
        },
        source_state,
    ))
}

fn fake_channel(channel_id: ChannelId, log: &LifecycleLog) -> Result<ChannelFixture, DomainError> {
    let port_state = Arc::new(Mutex::new(ChannelPortState::default()));
    let source_state = Arc::new(Mutex::new(ChannelSourceState::default()));
    Ok((
        FakeChannelPort {
            descriptor: channel_descriptor(channel_id)?,
            state: Arc::clone(&port_state),
            source_state: Arc::clone(&source_state),
            log: Arc::clone(log),
        },
        port_state,
        FakeChannelSource {
            state: Arc::clone(&source_state),
            log: Arc::clone(log),
            scope: ChannelSubscriptionScope::Persistent,
        },
        source_state,
    ))
}

fn last_provider_handle(
    state: &Arc<Mutex<ProviderSourceState>>,
) -> Result<ProviderEventHandle, ProviderRejected> {
    locked(state)
        .handles
        .last()
        .cloned()
        .ok_or(ProviderRejected("missing Provider Event handle"))
}

fn last_channel_handle(
    state: &Arc<Mutex<ChannelSourceState>>,
) -> Result<ChannelActionHandle, ChannelRejected> {
    locked(state)
        .handles
        .last()
        .cloned()
        .ok_or(ChannelRejected("missing Channel Action handle"))
}

#[test]
fn controlled_ingress_drives_bridge_and_survives_restart_without_stale_handles() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let (provider, provider_port, provider_source, provider_runtime) =
        fake_provider(provider_id, &log)?;
    let (channel, channel_port, channel_source, channel_runtime) = fake_channel(channel_id, &log)?;
    let mut host = RuntimeHost::new();
    host.register_provider(provider, provider_source)?;
    host.register_channel(channel, channel_source)?;

    assert_eq!(
        host.adapter_states().collect::<Vec<_>>(),
        vec![
            (
                RuntimeEndpointId::Provider(provider_id),
                AdapterLifecycleState::Stopped,
            ),
            (
                RuntimeEndpointId::Channel(channel_id),
                AdapterLifecycleState::Stopped,
            ),
        ]
    );
    let start = host.start()?;
    assert_eq!(start.operation(), RuntimeLifecycleOperation::Start);
    assert_eq!(start.outcome(), RuntimeLifecycleOutcome::Started);
    assert_eq!(
        start
            .adapters()
            .iter()
            .map(|result| result.endpoint())
            .collect::<Vec<_>>(),
        vec![
            RuntimeEndpointId::Provider(provider_id),
            RuntimeEndpointId::Channel(channel_id),
        ]
    );
    assert_eq!(locked(&log).as_slice(), ["start provider", "start channel"]);

    let old_provider_handle = last_provider_handle(&provider_runtime)?;
    let old_channel_handle = last_channel_handle(&channel_runtime)?;
    let report =
        old_provider_handle.publish_event(initial_event(session(provider_id, session_id)?)?)?;
    assert_eq!(report.outcome(), ProviderEventOutcome::Applied);
    assert_eq!(
        host.subscribe(channel_id, session_id)?,
        SubscribeOutcome::Subscribed {
            session_view_delivered: true,
            baseline_sequence: EventSequence::FIRST,
        }
    );
    old_channel_handle.submit_command(cancel_command(session_id, channel_id)?)?;
    assert_eq!(locked(&provider_port).commands.len(), 1);

    let report = old_provider_handle.publish_event(message_event(session_id, 2, "running")?)?;
    assert_eq!(report.outcome(), ProviderEventOutcome::Applied);
    locked(&channel_port).reentrant_command = Some(cancel_command(session_id, channel_id)?);
    let _ = old_provider_handle.publish_event(message_event(session_id, 3, "re-enter")?)?;
    assert_eq!(locked(&channel_port).reentrant_was_rejected, Some(true));
    assert_eq!(locked(&provider_port).commands.len(), 1);

    let duplicate_start = host.start()?;
    assert_eq!(
        duplicate_start.outcome(),
        RuntimeLifecycleOutcome::AlreadyStarted
    );
    assert!(duplicate_start.adapters().is_empty());
    assert_eq!(locked(&provider_runtime).starts, 1);
    assert_eq!(locked(&channel_runtime).starts, 1);

    let stop = host.stop()?;
    assert_eq!(stop.operation(), RuntimeLifecycleOperation::Stop);
    assert_eq!(stop.outcome(), RuntimeLifecycleOutcome::Stopped);
    assert_eq!(
        stop.adapters()
            .iter()
            .map(|result| result.endpoint())
            .collect::<Vec<_>>(),
        vec![
            RuntimeEndpointId::Channel(channel_id),
            RuntimeEndpointId::Provider(provider_id),
        ]
    );
    assert!(matches!(
        old_provider_handle.publish_event(message_event(session_id, 4, "stopped")?),
        Err(ProviderEventIngressError::Inactive { .. })
    ));
    assert!(matches!(
        old_channel_handle.submit_command(cancel_command(session_id, channel_id)?),
        Err(ChannelActionIngressError::Inactive { .. })
    ));
    assert!(host.inspect_bridge(|bridge| {
        bridge.session_aggregate(session_id).is_some()
            && bridge.is_subscribed(channel_id, session_id)
    })?);

    let restart = host.start()?;
    assert_eq!(restart.outcome(), RuntimeLifecycleOutcome::Started);
    let new_provider_handle = last_provider_handle(&provider_runtime)?;
    let new_channel_handle = last_channel_handle(&channel_runtime)?;
    assert!(matches!(
        old_provider_handle.publish_event(message_event(session_id, 4, "stale")?),
        Err(ProviderEventIngressError::Inactive { .. })
    ));
    let report = new_provider_handle.publish_event(message_event(session_id, 4, "fresh")?)?;
    assert_eq!(report.outcome(), ProviderEventOutcome::Applied);
    new_channel_handle.submit_command(cancel_command(session_id, channel_id)?)?;
    assert_eq!(locked(&provider_port).commands.len(), 2);
    assert_eq!(locked(&channel_port).events.len(), 3);
    let _ = host.stop()?;
    Ok(())
}

#[test]
fn start_failures_are_isolated_reported_and_do_not_roll_back_bridge_state() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let (provider, _provider_port, provider_source, provider_runtime) =
        fake_provider(provider_id, &log)?;
    let (channel, _channel_port, channel_source, channel_runtime) = fake_channel(channel_id, &log)?;
    {
        let mut state = locked(&provider_runtime);
        state.fail_starts_remaining = 1;
        state.event_on_start = Some(initial_event(session(provider_id, session_id)?)?);
        state.stop_probe = Some(message_event(session_id, 2, "stop probe")?);
    }
    let mut host = RuntimeHost::new();
    host.register_provider(provider, provider_source)?;
    host.register_channel(channel, channel_source)?;

    let error = match host.start() {
        Ok(_) => return Err(Box::new(ProviderRejected("expected start failure"))),
        Err(error) => error,
    };
    let report = error
        .report()
        .ok_or(ProviderRejected("missing lifecycle failure report"))?;
    assert_eq!(report.outcome(), RuntimeLifecycleOutcome::Started);
    assert_eq!(report.adapters().len(), 2);
    assert!(matches!(
        report.adapters()[0].error().map(|error| error.phase()),
        Some(AdapterLifecyclePhase::Start)
    ));
    assert!(report.adapters()[1].is_success());
    let adapter_error = error
        .source()
        .ok_or(ProviderRejected("missing Adapter lifecycle source"))?;
    assert_eq!(
        adapter_error
            .source()
            .ok_or(ProviderRejected("missing original Provider source"))?
            .to_string(),
        "Provider Source start rejected"
    );
    assert_eq!(host.state(), RuntimeHostState::Started);
    assert_eq!(
        host.adapter_state(RuntimeEndpointId::Provider(provider_id)),
        Some(AdapterLifecycleState::StartFailed)
    );
    assert_eq!(
        host.adapter_state(RuntimeEndpointId::Channel(channel_id)),
        Some(AdapterLifecycleState::Running)
    );
    assert!(locked(&provider_runtime).start_event_applied);
    assert!(host.inspect_bridge(|bridge| bridge.session_aggregate(session_id).is_some())?);
    assert!(matches!(
        last_provider_handle(&provider_runtime)?
            .publish_event(message_event(session_id, 2, "inactive")?),
        Err(ProviderEventIngressError::Inactive { .. })
    ));

    let stop = host.stop()?;
    assert_eq!(stop.outcome(), RuntimeLifecycleOutcome::Stopped);
    assert_eq!(locked(&provider_runtime).stops, 1);
    assert_eq!(locked(&channel_runtime).stops, 1);
    assert_eq!(locked(&provider_runtime).stop_saw_inactive, vec![true]);
    assert_eq!(
        locked(&log)
            .iter()
            .copied()
            .filter(|entry| entry.starts_with("stop"))
            .collect::<Vec<_>>(),
        vec!["stop channel", "stop provider"]
    );
    Ok(())
}

#[test]
fn stop_failures_are_isolated_and_only_failed_adapters_are_retried() -> TestResult {
    let first_provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let second_provider_id = ProviderId::new();
    let session_id = SessionId::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let (first_provider, _, first_source, first_runtime) = fake_provider(first_provider_id, &log)?;
    let (channel, _, channel_source, channel_runtime) = fake_channel(channel_id, &log)?;
    let (second_provider, _, second_source, second_runtime) =
        fake_provider(second_provider_id, &log)?;
    locked(&first_runtime).stop_probe = Some(initial_event(session(
        first_provider_id,
        SessionId::new(),
    )?)?);
    locked(&channel_runtime).stop_probe = Some(cancel_command(session_id, channel_id)?);
    locked(&channel_runtime).fail_stops_remaining = 1;
    locked(&second_runtime).stop_probe = Some(initial_event(session(
        second_provider_id,
        SessionId::new(),
    )?)?);

    let mut host = RuntimeHost::new();
    host.register_provider(first_provider, first_source)?;
    host.register_channel(channel, channel_source)?;
    host.register_provider(second_provider, second_source)?;
    let _ = host.start()?;
    locked(&log).clear();

    let error = match host.stop() {
        Ok(_) => return Err(Box::new(ChannelRejected("expected stop failure"))),
        Err(error) => error,
    };
    let report = error
        .report()
        .ok_or(ChannelRejected("missing stop failure report"))?;
    assert_eq!(report.outcome(), RuntimeLifecycleOutcome::StopFailed);
    assert_eq!(
        report
            .adapters()
            .iter()
            .map(|result| result.endpoint())
            .collect::<Vec<_>>(),
        vec![
            RuntimeEndpointId::Provider(second_provider_id),
            RuntimeEndpointId::Channel(channel_id),
            RuntimeEndpointId::Provider(first_provider_id),
        ]
    );
    assert_eq!(
        locked(&log).as_slice(),
        ["stop provider", "stop channel", "stop provider"]
    );
    assert_eq!(host.state(), RuntimeHostState::StopFailed);
    assert_eq!(locked(&first_runtime).stop_saw_inactive, vec![true]);
    assert_eq!(locked(&channel_runtime).stop_saw_inactive, vec![true]);
    assert_eq!(locked(&second_runtime).stop_saw_inactive, vec![true]);
    assert!(matches!(
        host.start(),
        Err(RuntimeLifecycleError::CleanupRequired)
    ));

    locked(&log).clear();
    let retry = host.stop()?;
    assert_eq!(retry.outcome(), RuntimeLifecycleOutcome::Stopped);
    assert_eq!(locked(&log).as_slice(), ["stop channel"]);
    assert_eq!(locked(&first_runtime).stops, 1);
    assert_eq!(locked(&channel_runtime).stops, 2);
    assert_eq!(locked(&second_runtime).stops, 1);
    let duplicate = host.stop()?;
    assert_eq!(duplicate.outcome(), RuntimeLifecycleOutcome::AlreadyStopped);
    assert!(duplicate.adapters().is_empty());
    Ok(())
}

#[test]
fn registration_identity_and_drop_cleanup_remain_controlled() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let (provider, _, provider_source, provider_runtime) = fake_provider(provider_id, &log)?;
    let (channel, _, channel_source, channel_runtime) = fake_channel(channel_id, &log)?;
    let mut host = RuntimeHost::new();
    host.register_provider(provider, provider_source)?;

    let (duplicate_provider, _, duplicate_source, _) = fake_provider(provider_id, &log)?;
    assert!(matches!(
        host.register_provider(duplicate_provider, duplicate_source),
        Err(RuntimeRegistrationError::Endpoint(
            EndpointRegistrationError::ProviderAlreadyRegistered { .. }
        ))
    ));
    host.register_channel(channel, channel_source)?;
    let _ = host.start()?;
    let (late_channel, _, late_source, _) = fake_channel(ChannelId::new(), &log)?;
    assert!(matches!(
        host.register_channel(late_channel, late_source),
        Err(RuntimeRegistrationError::HostNotStopped {
            state: RuntimeHostState::Started
        })
    ));

    let mut provider_handle = last_provider_handle(&provider_runtime)?;
    let wrong_provider_id = ProviderId::new();
    assert!(matches!(
        ProviderEventSink::publish_event(
            &mut provider_handle,
            wrong_provider_id,
            initial_event(session(provider_id, session_id)?)?,
        ),
        Err(ProviderEventIngressError::ProviderMismatch { .. })
    ));
    let mut channel_handle = last_channel_handle(&channel_runtime)?;
    assert!(matches!(
        ChannelActionSink::submit_command(
            &mut channel_handle,
            ChannelId::new(),
            cancel_command(session_id, channel_id)?,
        ),
        Err(ChannelActionIngressError::ChannelMismatch { .. })
    ));

    locked(&log).clear();
    drop(host);
    assert_eq!(
        locked(&log).as_slice(),
        [
            "stop channel",
            "stop provider",
            "drop channel source",
            "drop provider source",
            "drop provider port",
            "drop channel port",
        ]
    );
    assert!(matches!(
        provider_handle.publish_event(initial_event(session(provider_id, SessionId::new())?)?),
        Err(ProviderEventIngressError::HostDropped { .. })
    ));
    assert!(matches!(
        channel_handle.submit_command(cancel_command(session_id, channel_id)?),
        Err(ChannelActionIngressError::HostDropped { .. })
    ));
    Ok(())
}

#[test]
fn channel_handle_discovers_sessions_and_source_generation_stop_clears_subscriptions() -> TestResult
{
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let (provider, _, provider_source, provider_runtime) = fake_provider(provider_id, &log)?;
    let (channel, channel_port, mut channel_source, channel_runtime) =
        fake_channel(channel_id, &log)?;
    channel_source.scope = ChannelSubscriptionScope::SourceGeneration;

    let mut host = RuntimeHost::new();
    host.register_provider(provider, provider_source)?;
    host.register_channel(channel, channel_source)?;
    let _ = host.start()?;
    let provider_handle = last_provider_handle(&provider_runtime)?;
    let _ = provider_handle.publish_event(initial_event(session(provider_id, session_id)?)?)?;
    let channel_handle = last_channel_handle(&channel_runtime)?;

    let discovery = channel_handle.discovery_snapshot()?;
    assert_eq!(discovery.providers().len(), 1);
    assert_eq!(discovery.providers()[0].id(), provider_id);
    assert_eq!(discovery.sessions().len(), 1);
    assert_eq!(discovery.sessions()[0].session().id(), session_id);
    assert_eq!(
        discovery.sessions()[0].last_sequence(),
        EventSequence::FIRST
    );
    assert_eq!(
        channel_handle.subscribe(session_id)?,
        SubscribeOutcome::Subscribed {
            session_view_delivered: true,
            baseline_sequence: EventSequence::FIRST,
        }
    );
    assert_eq!(locked(&channel_port).sessions.len(), 1);
    assert!(host.inspect_bridge(|bridge| bridge.is_subscribed(channel_id, session_id))?);

    let _ = host.stop()?;
    assert!(!host.inspect_bridge(|bridge| bridge.is_subscribed(channel_id, session_id))?);
    assert!(matches!(
        channel_handle.discovery_snapshot(),
        Err(ChannelActionIngressError::Inactive { .. })
    ));
    Ok(())
}
