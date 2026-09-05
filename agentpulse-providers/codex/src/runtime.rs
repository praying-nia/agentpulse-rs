//! Managed Codex App Server process and Provider Source lifecycle.

use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use agentpulse_bridge::{ProviderEventHandle, ProviderEventSource};
use agentpulse_core::{
    AgentCommandPayload, AgentEventPayload, AgentMessage, AgentMessageLevel, AgentMessageRole,
    AgentState, InteractionCloseReason, InteractionClosed, NonEmptyText, PromptDelivery,
    QueueAction, SessionId, Timestamp,
};
use serde_json::json;
use tungstenite::{Message, WebSocket};

#[cfg(unix)]
use semver::Version;
#[cfg(unix)]
use std::{
    fs,
    net::Shutdown,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
};

use crate::{
    CodexProviderConfig, CodexProviderHealth, CodexProviderSourceError,
    approval::{
        ApprovalRoute, ApprovalRuntimeState, ClosedApproval, ResolvedApproval, SharedApprovalState,
        prepare_approval, prepare_user_input,
    },
    control::{ControlRuntimeState, SharedControlState, TurnDefaults},
    mapper::{CodexEventMapper, MappingDisposition},
    protocol::{
        ExpectedResponse, ObservedServerFrame, ProtocolEngine, ProtocolSchema, ServerFrame,
    },
    status::{SharedStatus, lock_status},
};

const IO_POLL_INTERVAL: Duration = Duration::from_millis(200);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const STDERR_LIMIT: usize = 64 * 1024;
#[cfg(unix)]
const MAX_PROXY_CONNECTIONS: usize = 16;
#[cfg(unix)]
const LATEST_VERIFIED_CODEX_CLI_CORE: (u64, u64, u64) = (0, 153, 0);

#[cfg(unix)]
#[derive(Debug, Eq, PartialEq)]
enum CodexCliCompatibility {
    Verified,
    UnverifiedNewer(Version),
}

pub(crate) enum ReadOutcome {
    Text(String),
    Timeout,
    Closed,
}

pub(crate) trait AppServerIo: Send {
    fn write_text(&mut self, text: String) -> Result<(), CodexProviderSourceError>;
    fn read(&mut self) -> Result<ReadOutcome, CodexProviderSourceError>;
    fn close(&mut self) -> Result<(), CodexProviderSourceError>;
}

pub(crate) trait AppServerRuntime: Send {
    fn start(
        &mut self,
        config: &CodexProviderConfig,
    ) -> Result<Box<dyn AppServerIo>, CodexProviderSourceError>;

    fn stop(&mut self, config: &CodexProviderConfig) -> Result<(), CodexProviderSourceError>;
}

/// RuntimeHost Source that owns the managed App Server and live reader.
pub struct CodexProviderSource {
    config: CodexProviderConfig,
    schema: ProtocolSchema,
    mapper: Arc<Mutex<CodexEventMapper>>,
    status: SharedStatus,
    approvals: SharedApprovalState,
    controls: SharedControlState,
    runtime: Box<dyn AppServerRuntime>,
    stop_sender: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<WorkerExit>>,
    client_proxy_enabled: bool,
    proxy_control: Option<Arc<ProxyControl>>,
    proxy_worker: Option<JoinHandle<ProxyExit>>,
    resources_acquired: bool,
}

impl CodexProviderSource {
    pub(crate) fn new(
        config: CodexProviderConfig,
        schema: ProtocolSchema,
        mapper: CodexEventMapper,
        status: SharedStatus,
        approvals: SharedApprovalState,
        controls: SharedControlState,
    ) -> Self {
        let mut source = Self::with_runtime_and_states(
            config,
            schema,
            mapper,
            status,
            approvals,
            controls,
            Box::new(ManagedCodexRuntime::default()),
        );
        source.client_proxy_enabled = true;
        source
    }

    fn with_runtime_and_states(
        config: CodexProviderConfig,
        schema: ProtocolSchema,
        mapper: CodexEventMapper,
        status: SharedStatus,
        approvals: SharedApprovalState,
        controls: SharedControlState,
        runtime: Box<dyn AppServerRuntime>,
    ) -> Self {
        Self {
            config,
            schema,
            mapper: Arc::new(Mutex::new(mapper)),
            status,
            approvals,
            controls,
            runtime,
            stop_sender: None,
            worker: None,
            client_proxy_enabled: false,
            proxy_control: None,
            proxy_worker: None,
            resources_acquired: false,
        }
    }

    fn start_inner(&mut self, events: ProviderEventHandle) -> Result<(), CodexProviderSourceError> {
        if self.resources_acquired || self.worker.is_some() {
            return Err(CodexProviderSourceError::AlreadyStarted);
        }
        {
            let mut status = lock_status(&self.status);
            status.health = CodexProviderHealth::Starting;
            status.last_error = None;
        }
        if let Err(error) = lock_mapper(&self.mapper).begin_reconnect(&events, &self.status) {
            return Err(self.record_start_failure(error, &events));
        }

        self.resources_acquired = true;
        let mut io = match self.runtime.start(&self.config) {
            Ok(io) => io,
            Err(error) => return Err(self.record_start_failure(error, &events)),
        };
        let mut protocol = ProtocolEngine::new(self.schema.clone());

        if let Err(error) = initialize_connection(
            &mut *io,
            &mut protocol,
            &self.mapper,
            &events,
            &self.status,
            self.config.startup_timeout,
        ) {
            let _ = io.close();
            return Err(self.record_start_failure(error, &events));
        }

        let mut resume_failures = Vec::new();
        for thread in &self.config.threads {
            let thread_id = thread.external_id.as_str();
            let (request_id, request) = match protocol.thread_resume_request(thread_id) {
                Ok(request) => request,
                Err(error) => {
                    resume_failures.push(format!("{thread_id}: {error}"));
                    continue;
                }
            };
            if let Err(error) = io.write_text(request) {
                protocol.cancel_pending(&request_id);
                resume_failures.push(format!("{thread_id}: {error}"));
                continue;
            }
            match wait_for_response(
                &mut *io,
                &mut protocol,
                &request_id,
                ExpectedResponse::ThreadResume,
                StartupFrameContext {
                    mapper: &self.mapper,
                    events: &events,
                    status: &self.status,
                    timeout: self.config.startup_timeout,
                },
            ) {
                Ok(result) => {
                    if let Err(error) =
                        lock_mapper(&self.mapper).resume_thread(&result, &events, &self.status)
                    {
                        resume_failures.push(format!("{thread_id}: {error}"));
                    }
                }
                Err(error) => {
                    protocol.cancel_pending(&request_id);
                    resume_failures.push(format!("{thread_id}: {error}"));
                }
            }
        }

        if !resume_failures.is_empty() {
            let _ = io.close();
            return Err(self.record_start_failure(
                CodexProviderSourceError::ThreadResume {
                    failures: resume_failures.join("; "),
                },
                &events,
            ));
        }

        let (stop_sender, stop_receiver) = mpsc::channel();
        let mapper = Arc::clone(&self.mapper);
        let status = Arc::clone(&self.status);
        let worker_events = events.clone();
        let approvals = Arc::clone(&self.approvals);
        let controls = Arc::clone(&self.controls);
        let worker = thread::Builder::new()
            .name("agentpulse-codex-reader".to_owned())
            .spawn(move || {
                run_worker(
                    io,
                    protocol,
                    mapper,
                    status,
                    approvals,
                    controls,
                    worker_events,
                    stop_receiver,
                )
            })
            .map_err(|error| {
                self.record_start_failure(
                    CodexProviderSourceError::runtime("reader thread spawn", error),
                    &events,
                )
            })?;
        self.stop_sender = Some(stop_sender);
        self.worker = Some(worker);
        if self.client_proxy_enabled {
            match start_client_proxy(
                &self.config,
                self.schema.clone(),
                Arc::clone(&self.mapper),
                Arc::clone(&self.status),
                Arc::clone(&self.approvals),
                events.clone(),
            ) {
                Ok((control, worker)) => {
                    self.proxy_control = Some(control);
                    self.proxy_worker = Some(worker);
                }
                Err(error) => {
                    let _ = self.stop_inner();
                    return Err(self.record_start_failure(error, &events));
                }
            }
        }
        lock_status(&self.status).health = CodexProviderHealth::Running;
        Ok(())
    }

    fn stop_inner(&mut self) -> Result<(), CodexProviderSourceError> {
        let mut cleanup_failures = Vec::new();
        if let Some(control) = self.proxy_control.take() {
            control.request_stop();
        }
        if let Some(sender) = self.stop_sender.take() {
            let _ = sender.send(());
        }
        if let Some(worker) = self.proxy_worker.take() {
            match worker.join() {
                Ok(ProxyExit::Stopped(Ok(()))) | Ok(ProxyExit::Failed) => {}
                Ok(ProxyExit::Stopped(Err(error))) => cleanup_failures.push(error),
                Err(_) => cleanup_failures.push("client proxy thread panicked".to_owned()),
            }
        }
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(WorkerExit::Stopped(Ok(()))) | Ok(WorkerExit::Failed) => {}
                Ok(WorkerExit::Stopped(Err(error))) => cleanup_failures.push(error),
                Err(_) => cleanup_failures.push("reader thread panicked".to_owned()),
            }
        }
        if self.resources_acquired {
            if let Err(error) = self.runtime.stop(&self.config) {
                cleanup_failures.push(error.to_string());
            }
            self.resources_acquired = false;
        }

        if cleanup_failures.is_empty() {
            let mut status = lock_status(&self.status);
            status.health = CodexProviderHealth::Stopped;
            Ok(())
        } else {
            let error = CodexProviderSourceError::Shutdown {
                message: cleanup_failures.join("; "),
            };
            let mut status = lock_status(&self.status);
            status.health = CodexProviderHealth::Failed;
            status.last_error = Some(error.to_string());
            Err(error)
        }
    }

    fn record_start_failure(
        &self,
        error: CodexProviderSourceError,
        events: &ProviderEventHandle,
    ) -> CodexProviderSourceError {
        lock_mapper(&self.mapper).disconnect_all(events, &self.status);
        let mut status = lock_status(&self.status);
        status.health = CodexProviderHealth::Failed;
        status.last_error = Some(error.to_string());
        error
    }
}

impl ProviderEventSource for CodexProviderSource {
    type Error = CodexProviderSourceError;

    fn start(&mut self, events: ProviderEventHandle) -> Result<(), Self::Error> {
        self.start_inner(events)
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.stop_inner()
    }
}

fn initialize_connection(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
    timeout: Duration,
) -> Result<(), CodexProviderSourceError> {
    let (request_id, request) = protocol.initialize_request()?;
    io.write_text(request)?;
    let _ = wait_for_response(
        io,
        protocol,
        &request_id,
        ExpectedResponse::Initialize,
        StartupFrameContext {
            mapper,
            events,
            status,
            timeout,
        },
    )?;
    io.write_text(protocol.initialized_notification()?)
}

struct StartupFrameContext<'a> {
    mapper: &'a Arc<Mutex<CodexEventMapper>>,
    events: &'a ProviderEventHandle,
    status: &'a SharedStatus,
    timeout: Duration,
}

fn wait_for_response(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    request_id: &crate::protocol::RequestId,
    expected: ExpectedResponse,
    context: StartupFrameContext<'_>,
) -> Result<serde_json::Value, CodexProviderSourceError> {
    let deadline = Instant::now() + context.timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(CodexProviderSourceError::StartupTimeout {
                timeout: context.timeout,
            });
        }
        let text = match io.read()? {
            ReadOutcome::Text(text) => text,
            ReadOutcome::Timeout => continue,
            ReadOutcome::Closed => {
                return Err(CodexProviderSourceError::transport(
                    "App Server closed while a response was pending",
                ));
            }
        };
        let frame = protocol.parse_server_text(&text)?;
        lock_status(context.status).validated_frames += 1;
        match frame {
            ServerFrame::Response {
                id,
                expected: actual,
                result,
            } if actual == expected && id == *request_id => return Ok(result),
            ServerFrame::Response {
                id,
                expected: actual,
                ..
            } => {
                return Err(CodexProviderSourceError::protocol(format!(
                    "received {} response with ID {id:?} while waiting for {} ID {request_id:?}",
                    actual.method(),
                    expected.method()
                )));
            }
            ServerFrame::Error {
                id,
                expected: actual,
                code,
                message,
            } if actual == expected && id == *request_id => {
                return Err(CodexProviderSourceError::protocol(format!(
                    "{} returned error {code}: {message}",
                    expected.method()
                )));
            }
            ServerFrame::Error {
                id,
                expected: actual,
                ..
            } => {
                return Err(CodexProviderSourceError::protocol(format!(
                    "received {} error with ID {id:?} while waiting for {} ID {request_id:?}",
                    actual.method(),
                    expected.method()
                )));
            }
            ServerFrame::Request { id, method, .. } => {
                io.write_text(protocol.unsupported_request_response(id, &method)?)?;
                lock_status(context.status).rejected_server_requests += 1;
            }
            ServerFrame::Notification { method, params } => {
                let disposition = lock_mapper(context.mapper).notification(
                    &method,
                    &params,
                    context.events,
                    context.status,
                )?;
                if disposition == MappingDisposition::ValidatedUnmapped {
                    lock_status(context.status).validated_unmapped_frames += 1;
                }
            }
        }
    }
}

enum WorkerExit {
    Stopped(Result<(), String>),
    Failed,
}

enum ProxyExit {
    Stopped(Result<(), String>),
    Failed,
}

enum PendingControl {
    Report {
        session_id: SessionId,
    },
    SelectModel {
        session_id: SessionId,
        model: String,
        effort: Option<String>,
    },
    ResumeThread {
        source_session_id: SessionId,
    },
    StartThread,
    ForkThread,
    TurnStart {
        session_id: SessionId,
        thread_id: String,
        prompt: NonEmptyText,
    },
    Silent {
        session_id: SessionId,
    },
    HistoryItems {
        session_id: SessionId,
        thread_id: String,
    },
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    mut io: Box<dyn AppServerIo>,
    mut protocol: ProtocolEngine,
    mapper: Arc<Mutex<CodexEventMapper>>,
    status: SharedStatus,
    approvals: SharedApprovalState,
    controls: SharedControlState,
    events: ProviderEventHandle,
    stop_receiver: mpsc::Receiver<()>,
) -> WorkerExit {
    lock_controls(&controls).clear_all_inflight();
    let mut pending_controls = BTreeMap::new();
    loop {
        if stop_receiver.try_recv().is_ok() {
            close_all_approvals(&approvals, &mapper, &events, &status);
            return WorkerExit::Stopped(io.close().map_err(|error| error.to_string()));
        }
        let result =
            flush_approval_responses(&mut *io, &protocol, &approvals, ApprovalRoute::Observer);
        if let Err(error) = result {
            close_all_approvals(&approvals, &mapper, &events, &status);
            lock_mapper(&mapper).disconnect_all(&events, &status);
            let message = error.to_string();
            let mut current = lock_status(&status);
            current.health = CodexProviderHealth::Failed;
            current.last_error = Some(message);
            return WorkerExit::Failed;
        }
        if let Err(error) = flush_control_commands(
            &mut *io,
            &mut protocol,
            &mapper,
            &status,
            &controls,
            &events,
            &mut pending_controls,
        ) {
            lock_mapper(&mapper).disconnect_all(&events, &status);
            let message = error.to_string();
            let mut current = lock_status(&status);
            current.health = CodexProviderHealth::Failed;
            current.last_error = Some(message);
            return WorkerExit::Failed;
        }
        let result = match io.read() {
            Ok(ReadOutcome::Timeout) => continue,
            Ok(ReadOutcome::Closed) => Err(CodexProviderSourceError::transport(
                "App Server closed the WebSocket",
            )),
            Ok(ReadOutcome::Text(text)) => process_live_frame(
                &mut *io,
                &mut protocol,
                &mapper,
                &status,
                &approvals,
                &controls,
                &events,
                &mut pending_controls,
                &text,
                ApprovalRoute::Observer,
            ),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            if stop_receiver.try_recv().is_ok() {
                close_all_approvals(&approvals, &mapper, &events, &status);
                return WorkerExit::Stopped(io.close().map_err(|close| close.to_string()));
            }
            close_all_approvals(&approvals, &mapper, &events, &status);
            lock_mapper(&mapper).disconnect_all(&events, &status);
            let message = error.to_string();
            let mut current = lock_status(&status);
            current.health = CodexProviderHealth::Failed;
            current.last_error = Some(message.clone());
            return WorkerExit::Failed;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn flush_control_commands(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    controls: &SharedControlState,
    events: &ProviderEventHandle,
    pending: &mut BTreeMap<crate::protocol::RequestId, PendingControl>,
) -> Result<(), CodexProviderSourceError> {
    let inflight_sessions = { lock_controls(controls).inflight_sessions() };
    for session_id in inflight_sessions {
        let active = lock_mapper(mapper)
            .command_context(session_id)
            .is_some_and(|(_, turn, _)| turn.is_some());
        lock_controls(controls).observe_turn(session_id, active);
    }
    loop {
        // Keep the controls mutex out of the command body. Some commands publish
        // events through the Bridge, while another Bridge caller may concurrently
        // be waiting to enqueue a command under this mutex.
        let command = { lock_controls(controls).pop_command() };
        let Some(command) = command else {
            break;
        };
        let session_id = command.session_id();
        let context = lock_mapper(mapper)
            .command_context(session_id)
            .map(|(thread, turn, state)| (thread.to_owned(), turn.map(str::to_owned), state));
        match command.payload() {
            AgentCommandPayload::SelectModel { model, effort } => {
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::ModelList,
                    json!({"limit": 50}),
                    PendingControl::SelectModel {
                        session_id,
                        model: model.to_string(),
                        effort: effort.as_ref().map(ToString::to_string),
                    },
                    pending,
                )?;
            }
            AgentCommandPayload::SetPlanMode { enabled } => {
                lock_controls(controls).defaults_mut(session_id).plan_mode = *enabled;
                publish_system_for_session(
                    context.as_ref(),
                    if *enabled {
                        "Plan mode enabled"
                    } else {
                        "Plan mode disabled"
                    },
                    mapper,
                    events,
                    status,
                )?;
            }
            AgentCommandPayload::SelectPermissionProfile { profile } => {
                lock_controls(controls)
                    .defaults_mut(session_id)
                    .permission_profile = Some(profile.to_string());
                publish_system_for_session(
                    context.as_ref(),
                    format!("Permission profile set to {profile}"),
                    mapper,
                    events,
                    status,
                )?;
            }
            AgentCommandPayload::Queue { action } => {
                let (count, bytes, paused) = lock_controls(controls).queue_summary(session_id);
                publish_system_for_session(
                    context.as_ref(),
                    format!(
                        "Queue {}: {count} prompt(s), {bytes} bytes",
                        if paused { "paused" } else { "active" }
                    ),
                    mapper,
                    events,
                    status,
                )?;
                if matches!(action, QueueAction::Clear) {
                    // The summary above is the durable user-visible acknowledgement.
                }
            }
            AgentCommandPayload::Status => {
                let defaults = lock_controls(controls).defaults(session_id);
                let (count, bytes, paused) = lock_controls(controls).queue_summary(session_id);
                let state = context
                    .as_ref()
                    .map(|v| format!("{:?}", v.2))
                    .unwrap_or_else(|| "unknown".to_owned());
                publish_system_for_session(
                    context.as_ref(),
                    format!(
                        "State: {state}; model: {}; plan: {}; permissions: {}; queue: {count} item(s)/{bytes} bytes{}",
                        defaults.model.as_deref().unwrap_or("default"),
                        if defaults.plan_mode { "on" } else { "off" },
                        defaults.permission_profile.as_deref().unwrap_or("default"),
                        if paused { " (paused)" } else { "" },
                    ),
                    mapper,
                    events,
                    status,
                )?;
            }
            AgentCommandPayload::ListModels => {
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::ModelList,
                    json!({"limit": 50}),
                    PendingControl::Report { session_id },
                    pending,
                )?;
            }
            AgentCommandPayload::ListPermissionProfiles => {
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::PermissionProfileList,
                    json!({"limit": 50}),
                    PendingControl::Report { session_id },
                    pending,
                )?;
            }
            AgentCommandPayload::ListThreads { cursor } => {
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::ThreadList,
                    json!({"cursor": cursor.as_ref().map(ToString::to_string), "limit": 50, "sortKey": "updated_at", "sortDirection": "desc"}),
                    PendingControl::Report { session_id },
                    pending,
                )?;
            }
            AgentCommandPayload::ResumeThread { thread_id } => {
                lock_mapper(mapper).track_discovered_thread(thread_id.as_str())?;
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::ThreadResume,
                    json!({"threadId": thread_id.as_str()}),
                    PendingControl::ResumeThread {
                        source_session_id: session_id,
                    },
                    pending,
                )?;
            }
            AgentCommandPayload::StartThread { cwd } => {
                let defaults = lock_controls(controls).defaults(session_id);
                let mut params = json!({"cwd": cwd.as_str(), "sessionStartSource": "clear"});
                apply_thread_defaults(&mut params, &defaults);
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::ThreadStart,
                    params,
                    PendingControl::StartThread,
                    pending,
                )?;
            }
            AgentCommandPayload::Compact => {
                if let Some((thread_id, _, _)) = context.as_ref() {
                    send_control_request(
                        io,
                        protocol,
                        ExpectedResponse::ThreadCompact,
                        json!({"threadId": thread_id}),
                        PendingControl::Silent { session_id },
                        pending,
                    )?;
                }
            }
            AgentCommandPayload::Review { instructions } => {
                if let Some((thread_id, _, _)) = context.as_ref() {
                    let target = instructions.as_ref().map_or_else(
                        || json!({"type": "uncommittedChanges"}),
                        |value| json!({"type": "custom", "instructions": value.as_str()}),
                    );
                    send_control_request(
                        io,
                        protocol,
                        ExpectedResponse::ReviewStart,
                        json!({"threadId": thread_id, "target": target, "delivery": "inline"}),
                        PendingControl::Silent { session_id },
                        pending,
                    )?;
                }
            }
            AgentCommandPayload::Rename { name } => {
                if let Some((thread_id, _, _)) = context.as_ref() {
                    send_control_request(
                        io,
                        protocol,
                        ExpectedResponse::ThreadSetName,
                        json!({"threadId": thread_id, "name": name.as_str()}),
                        PendingControl::Silent { session_id },
                        pending,
                    )?;
                }
            }
            AgentCommandPayload::Fork => {
                if let Some((thread_id, _, _)) = context.as_ref() {
                    send_control_request(
                        io,
                        protocol,
                        ExpectedResponse::ThreadFork,
                        json!({"threadId": thread_id}),
                        PendingControl::ForkThread,
                        pending,
                    )?;
                }
            }
            AgentCommandPayload::CancelSession { .. } => {
                if let Some((thread_id, Some(turn_id), _)) = context.as_ref() {
                    send_control_request(
                        io,
                        protocol,
                        ExpectedResponse::TurnInterrupt,
                        json!({"threadId": thread_id, "turnId": turn_id}),
                        PendingControl::Silent { session_id },
                        pending,
                    )?;
                }
            }
            AgentCommandPayload::SubmitPrompt {
                text,
                delivery: PromptDelivery::Steer,
            } => {
                if let Some((thread_id, Some(turn_id), _)) = context.as_ref() {
                    send_control_request(
                        io,
                        protocol,
                        ExpectedResponse::TurnSteer,
                        json!({"threadId": thread_id, "expectedTurnId": turn_id, "input": [text_input(text.as_str())]}),
                        PendingControl::Silent { session_id },
                        pending,
                    )?;
                    publish_user_message(thread_id, text.as_str(), mapper, events, status)?;
                } else {
                    publish_system_for_session(
                        context.as_ref(),
                        "Cannot steer: no active turn",
                        mapper,
                        events,
                        status,
                    )?;
                }
            }
            AgentCommandPayload::SubmitPrompt {
                delivery: PromptDelivery::Queue,
                ..
            } => {}
            _ => {}
        }
    }

    let queued_sessions = { lock_controls(controls).queued_sessions() };
    for session_id in queued_sessions {
        let context = lock_mapper(mapper)
            .command_context(session_id)
            .map(|(thread, turn, state)| (thread.to_owned(), turn.map(str::to_owned), state));
        let Some((thread_id, None, _)) = context else {
            continue;
        };
        let Some(prompt) = lock_controls(controls).front_prompt(session_id) else {
            continue;
        };
        let defaults = lock_controls(controls).defaults(session_id);
        let thread_settings = lock_mapper(mapper)
            .model_settings_for_session(session_id)
            .map(|(model, effort)| (model.to_owned(), effort.map(str::to_owned)));
        let mut params = json!({"threadId": thread_id, "input": turn_input(prompt.as_str())});
        apply_turn_defaults(&mut params, &defaults, thread_settings.as_ref())?;
        send_control_request(
            io,
            protocol,
            ExpectedResponse::TurnStart,
            params,
            PendingControl::TurnStart {
                session_id,
                thread_id,
                prompt,
            },
            pending,
        )?;
        lock_controls(controls).mark_turn_inflight(session_id);
    }
    Ok(())
}

fn send_control_request(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    expected: ExpectedResponse,
    params: serde_json::Value,
    kind: PendingControl,
    pending: &mut BTreeMap<crate::protocol::RequestId, PendingControl>,
) -> Result<(), CodexProviderSourceError> {
    let (id, request) = protocol.request(expected, params)?;
    if let Err(error) = io.write_text(request) {
        protocol.cancel_pending(&id);
        return Err(error);
    }
    pending.insert(id, kind);
    Ok(())
}

fn text_input(text: &str) -> serde_json::Value {
    json!({"type": "text", "text": text})
}

fn turn_input(text: &str) -> serde_json::Value {
    json!([text_input(text)])
}

fn apply_turn_defaults(
    params: &mut serde_json::Value,
    defaults: &TurnDefaults,
    thread_settings: Option<&(String, Option<String>)>,
) -> Result<(), CodexProviderSourceError> {
    let Some(object) = params.as_object_mut() else {
        return Err(CodexProviderSourceError::protocol(
            "turn/start params must be an object",
        ));
    };
    if let Some(model) = &defaults.model {
        object.insert("model".to_owned(), json!(model));
    }
    if let Some(effort) = &defaults.effort {
        object.insert("effort".to_owned(), json!(effort));
    }
    let effective_model = defaults
        .model
        .as_ref()
        .or_else(|| thread_settings.map(|(model, _)| model));
    let effective_effort = defaults
        .effort
        .as_ref()
        .or_else(|| thread_settings.and_then(|(_, effort)| effort.as_ref()));
    if defaults.plan_mode && effective_model.is_none() {
        return Err(CodexProviderSourceError::protocol(
            "cannot start a Plan turn before Codex reports the thread model",
        ));
    }
    if let Some(model) = effective_model {
        object.insert(
            "collaborationMode".to_owned(),
            json!({
                "mode": if defaults.plan_mode { "plan" } else { "default" },
                "settings": {
                    "model": model,
                    "reasoning_effort": effective_effort,
                    "developer_instructions": null
                }
            }),
        );
    }
    if let Some(profile) = defaults.permission_profile.as_deref() {
        let policy = match profile {
            "read-only" | "read_only" => Some(json!({"type": "readOnly"})),
            "workspace-write" | "workspace_write" => Some(json!({"type": "workspaceWrite"})),
            "danger-full-access" | "danger_full_access" => {
                Some(json!({"type": "dangerFullAccess"}))
            }
            _ => None,
        };
        if let Some(policy) = policy {
            object.insert("sandboxPolicy".to_owned(), policy);
        }
    }
    Ok(())
}

fn apply_thread_defaults(params: &mut serde_json::Value, defaults: &TurnDefaults) {
    let Some(object) = params.as_object_mut() else {
        return;
    };
    if let Some(model) = &defaults.model {
        object.insert("model".to_owned(), json!(model));
    }
}

fn publish_user_message(
    thread_id: &str,
    text: &str,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    lock_mapper(mapper).publish_payload(
        thread_id,
        Timestamp::now_utc(),
        AgentEventPayload::Message(AgentMessage::with_role(
            AgentMessageRole::User,
            AgentMessageLevel::Info,
            NonEmptyText::new(text.to_owned())?,
        )),
        events,
        status,
    )
}

fn publish_system_for_session(
    context: Option<&(String, Option<String>, AgentState)>,
    text: impl Into<String>,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    let Some((thread_id, _, _)) = context else {
        return Ok(());
    };
    lock_mapper(mapper).publish_payload(
        thread_id,
        Timestamp::now_utc(),
        AgentEventPayload::Message(AgentMessage::with_role(
            AgentMessageRole::System,
            AgentMessageLevel::Info,
            NonEmptyText::new(text.into())?,
        )),
        events,
        status,
    )
}

fn lock_controls(state: &SharedControlState) -> MutexGuard<'_, ControlRuntimeState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[allow(clippy::too_many_arguments)]
fn process_control_response(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    id: crate::protocol::RequestId,
    expected: ExpectedResponse,
    result: serde_json::Value,
    pending: &mut BTreeMap<crate::protocol::RequestId, PendingControl>,
    controls: &SharedControlState,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    let kind = pending.remove(&id).ok_or_else(|| {
        CodexProviderSourceError::protocol(format!(
            "unexpected live {} response",
            expected.method()
        ))
    })?;
    match kind {
        PendingControl::Report { session_id } => {
            let current_cwd = lock_mapper(mapper)
                .cwd_for_session(session_id)
                .map(str::to_owned);
            let text = format_catalog(expected, &result, current_cwd.as_deref())?;
            let context = lock_mapper(mapper)
                .command_context(session_id)
                .map(|(thread, turn, state)| (thread.to_owned(), turn.map(str::to_owned), state));
            publish_system_for_session(context.as_ref(), text, mapper, events, status)
        }
        PendingControl::SelectModel {
            session_id,
            model,
            effort,
        } => {
            let context = lock_mapper(mapper)
                .command_context(session_id)
                .map(|(thread, turn, state)| (thread.to_owned(), turn.map(str::to_owned), state));
            match validate_model_selection(&result, &model, effort.as_deref()) {
                Ok(()) => {
                    let mut controls = lock_controls(controls);
                    let defaults = controls.defaults_mut(session_id);
                    defaults.model = Some(model.clone());
                    defaults.effort = effort.clone();
                    drop(controls);
                    publish_system_for_session(
                        context.as_ref(),
                        format!(
                            "Model set to {}{}",
                            model,
                            effort
                                .as_ref()
                                .map(|value| format!(" ({value})"))
                                .unwrap_or_default()
                        ),
                        mapper,
                        events,
                        status,
                    )
                }
                Err(message) => publish_system_for_session(
                    context.as_ref(),
                    format!("Model selection rejected: {message}"),
                    mapper,
                    events,
                    status,
                ),
            }
        }
        PendingControl::ResumeThread { .. } => {
            let thread_id = result
                .get("thread")
                .and_then(|thread| thread.get("id"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    CodexProviderSourceError::protocol("thread/resume omitted thread.id")
                })?
                .to_owned();
            let hydrate_history = !lock_mapper(mapper).is_thread_tracked(&thread_id);
            lock_mapper(mapper).resume_thread(&result, events, status)?;
            if !hydrate_history {
                return Ok(());
            }
            let session_id = lock_mapper(mapper)
                .session_id_for_thread(&thread_id)
                .ok_or_else(|| {
                    CodexProviderSourceError::protocol("resumed thread was not tracked")
                })?;
            send_control_request(
                io,
                protocol,
                ExpectedResponse::ThreadItemsList,
                json!({"threadId": thread_id, "limit": 100, "sortDirection": "asc"}),
                PendingControl::HistoryItems {
                    session_id,
                    thread_id,
                },
                pending,
            )
        }
        PendingControl::StartThread | PendingControl::ForkThread => {
            let thread_id = result
                .get("thread")
                .and_then(|thread| thread.get("id"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    CodexProviderSourceError::protocol("thread response omitted thread.id")
                })?
                .to_owned();
            let mut mapper = lock_mapper(mapper);
            mapper.track_discovered_thread(&thread_id)?;
            mapper.resume_thread(&result, events, status)
        }
        PendingControl::TurnStart {
            session_id,
            thread_id,
            prompt,
        } => {
            let removed = lock_controls(controls).pop_prompt(session_id);
            if removed.as_ref() != Some(&prompt) {
                return Err(CodexProviderSourceError::protocol(
                    "queued prompt changed before turn/start completed",
                ));
            }
            publish_user_message(&thread_id, prompt.as_str(), mapper, events, status)
        }
        PendingControl::Silent { .. } => Ok(()),
        PendingControl::HistoryItems {
            session_id,
            thread_id,
        } => {
            let data = result
                .get("data")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    CodexProviderSourceError::protocol("thread/items/list omitted data")
                })?;
            lock_mapper(mapper).publish_history_items(&thread_id, data, events, status)?;
            if let Some(cursor) = result.get("nextCursor").and_then(serde_json::Value::as_str) {
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::ThreadItemsList,
                    json!({"threadId": thread_id, "cursor": cursor, "limit": 100, "sortDirection": "asc"}),
                    PendingControl::HistoryItems {
                        session_id,
                        thread_id,
                    },
                    pending,
                )?;
            }
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_control_error(
    id: crate::protocol::RequestId,
    expected: ExpectedResponse,
    code: i64,
    message: String,
    pending: &mut BTreeMap<crate::protocol::RequestId, PendingControl>,
    controls: &SharedControlState,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    let kind = pending.remove(&id).ok_or_else(|| {
        CodexProviderSourceError::protocol(format!(
            "unexpected live {} error response",
            expected.method()
        ))
    })?;
    match kind {
        PendingControl::Report { session_id }
        | PendingControl::SelectModel { session_id, .. }
        | PendingControl::Silent { session_id }
        | PendingControl::HistoryItems { session_id, .. }
        | PendingControl::ResumeThread {
            source_session_id: session_id,
        } => {
            let context = lock_mapper(mapper)
                .command_context(session_id)
                .map(|(thread, turn, state)| (thread.to_owned(), turn.map(str::to_owned), state));
            publish_system_for_session(
                context.as_ref(),
                format!("{} failed ({code}): {message}", expected.method()),
                mapper,
                events,
                status,
            )?;
        }
        PendingControl::TurnStart { session_id, .. } => {
            let mut controls = lock_controls(controls);
            controls.clear_turn_inflight(session_id);
            controls.pause_prompts(session_id);
            drop(controls);
            let context = lock_mapper(mapper)
                .command_context(session_id)
                .map(|(thread, turn, state)| (thread.to_owned(), turn.map(str::to_owned), state));
            publish_system_for_session(
                context.as_ref(),
                format!("{} failed ({code}): {message}", expected.method()),
                mapper,
                events,
                status,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn format_catalog(
    expected: ExpectedResponse,
    result: &serde_json::Value,
    current_cwd: Option<&str>,
) -> Result<String, CodexProviderSourceError> {
    let data = result
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| CodexProviderSourceError::protocol("catalog response omitted data"))?;
    let mut lines = Vec::with_capacity(data.len() + 1);
    match expected {
        ExpectedResponse::ModelList => {
            lines.push("Available models (/model <id> [effort]):".to_owned());
            let mut threads = data.iter().collect::<Vec<_>>();
            threads.sort_by_key(|value| {
                value.get("cwd").and_then(serde_json::Value::as_str) != current_cwd
            });
            for value in threads {
                let id = value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?");
                let name = value
                    .get("displayName")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(id);
                let efforts = supported_reasoning_efforts(value);
                lines.push(if efforts.is_empty() {
                    format!("{id} — {name}")
                } else {
                    format!("{id} — {name} — efforts: {}", efforts.join(", "))
                });
            }
        }
        ExpectedResponse::PermissionProfileList => {
            lines.push("Permission profiles (/permissions <id>):".to_owned());
            for value in data {
                let id = value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?");
                let description = value
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                lines.push(format!("{id} — {description}"));
            }
        }
        ExpectedResponse::ThreadList => {
            lines.push("Threads, newest first (/resume <id>):".to_owned());
            for value in data {
                let id = value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?");
                let title = value
                    .get("name")
                    .or_else(|| value.get("preview"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Untitled");
                let cwd = value
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?");
                lines.push(format!("{id} — {title} — {cwd}"));
            }
            if let Some(cursor) = result.get("nextCursor").and_then(serde_json::Value::as_str) {
                lines.push(format!("More: /resume --cursor {cursor}"));
            }
        }
        _ => {
            return Err(CodexProviderSourceError::protocol(
                "non-catalog response used as a catalog",
            ));
        }
    }
    Ok(lines.join("\n"))
}

fn supported_reasoning_efforts(model: &serde_json::Value) -> Vec<&str> {
    model
        .get("supportedReasoningEfforts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| {
            option
                .get("reasoningEffort")
                .and_then(serde_json::Value::as_str)
        })
        .collect()
}

fn validate_model_selection(
    result: &serde_json::Value,
    requested_model: &str,
    requested_effort: Option<&str>,
) -> Result<(), String> {
    let models = result
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Codex model catalog omitted data".to_owned())?;
    let model = models
        .iter()
        .find(|value| value.get("id").and_then(serde_json::Value::as_str) == Some(requested_model))
        .ok_or_else(|| {
            format!("unknown model `{requested_model}`; run /model to list valid IDs")
        })?;
    if let Some(effort) = requested_effort {
        let supported = supported_reasoning_efforts(model);
        if !supported.contains(&effort) {
            return Err(format!(
                "effort `{effort}` is not supported by `{requested_model}`; valid efforts: {}",
                if supported.is_empty() {
                    "none advertised".to_owned()
                } else {
                    supported.join(", ")
                }
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_live_frame(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    approvals: &SharedApprovalState,
    controls: &SharedControlState,
    events: &ProviderEventHandle,
    pending_controls: &mut BTreeMap<crate::protocol::RequestId, PendingControl>,
    text: &str,
    route: ApprovalRoute,
) -> Result<(), CodexProviderSourceError> {
    let frame = protocol.parse_server_text(text)?;
    lock_status(status).validated_frames += 1;
    match frame {
        ServerFrame::Notification { method, params } => {
            if method == "serverRequest/resolved" {
                process_resolved_notification(&params, route, mapper, status, approvals, events)?;
                return Ok(());
            }
            close_approvals_for_lifecycle(&method, &params, mapper, status, approvals, events)?;
            let disposition = lock_mapper(mapper).notification(&method, &params, events, status)?;
            if disposition == MappingDisposition::ValidatedUnmapped {
                lock_status(status).validated_unmapped_frames += 1;
            }
            Ok(())
        }
        ServerFrame::Request { id, method, params } => {
            if is_interaction_method(&method) {
                process_approval_request(
                    Some(io),
                    protocol,
                    route,
                    id,
                    method,
                    params,
                    mapper,
                    status,
                    approvals,
                    events,
                )
            } else {
                io.write_text(protocol.unsupported_request_response(id, &method)?)?;
                lock_status(status).rejected_server_requests += 1;
                Ok(())
            }
        }
        ServerFrame::Response {
            id,
            expected,
            result,
        } => process_control_response(
            io,
            protocol,
            id,
            expected,
            result,
            pending_controls,
            controls,
            mapper,
            events,
            status,
        ),
        ServerFrame::Error {
            id,
            expected,
            code,
            message,
        } => process_control_error(
            id,
            expected,
            code,
            message,
            pending_controls,
            controls,
            mapper,
            events,
            status,
        ),
    }
}

fn is_interaction_method(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/tool/requestUserInput"
    )
}

#[allow(clippy::too_many_arguments)]
fn process_approval_request(
    mut io: Option<&mut dyn AppServerIo>,
    protocol: &ProtocolEngine,
    route: ApprovalRoute,
    id: crate::protocol::RequestId,
    method: String,
    params: serde_json::Value,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    approvals: &SharedApprovalState,
    events: &ProviderEventHandle,
) -> Result<(), CodexProviderSourceError> {
    let thread_id = protocol_string(&params, "threadId")?;
    let turn_id = protocol_string(&params, "turnId")?;
    let item_id = protocol_string(&params, "itemId")?;
    let context = lock_mapper(mapper).approval_context(thread_id, turn_id, item_id)?;
    let Some((session_id, item)) = context else {
        if let Some(io) = io.as_mut() {
            io.write_text(protocol.unsupported_request_response(id, &method)?)?;
        }
        lock_status(status).rejected_server_requests += 1;
        return Ok(());
    };
    let prepared = if method == "item/tool/requestUserInput" {
        prepare_user_input(&params, session_id)?
    } else {
        prepare_approval(&method, &params, item.as_ref(), session_id)?
    };
    let interaction_id = prepared.request.id();
    {
        let mut state = lock_approvals(approvals);
        state.register(route, id, method, &prepared)?;
    }
    let publish = lock_mapper(mapper).publish_payload(
        &prepared.thread_id,
        prepared.request.requested_at(),
        AgentEventPayload::InteractionRequested(prepared.request),
        events,
        status,
    );
    if publish.is_err() {
        lock_approvals(approvals).remove(interaction_id);
    }
    publish
}

fn flush_approval_responses(
    io: &mut dyn AppServerIo,
    protocol: &ProtocolEngine,
    approvals: &SharedApprovalState,
    route: ApprovalRoute,
) -> Result<(), CodexProviderSourceError> {
    loop {
        let Some(outbound) = lock_approvals(approvals).pop_outbound_for(route) else {
            return Ok(());
        };
        io.write_text(protocol.interaction_response(
            outbound.request_id,
            &outbound.method,
            outbound.result,
        )?)?;
        lock_approvals(approvals).mark_sent(outbound.interaction_id)?;
    }
}

fn process_resolved_notification(
    params: &serde_json::Value,
    route: ApprovalRoute,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    approvals: &SharedApprovalState,
    events: &ProviderEventHandle,
) -> Result<(), CodexProviderSourceError> {
    let thread_id = protocol_string(params, "threadId")?;
    let request_id = params
        .get("requestId")
        .ok_or_else(|| CodexProviderSourceError::protocol("requestId is required"))
        .and_then(crate::protocol::RequestId::from_value)?;
    match lock_approvals(approvals).resolve(route, &request_id, thread_id)? {
        Some(ResolvedApproval::Responded {
            thread_id,
            response,
        }) => {
            let occurred_at = response.responded_at();
            lock_mapper(mapper).publish_payload(
                &thread_id,
                occurred_at,
                AgentEventPayload::InteractionResponded(response),
                events,
                status,
            )?;
        }
        Some(ResolvedApproval::Closed {
            thread_id,
            session_id,
            interaction_id,
        }) => {
            publish_closed(
                ClosedApproval {
                    thread_id,
                    session_id,
                    interaction_id,
                },
                InteractionCloseReason::ResolvedElsewhere,
                mapper,
                events,
                status,
            )?;
        }
        Some(ResolvedApproval::RespondedForm {
            thread_id,
            session_id,
            interaction_id,
        }) => {
            // Form answers can contain secrets. Their values are written once to Codex and are
            // deliberately not retained or republished through the event history.
            publish_closed(
                ClosedApproval {
                    thread_id,
                    session_id,
                    interaction_id,
                },
                InteractionCloseReason::ResolvedElsewhere,
                mapper,
                events,
                status,
            )?;
        }
        None => lock_status(status).validated_unmapped_frames += 1,
    }
    Ok(())
}

fn close_approvals_for_lifecycle(
    method: &str,
    params: &serde_json::Value,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    approvals: &SharedApprovalState,
    events: &ProviderEventHandle,
) -> Result<(), CodexProviderSourceError> {
    let closed = match method {
        "item/completed" => {
            let thread_id = protocol_string(params, "threadId")?;
            let turn_id = protocol_string(params, "turnId")?;
            let item_id = params
                .get("item")
                .and_then(|item| item.get("id"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| CodexProviderSourceError::protocol("item.id must be a string"))?;
            lock_approvals(approvals).close_item(thread_id, turn_id, item_id)
        }
        "turn/completed" => {
            let thread_id = protocol_string(params, "threadId")?;
            let turn_id = params
                .get("turn")
                .and_then(|turn| turn.get("id"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| CodexProviderSourceError::protocol("turn.id must be a string"))?;
            lock_approvals(approvals).close_turn(thread_id, turn_id)
        }
        "thread/closed" => {
            let thread_id = protocol_string(params, "threadId")?;
            lock_approvals(approvals).close_thread(thread_id)
        }
        "thread/status/changed"
            if params
                .get("status")
                .and_then(|value| value.get("type"))
                .and_then(serde_json::Value::as_str)
                == Some("notLoaded") =>
        {
            let thread_id = protocol_string(params, "threadId")?;
            lock_approvals(approvals).close_thread(thread_id)
        }
        _ => Vec::new(),
    };
    for closed in closed {
        publish_closed(
            closed,
            InteractionCloseReason::ProviderCancelled,
            mapper,
            events,
            status,
        )?;
    }
    Ok(())
}

fn close_all_approvals(
    approvals: &SharedApprovalState,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) {
    let closed = lock_approvals(approvals).close_all();
    for closed in closed {
        let _ = publish_closed(
            closed,
            InteractionCloseReason::ProviderCancelled,
            mapper,
            events,
            status,
        );
    }
}

fn publish_closed(
    closed: ClosedApproval,
    reason: InteractionCloseReason,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    lock_mapper(mapper).publish_payload(
        &closed.thread_id,
        Timestamp::now_utc(),
        AgentEventPayload::InteractionClosed(InteractionClosed::new(
            closed.interaction_id,
            closed.session_id,
            reason,
        )),
        events,
        status,
    )
}

fn protocol_string<'a>(
    value: &'a serde_json::Value,
    field: &'static str,
) -> Result<&'a str, CodexProviderSourceError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| CodexProviderSourceError::protocol(format!("{field} must be a string")))
}

#[derive(Default)]
struct ProxyControl {
    stopped: AtomicBool,
    #[cfg(unix)]
    connections: Mutex<BTreeMap<u64, Vec<UnixStream>>>,
}

impl ProxyControl {
    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn request_stop(&self) {
        self.stopped.store(true, Ordering::Release);
        #[cfg(unix)]
        {
            let connections = {
                let mut connections = self
                    .connections
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                std::mem::take(&mut *connections)
            };
            for stream in connections.into_values().flatten() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }

    #[cfg(unix)]
    fn register(
        &self,
        route: ApprovalRoute,
        stream: &UnixStream,
    ) -> Result<bool, CodexProviderSourceError> {
        let interrupt = stream.try_clone().map_err(|error| {
            CodexProviderSourceError::runtime("client proxy cancellation socket clone", error)
        })?;
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_stopped() {
            drop(connections);
            let _ = interrupt.shutdown(Shutdown::Both);
            return Ok(false);
        }
        connections
            .entry(route_number(route))
            .or_default()
            .push(interrupt);
        Ok(true)
    }

    #[cfg(unix)]
    fn unregister(&self, route: ApprovalRoute) {
        self.connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&route_number(route));
    }
}

#[cfg(unix)]
fn start_client_proxy(
    config: &CodexProviderConfig,
    schema: ProtocolSchema,
    mapper: Arc<Mutex<CodexEventMapper>>,
    status: SharedStatus,
    approvals: SharedApprovalState,
    events: ProviderEventHandle,
) -> Result<(Arc<ProxyControl>, JoinHandle<ProxyExit>), CodexProviderSourceError> {
    let listener = UnixListener::bind(&config.proxy_socket_path)
        .map_err(|error| CodexProviderSourceError::runtime("client proxy bind", error))?;
    if let Err(error) =
        fs::set_permissions(&config.proxy_socket_path, fs::Permissions::from_mode(0o600))
    {
        let _ = fs::remove_file(&config.proxy_socket_path);
        return Err(CodexProviderSourceError::runtime(
            "client proxy permissions",
            error,
        ));
    }
    if let Err(error) = listener.set_nonblocking(true) {
        let _ = fs::remove_file(&config.proxy_socket_path);
        return Err(CodexProviderSourceError::runtime(
            "client proxy nonblocking mode",
            error,
        ));
    }

    let control = Arc::new(ProxyControl::default());
    let worker_control = Arc::clone(&control);
    let app_server_socket_path = config.socket_path.clone();
    let proxy_socket_path = config.proxy_socket_path.clone();
    let worker = thread::Builder::new()
        .name("agentpulse-codex-proxy".to_owned())
        .spawn(move || {
            run_client_proxy(
                listener,
                app_server_socket_path,
                proxy_socket_path,
                schema,
                mapper,
                status,
                approvals,
                events,
                worker_control,
            )
        })
        .map_err(|error| {
            let _ = fs::remove_file(&config.proxy_socket_path);
            CodexProviderSourceError::runtime("client proxy thread spawn", error)
        })?;
    Ok((control, worker))
}

#[cfg(not(unix))]
fn start_client_proxy(
    _config: &CodexProviderConfig,
    _schema: ProtocolSchema,
    _mapper: Arc<Mutex<CodexEventMapper>>,
    _status: SharedStatus,
    _approvals: SharedApprovalState,
    _events: ProviderEventHandle,
) -> Result<(Arc<ProxyControl>, JoinHandle<ProxyExit>), CodexProviderSourceError> {
    Err(CodexProviderSourceError::UnsupportedPlatform)
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_client_proxy(
    listener: UnixListener,
    app_server_socket_path: std::path::PathBuf,
    proxy_socket_path: std::path::PathBuf,
    schema: ProtocolSchema,
    mapper: Arc<Mutex<CodexEventMapper>>,
    status: SharedStatus,
    approvals: SharedApprovalState,
    events: ProviderEventHandle,
    control: Arc<ProxyControl>,
) -> ProxyExit {
    let mut next_route = 1_u64;
    let mut clients: Vec<JoinHandle<()>> = Vec::new();
    while !control.is_stopped() {
        let mut index = 0;
        while index < clients.len() {
            if clients[index].is_finished() {
                let worker = clients.swap_remove(index);
                let _ = worker.join();
            } else {
                index += 1;
            }
        }

        match listener.accept() {
            Ok((stream, _)) => {
                if clients.len() >= MAX_PROXY_CONNECTIONS {
                    drop(stream);
                    continue;
                }
                let route = ApprovalRoute::Proxy(next_route);
                next_route = next_route.saturating_add(1);
                let worker_path = app_server_socket_path.clone();
                let worker_schema = schema.clone();
                let worker_mapper = Arc::clone(&mapper);
                let worker_status = Arc::clone(&status);
                let worker_approvals = Arc::clone(&approvals);
                let worker_events = events.clone();
                let worker_control = Arc::clone(&control);
                match thread::Builder::new()
                    .name(format!("agentpulse-codex-client-{}", route_number(route)))
                    .spawn(move || {
                        run_proxy_connection(
                            stream,
                            &worker_path,
                            worker_schema,
                            worker_mapper,
                            worker_status,
                            worker_approvals,
                            worker_events,
                            worker_control,
                            route,
                        );
                    }) {
                    Ok(worker) => clients.push(worker),
                    Err(error) => {
                        control.unregister(route);
                        eprintln!("warning: failed to spawn Codex client proxy: {error}");
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(PROCESS_POLL_INTERVAL);
            }
            Err(error) => {
                let message = format!("Codex client proxy accept failed: {error}");
                let mut current = lock_status(&status);
                current.health = CodexProviderHealth::Failed;
                current.last_error = Some(message);
                control.request_stop();
                for worker in clients {
                    let _ = worker.join();
                }
                let _ = fs::remove_file(proxy_socket_path);
                return ProxyExit::Failed;
            }
        }
    }

    for worker in clients {
        let _ = worker.join();
    }
    let cleanup = match fs::remove_file(proxy_socket_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("client proxy socket removal: {error}")),
    };
    ProxyExit::Stopped(cleanup)
}

#[cfg(unix)]
const fn route_number(route: ApprovalRoute) -> u64 {
    match route {
        ApprovalRoute::Proxy(number) => number,
        ApprovalRoute::Observer => 0,
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_proxy_connection(
    downstream_stream: UnixStream,
    app_server_socket_path: &std::path::Path,
    schema: ProtocolSchema,
    mapper: Arc<Mutex<CodexEventMapper>>,
    status: SharedStatus,
    approvals: SharedApprovalState,
    events: ProviderEventHandle,
    control: Arc<ProxyControl>,
    route: ApprovalRoute,
) {
    let result = proxy_connection_loop(
        downstream_stream,
        app_server_socket_path,
        &schema,
        &mapper,
        &status,
        &approvals,
        &events,
        &control,
        route,
    );
    control.unregister(route);
    let closed = lock_approvals(&approvals).close_route(route);
    for approval in closed {
        let _ = publish_closed(
            approval,
            InteractionCloseReason::ProviderCancelled,
            &mapper,
            &events,
            &status,
        );
    }
    if let Err(error) = result
        && !control.is_stopped()
    {
        eprintln!("warning: Codex client proxy connection ended: {error}");
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn proxy_connection_loop(
    downstream_stream: UnixStream,
    app_server_socket_path: &std::path::Path,
    schema: &ProtocolSchema,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    approvals: &SharedApprovalState,
    events: &ProviderEventHandle,
    control: &ProxyControl,
    route: ApprovalRoute,
) -> Result<(), CodexProviderSourceError> {
    if !control.register(route, &downstream_stream)? {
        return Ok(());
    }
    downstream_stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| CodexProviderSourceError::runtime("client handshake timeout", error))?;
    downstream_stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| CodexProviderSourceError::runtime("client write timeout", error))?;
    let mut downstream = tungstenite::accept(downstream_stream).map_err(|error| {
        CodexProviderSourceError::transport(format!("client WebSocket handshake failed: {error}"))
    })?;
    configure_proxy_stream(downstream.get_mut())?;

    let upstream_stream = UnixStream::connect(app_server_socket_path)
        .map_err(|error| CodexProviderSourceError::runtime("proxy upstream connection", error))?;
    if !control.register(route, &upstream_stream)? {
        return Ok(());
    }
    configure_proxy_stream(&upstream_stream)?;
    let (mut upstream, _) =
        tungstenite::client("ws://localhost/", upstream_stream).map_err(|error| {
            CodexProviderSourceError::transport(format!(
                "proxy upstream WebSocket handshake failed: {error}"
            ))
        })?;
    let protocol = ProtocolEngine::new(schema.clone());

    while !control.is_stopped() {
        flush_approval_responses(
            &mut UnixWebSocketRef(&mut upstream),
            &protocol,
            approvals,
            route,
        )?;

        match downstream.read() {
            Ok(Message::Text(text)) => upstream
                .send(Message::Text(text))
                .map_err(proxy_transport_error)?,
            Ok(Message::Binary(bytes)) => upstream
                .send(Message::Binary(bytes))
                .map_err(proxy_transport_error)?,
            Ok(Message::Ping(_) | Message::Pong(_)) => {
                downstream.flush().map_err(proxy_transport_error)?;
            }
            Ok(Message::Close(frame)) => {
                let _ = upstream.close(frame);
                return Ok(());
            }
            Ok(Message::Frame(_)) => {}
            Err(tungstenite::Error::Io(error)) if is_timeout(&error) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(());
            }
            Err(error) => return Err(proxy_transport_error(error)),
        }

        match upstream.read() {
            Ok(Message::Text(text)) => {
                if let Err(error) = observe_proxy_server_text(
                    &protocol, route, mapper, status, approvals, events, &text,
                ) {
                    eprintln!(
                        "warning: Codex client frame was not mirrored to AgentPulse: {error}"
                    );
                }
                downstream
                    .send(Message::Text(text))
                    .map_err(proxy_transport_error)?;
            }
            Ok(Message::Binary(bytes)) => downstream
                .send(Message::Binary(bytes))
                .map_err(proxy_transport_error)?,
            Ok(Message::Ping(_) | Message::Pong(_)) => {
                upstream.flush().map_err(proxy_transport_error)?;
            }
            Ok(Message::Close(frame)) => {
                let _ = downstream.close(frame);
                return Ok(());
            }
            Ok(Message::Frame(_)) => {}
            Err(tungstenite::Error::Io(error)) if is_timeout(&error) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(());
            }
            Err(error) => return Err(proxy_transport_error(error)),
        }
    }

    let _ = downstream.close(None);
    let _ = upstream.close(None);
    Ok(())
}

#[cfg(unix)]
fn configure_proxy_stream(stream: &UnixStream) -> Result<(), CodexProviderSourceError> {
    stream
        .set_read_timeout(Some(IO_POLL_INTERVAL))
        .map_err(|error| CodexProviderSourceError::runtime("proxy socket read timeout", error))?;
    stream
        .set_write_timeout(Some(IO_POLL_INTERVAL))
        .map_err(|error| CodexProviderSourceError::runtime("proxy socket write timeout", error))
}

#[cfg(unix)]
fn proxy_transport_error(error: tungstenite::Error) -> CodexProviderSourceError {
    CodexProviderSourceError::transport(format!("client proxy: {error}"))
}

#[cfg(unix)]
fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn observe_proxy_server_text(
    protocol: &ProtocolEngine,
    route: ApprovalRoute,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    approvals: &SharedApprovalState,
    events: &ProviderEventHandle,
    text: &str,
) -> Result<(), CodexProviderSourceError> {
    let frame = protocol.parse_observed_server_text(text)?;
    lock_status(status).validated_frames += 1;
    match frame {
        ObservedServerFrame::Notification { method, params } => {
            if method == "serverRequest/resolved" {
                return process_resolved_notification(
                    &params, route, mapper, status, approvals, events,
                );
            }
            close_approvals_for_lifecycle(&method, &params, mapper, status, approvals, events)?;
            let disposition = lock_mapper(mapper).notification(&method, &params, events, status)?;
            if disposition == MappingDisposition::ValidatedUnmapped {
                lock_status(status).validated_unmapped_frames += 1;
            }
            Ok(())
        }
        ObservedServerFrame::Request { id, method, params } => {
            if is_interaction_method(&method) {
                process_approval_request(
                    None, protocol, route, id, method, params, mapper, status, approvals, events,
                )
            } else {
                lock_status(status).validated_unmapped_frames += 1;
                Ok(())
            }
        }
        ObservedServerFrame::PassThrough => Ok(()),
    }
}

#[cfg(unix)]
struct UnixWebSocketRef<'a>(&'a mut WebSocket<UnixStream>);

#[cfg(unix)]
impl AppServerIo for UnixWebSocketRef<'_> {
    fn write_text(&mut self, text: String) -> Result<(), CodexProviderSourceError> {
        self.0
            .send(Message::Text(text.into()))
            .map_err(proxy_transport_error)
    }

    fn read(&mut self) -> Result<ReadOutcome, CodexProviderSourceError> {
        Err(CodexProviderSourceError::protocol(
            "proxy write adapter cannot read",
        ))
    }

    fn close(&mut self) -> Result<(), CodexProviderSourceError> {
        Ok(())
    }
}

fn lock_approvals(approvals: &SharedApprovalState) -> MutexGuard<'_, ApprovalRuntimeState> {
    approvals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_mapper(mapper: &Arc<Mutex<CodexEventMapper>>) -> MutexGuard<'_, CodexEventMapper> {
    mapper
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct ManagedCodexRuntime {
    process: Option<ManagedProcess>,
    owns_runtime_directory: bool,
}

impl AppServerRuntime for ManagedCodexRuntime {
    fn start(
        &mut self,
        config: &CodexProviderConfig,
    ) -> Result<Box<dyn AppServerIo>, CodexProviderSourceError> {
        #[cfg(not(unix))]
        {
            let _ = config;
            Err(CodexProviderSourceError::UnsupportedPlatform)
        }

        #[cfg(unix)]
        {
            if let CodexCliCompatibility::UnverifiedNewer(version) = verify_codex_version(config)? {
                eprintln!(
                    "warning: Codex CLI {version} is newer than the latest verified version {}; \
                     starting best-effort with the strict {} App Server schema, and any protocol \
                     incompatibility will fail the Codex Provider",
                    crate::SUPPORTED_CODEX_CLI_VERSION,
                    crate::SUPPORTED_CODEX_CLI_VERSION,
                );
            }
            fs::create_dir_all(&config.runtime_root)
                .map_err(|error| CodexProviderSourceError::runtime("root creation", error))?;
            match fs::create_dir(&config.runtime_directory) {
                Ok(()) => self.owns_runtime_directory = true,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(CodexProviderSourceError::RuntimePathOccupied {
                        path: config.runtime_directory.clone(),
                    });
                }
                Err(error) => {
                    return Err(CodexProviderSourceError::runtime(
                        "private directory creation",
                        error,
                    ));
                }
            }
            fs::set_permissions(&config.runtime_directory, fs::Permissions::from_mode(0o700))
                .map_err(|error| {
                    CodexProviderSourceError::runtime("private directory permissions", error)
                })?;

            let mut command = Command::new(&config.codex_executable);
            command
                .arg("app-server")
                .arg("--listen")
                .arg(&config.app_server_uri)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            command.process_group(0);
            let child = command
                .spawn()
                .map_err(|error| CodexProviderSourceError::runtime("process launch", error))?;
            self.process = Some(ManagedProcess::new_in_own_process_group(child)?);

            let deadline = Instant::now() + config.startup_timeout;
            loop {
                if let Some(process) = self.process.as_mut() {
                    match process.try_wait() {
                        Ok(Some(status)) => {
                            let stderr = process.stderr_snapshot();
                            return Err(CodexProviderSourceError::ProcessExited {
                                status: display_exit_status(status),
                                stderr,
                            });
                        }
                        Ok(None) => {}
                        Err(error) => {
                            return Err(CodexProviderSourceError::runtime("process status", error));
                        }
                    }
                }
                if config.socket_path.exists() {
                    match UnixStream::connect(&config.socket_path) {
                        Ok(stream) => {
                            stream
                                .set_read_timeout(Some(IO_POLL_INTERVAL))
                                .map_err(|error| {
                                    CodexProviderSourceError::runtime("socket read timeout", error)
                                })?;
                            stream
                                .set_write_timeout(Some(IO_POLL_INTERVAL))
                                .map_err(|error| {
                                    CodexProviderSourceError::runtime("socket write timeout", error)
                                })?;
                            let (socket, _) = tungstenite::client("ws://localhost/", stream)
                                .map_err(|error| {
                                    CodexProviderSourceError::transport(format!(
                                        "Unix WebSocket handshake failed: {error}"
                                    ))
                                })?;
                            return Ok(Box::new(UnixWebSocketIo { socket }));
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                            ) => {}
                        Err(error) => {
                            return Err(CodexProviderSourceError::runtime(
                                "socket connection",
                                error,
                            ));
                        }
                    }
                }
                if Instant::now() >= deadline {
                    return Err(CodexProviderSourceError::StartupTimeout {
                        timeout: config.startup_timeout,
                    });
                }
                thread::sleep(PROCESS_POLL_INTERVAL);
            }
        }
    }

    fn stop(&mut self, config: &CodexProviderConfig) -> Result<(), CodexProviderSourceError> {
        let mut failures = Vec::new();
        if let Some(mut process) = self.process.take() {
            if let Err(error) = process.terminate()
                && error.kind() != io::ErrorKind::InvalidInput
            {
                failures.push(format!("process termination request: {error}"));
            }
            let deadline = Instant::now() + config.shutdown_timeout;
            let mut exited = false;
            while Instant::now() < deadline {
                match process.try_wait() {
                    Ok(Some(_)) => {
                        exited = true;
                        break;
                    }
                    Ok(None) => thread::sleep(PROCESS_POLL_INTERVAL),
                    Err(error) => {
                        failures.push(format!("process status: {error}"));
                        break;
                    }
                }
            }
            if !exited {
                if let Err(error) = process.kill()
                    && error.kind() != io::ErrorKind::InvalidInput
                {
                    failures.push(format!("process termination: {error}"));
                }
                if let Err(error) = process.wait() {
                    failures.push(format!("process wait: {error}"));
                }
            } else if let Err(error) = process.kill_remaining_process_group()
                && error.kind() != io::ErrorKind::InvalidInput
            {
                failures.push(format!("remaining process group termination: {error}"));
            }
            process.join_stderr(IO_POLL_INTERVAL);
        }

        #[cfg(unix)]
        if self.owns_runtime_directory {
            if config.socket_path.exists()
                && let Err(error) = fs::remove_file(&config.socket_path)
            {
                failures.push(format!("socket removal: {error}"));
            }
            if let Err(error) = fs::remove_dir(&config.runtime_directory) {
                if error.kind() != io::ErrorKind::NotFound {
                    failures.push(format!("private directory removal: {error}"));
                }
            } else {
                self.owns_runtime_directory = false;
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(CodexProviderSourceError::Shutdown {
                message: failures.join("; "),
            })
        }
    }
}

#[cfg(unix)]
fn verify_codex_version(
    config: &CodexProviderConfig,
) -> Result<CodexCliCompatibility, CodexProviderSourceError> {
    let output = Command::new(&config.codex_executable)
        .arg("--version")
        .output()
        .map_err(|error| CodexProviderSourceError::VersionProbe {
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(CodexProviderSourceError::VersionProbe {
            message: format!("process exited with {}", output.status),
        });
    }
    let actual = String::from_utf8(output.stdout)
        .map_err(|error| CodexProviderSourceError::VersionProbe {
            message: format!("stdout was not UTF-8: {error}"),
        })?
        .trim()
        .to_owned();
    classify_codex_version(&actual)
}

#[cfg(unix)]
fn classify_codex_version(actual: &str) -> Result<CodexCliCompatibility, CodexProviderSourceError> {
    let Some(version_text) = actual.strip_prefix("codex-cli ") else {
        return Err(version_mismatch(actual));
    };
    if crate::SUPPORTED_CODEX_CLI_VERSIONS.contains(&version_text) {
        return Ok(CodexCliCompatibility::Verified);
    }
    let version = Version::parse(version_text).map_err(|_| version_mismatch(actual))?;
    let actual_core = (version.major, version.minor, version.patch);
    if actual_core > LATEST_VERIFIED_CODEX_CLI_CORE {
        Ok(CodexCliCompatibility::UnverifiedNewer(version))
    } else {
        Err(version_mismatch(actual))
    }
}

#[cfg(unix)]
fn version_mismatch(actual: &str) -> CodexProviderSourceError {
    CodexProviderSourceError::VersionMismatch {
        expected: crate::SUPPORTED_CODEX_CLI_VERSION_REQUIREMENT,
        actual: actual.to_owned(),
    }
}

struct ManagedProcess {
    child: Child,
    #[cfg(unix)]
    process_group: Option<i32>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    stderr_thread: Option<JoinHandle<()>>,
    stderr_done: mpsc::Receiver<()>,
}

impl ManagedProcess {
    #[cfg(test)]
    fn new(child: Child) -> Result<Self, CodexProviderSourceError> {
        Self::new_with_process_group(child, None)
    }

    #[cfg(unix)]
    fn new_in_own_process_group(child: Child) -> Result<Self, CodexProviderSourceError> {
        let process_group = i32::try_from(child.id()).map_err(|_| {
            CodexProviderSourceError::runtime(
                "process group capture",
                "child PID exceeds the supported process-group range",
            )
        })?;
        Self::new_with_process_group(child, Some(process_group))
    }

    fn new_with_process_group(
        mut child: Child,
        #[cfg(unix)] process_group: Option<i32>,
        #[cfg(not(unix))] _process_group: Option<i32>,
    ) -> Result<Self, CodexProviderSourceError> {
        let Some(stderr_pipe) = child.stderr.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CodexProviderSourceError::runtime(
                "stderr capture",
                "stderr pipe was unavailable",
            ));
        };
        let stderr = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_LIMIT)));
        let worker_buffer = Arc::clone(&stderr);
        let (stderr_done_sender, stderr_done) = mpsc::channel();
        let stderr_thread = match thread::Builder::new()
            .name("agentpulse-codex-stderr".to_owned())
            .spawn(move || {
                capture_stderr(stderr_pipe, &worker_buffer);
                let _ = stderr_done_sender.send(());
            }) {
            Ok(worker) => worker,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CodexProviderSourceError::runtime(
                    "stderr reader thread spawn",
                    error,
                ));
            }
        };
        Ok(Self {
            child,
            #[cfg(unix)]
            process_group,
            stderr,
            stderr_thread: Some(stderr_thread),
            stderr_done,
        })
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    #[cfg(unix)]
    fn terminate(&mut self) -> io::Result<()> {
        use nix::{
            errno::Errno,
            sys::signal::{Signal, kill},
            unistd::Pid,
        };

        let child_pid = i32::try_from(self.child.id())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "child PID exceeds i32"))?;
        let target = self.process_group.map_or(child_pid, |group| -group);
        match kill(Pid::from_raw(target), Signal::SIGTERM) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(io::Error::from(error)),
        }
    }

    #[cfg(not(unix))]
    fn terminate(&mut self) -> io::Result<()> {
        self.kill()
    }

    #[cfg(unix)]
    fn kill(&mut self) -> io::Result<()> {
        use nix::{
            errno::Errno,
            sys::signal::{Signal, kill},
            unistd::Pid,
        };

        if let Some(process_group) = self.process_group {
            return match kill(Pid::from_raw(-process_group), Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => Ok(()),
                Err(error) => Err(io::Error::from(error)),
            };
        }
        self.child.kill()
    }

    #[cfg(not(unix))]
    fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    #[cfg(unix)]
    fn kill_remaining_process_group(&mut self) -> io::Result<()> {
        use nix::{
            errno::Errno,
            sys::signal::{Signal, kill},
            unistd::Pid,
        };

        let Some(process_group) = self.process_group else {
            return Ok(());
        };
        match kill(Pid::from_raw(-process_group), Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(io::Error::from(error)),
        }
    }

    #[cfg(not(unix))]
    fn kill_remaining_process_group(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    fn stderr_snapshot(&self) -> String {
        let bytes = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }

    fn join_stderr(&mut self, timeout: Duration) {
        if self.stderr_done.recv_timeout(timeout).is_ok()
            && let Some(worker) = self.stderr_thread.take()
        {
            let _ = worker.join();
        }
        let _ = self.stderr_thread.take();
    }
}

fn capture_stderr(mut stderr: impl Read, buffer: &Arc<Mutex<VecDeque<u8>>>) {
    let mut chunk = [0_u8; 4_096];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(length) => {
                let mut buffer = buffer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for byte in &chunk[..length] {
                    if buffer.len() == STDERR_LIMIT {
                        let _ = buffer.pop_front();
                    }
                    buffer.push_back(*byte);
                }
            }
        }
    }
}

fn display_exit_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| code.to_string(),
    )
}

#[cfg(unix)]
struct UnixWebSocketIo {
    socket: WebSocket<UnixStream>,
}

#[cfg(unix)]
impl AppServerIo for UnixWebSocketIo {
    fn write_text(&mut self, text: String) -> Result<(), CodexProviderSourceError> {
        self.socket
            .send(Message::Text(text.into()))
            .map_err(|error| CodexProviderSourceError::transport(error.to_string()))
    }

    fn read(&mut self) -> Result<ReadOutcome, CodexProviderSourceError> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return Ok(ReadOutcome::Text(text.to_string())),
                Ok(Message::Ping(payload)) => {
                    self.socket
                        .send(Message::Pong(payload))
                        .map_err(|error| CodexProviderSourceError::transport(error.to_string()))?;
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => return Ok(ReadOutcome::Closed),
                Ok(Message::Binary(_)) => {
                    return Err(CodexProviderSourceError::protocol(
                        "binary App Server frames are unsupported",
                    ));
                }
                Ok(Message::Frame(_)) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(ReadOutcome::Timeout);
                }
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Ok(ReadOutcome::Closed);
                }
                Err(error) => {
                    return Err(CodexProviderSourceError::transport(error.to_string()));
                }
            }
        }
    }

    fn close(&mut self) -> Result<(), CodexProviderSourceError> {
        match self.socket.close(None) {
            Ok(())
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(())
            }
            Err(error) => Err(CodexProviderSourceError::transport(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        error::Error,
        fmt,
        net::TcpStream,
        str::FromStr,
        sync::{Arc, Mutex, TryLockError},
        thread,
        time::{Duration, Instant},
    };

    #[cfg(unix)]
    use std::{
        fs,
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
        path::PathBuf,
    };

    use agentpulse_bridge::{ChannelActionHandle, ChannelActionSource, ChannelPort, RuntimeHost};
    use agentpulse_channel_native::{
        NATIVE_WEBSOCKET_PATH, NATIVE_WEBSOCKET_SUBPROTOCOL, NativeChannel, NativeChannelConfig,
        NativeClientMessage, NativeDeliveryContext, NativeServerMessage, NativeSubscriptionStatus,
        decode_server_message, encode_client_message,
    };
    use agentpulse_core::{
        AgentEvent, AgentEventPayload, AgentSession, AgentState, ApprovalSelection,
        ChannelCapabilities, ChannelDescriptor, ChannelEventRoute, ChannelId, ChannelKind,
        ConnectionState, EventSequence, InteractionRequestPayload, InteractionResponse,
        InteractionResponsePayload, NonEmptyText, ProviderCapabilities, ProviderDescriptor,
        ProviderId, ProviderKind, SessionId, SessionOutcome, Timestamp,
    };
    use agentpulse_protocol::{ProtocolMessage, V2_PROTOCOL_VERSION};
    use tungstenite::{ClientRequestBuilder, Message, WebSocket, client, http::Uri};

    use super::*;
    use crate::{CodexProviderPort, status::snapshot};

    type TestResult = Result<(), Box<dyn Error>>;

    const THREAD_ID: &str = "019976a4-00f0-7312-b36c-d01f9c5c06f6";
    const SECOND_THREAD_ID: &str = "019976a4-00f4-7561-a2a4-156c98eb31bc";
    const LIVE_FIXTURE: &str = include_str!("../tests/fixtures/live_success.jsonl");

    #[test]
    fn model_selection_requires_an_advertised_model_and_effort() -> TestResult {
        let catalog = json!({
            "data": [{
                "id": "gpt-test",
                "displayName": "GPT Test",
                "supportedReasoningEfforts": [
                    {"reasoningEffort": "low", "description": "Fast"},
                    {"reasoningEffort": "high", "description": "Thorough"}
                ]
            }]
        });

        assert_eq!(validate_model_selection(&catalog, "gpt-test", None), Ok(()));
        assert_eq!(
            validate_model_selection(&catalog, "gpt-test", Some("high")),
            Ok(())
        );
        assert_eq!(
            validate_model_selection(&catalog, "missing", None),
            Err("unknown model `missing`; run /model to list valid IDs".to_owned())
        );
        assert_eq!(
            validate_model_selection(&catalog, "gpt-test", Some("maximum")),
            Err(
                "effort `maximum` is not supported by `gpt-test`; valid efforts: low, high"
                    .to_owned()
            )
        );
        assert!(
            format_catalog(ExpectedResponse::ModelList, &catalog, None)?
                .contains("efforts: low, high")
        );
        Ok(())
    }
    const SERVER_REQUEST: &str = include_str!("../tests/fixtures/server_request.json");
    const INVALID_NOTIFICATION: &str = include_str!("../tests/fixtures/invalid_notification.json");

    #[derive(Default)]
    struct FakeControl {
        incoming: Mutex<VecDeque<Result<ReadOutcome, String>>>,
        outgoing: Mutex<Vec<String>>,
        starts: Mutex<u64>,
        stops: Mutex<u64>,
        closes: Mutex<u64>,
    }

    impl FakeControl {
        fn push_text(&self, text: impl Into<String>) {
            locked(&self.incoming).push_back(Ok(ReadOutcome::Text(text.into())));
        }

        fn push_failure(&self, message: impl Into<String>) {
            locked(&self.incoming).push_back(Err(message.into()));
        }
    }

    struct FakeIo {
        control: Arc<FakeControl>,
    }

    impl AppServerIo for FakeIo {
        fn write_text(&mut self, text: String) -> Result<(), CodexProviderSourceError> {
            locked(&self.control.outgoing).push(text);
            Ok(())
        }

        fn read(&mut self) -> Result<ReadOutcome, CodexProviderSourceError> {
            if let Some(outcome) = locked(&self.control.incoming).pop_front() {
                outcome.map_err(CodexProviderSourceError::transport)
            } else {
                thread::sleep(Duration::from_millis(2));
                Ok(ReadOutcome::Timeout)
            }
        }

        fn close(&mut self) -> Result<(), CodexProviderSourceError> {
            *locked(&self.control.closes) += 1;
            Ok(())
        }
    }

    struct FakeRuntime {
        control: Arc<FakeControl>,
    }

    impl AppServerRuntime for FakeRuntime {
        fn start(
            &mut self,
            _config: &CodexProviderConfig,
        ) -> Result<Box<dyn AppServerIo>, CodexProviderSourceError> {
            *locked(&self.control.starts) += 1;
            Ok(Box::new(FakeIo {
                control: Arc::clone(&self.control),
            }))
        }

        fn stop(&mut self, _config: &CodexProviderConfig) -> Result<(), CodexProviderSourceError> {
            *locked(&self.control.stops) += 1;
            Ok(())
        }
    }

    #[cfg(unix)]
    #[derive(Default)]
    struct ProxyTestControl {
        approval_response: Mutex<Option<String>>,
        server_error: Mutex<Option<String>>,
        stop: AtomicBool,
    }

    #[cfg(unix)]
    struct ProxyTestRuntime {
        observer: Arc<FakeControl>,
        proxy: Arc<ProxyTestControl>,
        worker: Option<JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl AppServerRuntime for ProxyTestRuntime {
        fn start(
            &mut self,
            config: &CodexProviderConfig,
        ) -> Result<Box<dyn AppServerIo>, CodexProviderSourceError> {
            fs::create_dir_all(&config.runtime_directory).map_err(|error| {
                CodexProviderSourceError::runtime("proxy test directory creation", error)
            })?;
            let listener = UnixListener::bind(&config.socket_path).map_err(|error| {
                CodexProviderSourceError::runtime("proxy test upstream bind", error)
            })?;
            listener.set_nonblocking(true).map_err(|error| {
                CodexProviderSourceError::runtime("proxy test upstream nonblocking mode", error)
            })?;
            let control = Arc::clone(&self.proxy);
            self.worker = Some(
                thread::Builder::new()
                    .name("agentpulse-proxy-test-upstream".to_owned())
                    .spawn(move || proxy_test_upstream(listener, control))
                    .map_err(|error| {
                        CodexProviderSourceError::runtime("proxy test upstream thread", error)
                    })?,
            );
            Ok(Box::new(FakeIo {
                control: Arc::clone(&self.observer),
            }))
        }

        fn stop(&mut self, config: &CodexProviderConfig) -> Result<(), CodexProviderSourceError> {
            self.proxy.stop.store(true, Ordering::Release);
            let _ = UnixStream::connect(&config.socket_path);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            for path in [&config.proxy_socket_path, &config.socket_path] {
                if let Err(error) = fs::remove_file(path)
                    && error.kind() != io::ErrorKind::NotFound
                {
                    return Err(CodexProviderSourceError::runtime(
                        "proxy test socket removal",
                        error,
                    ));
                }
            }
            if let Err(error) = fs::remove_dir(&config.runtime_directory)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(CodexProviderSourceError::runtime(
                    "proxy test runtime directory removal",
                    error,
                ));
            }
            if let Err(error) = fs::remove_dir(&config.runtime_root)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(CodexProviderSourceError::runtime(
                    "proxy test root removal",
                    error,
                ));
            }
            Ok(())
        }
    }

    #[cfg(unix)]
    fn proxy_test_upstream(listener: UnixListener, control: Arc<ProxyTestControl>) {
        while !control.stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) = proxy_test_exchange(stream, &control)
                        && !control.stop.load(Ordering::Acquire)
                    {
                        *locked(&control.server_error) = Some(error);
                    }
                    return;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    *locked(&control.server_error) = Some(error.to_string());
                    return;
                }
            }
        }
    }

    #[cfg(unix)]
    fn proxy_test_exchange(stream: UnixStream, control: &ProxyTestControl) -> Result<(), String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| error.to_string())?;
        let mut socket = tungstenite::accept(stream).map_err(|error| error.to_string())?;
        let first = socket.read().map_err(|error| error.to_string())?;
        if !matches!(first, Message::Text(_)) {
            return Err("proxy did not forward the desktop request".to_owned());
        }
        let frames = [
            r#"{"id":77,"result":{"proxied":true}}"#.to_owned(),
            format!(
                r#"{{"method":"turn/started","params":{{"threadId":"{THREAD_ID}","turn":{{"id":"019976a4-00f1-76c0-b845-e1509dc4e3de","items":[],"startedAt":102,"status":"inProgress"}}}}}}"#,
            ),
            format!(
                r#"{{"method":"item/started","params":{{"item":{{"command":"touch /var/tmp/agentpulse-proxy-test","commandActions":[],"cwd":"/workspace","id":"019976a4-00f2-741b-870f-21b4fb983746","status":"inProgress","type":"commandExecution"}},"startedAtMs":103000,"threadId":"{THREAD_ID}","turnId":"019976a4-00f1-76c0-b845-e1509dc4e3de"}}}}"#,
            ),
            format!(
                r#"{{"id":"proxy-approval","method":"item/commandExecution/requestApproval","params":{{"command":"touch /var/tmp/agentpulse-proxy-test","cwd":"/workspace","itemId":"019976a4-00f2-741b-870f-21b4fb983746","reason":"Verify proxy routing","startedAtMs":103000,"threadId":"{THREAD_ID}","turnId":"019976a4-00f1-76c0-b845-e1509dc4e3de"}}}}"#,
            ),
        ];
        for frame in frames {
            socket
                .send(Message::Text(frame.into()))
                .map_err(|error| error.to_string())?;
        }

        loop {
            match socket.read() {
                Ok(Message::Text(text)) if text.contains("\"id\":\"proxy-approval\"") => {
                    *locked(&control.approval_response) = Some(text.to_string());
                    socket
                        .send(Message::Text(
                            format!(
                                r#"{{"method":"serverRequest/resolved","params":{{"requestId":"proxy-approval","threadId":"{THREAD_ID}"}}}}"#,
                            )
                            .into(),
                        ))
                        .map_err(|error| error.to_string())?;
                    return Ok(());
                }
                Ok(Message::Ping(payload)) => socket
                    .send(Message::Pong(payload))
                    .map_err(|error| error.to_string())?,
                Ok(Message::Pong(_) | Message::Frame(_)) => {}
                Ok(Message::Close(_)) => return Err("desktop closed before approval".to_owned()),
                Ok(Message::Text(_) | Message::Binary(_)) => {}
                Err(tungstenite::Error::Io(error)) if is_timeout(&error) => {
                    if control.stop.load(Ordering::Acquire) {
                        return Ok(());
                    }
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn provider_descriptor(
        provider_id: ProviderId,
    ) -> Result<ProviderDescriptor, agentpulse_core::DomainError> {
        Ok(ProviderDescriptor::new(
            provider_id,
            ProviderKind::new("codex")?,
            NonEmptyText::new("Codex Test")?,
            ProviderCapabilities::SESSION_STATE
                | ProviderCapabilities::APPROVAL_REQUEST
                | ProviderCapabilities::APPROVAL_RESPONSE
                | ProviderCapabilities::PROMPT_SUBMIT
                | ProviderCapabilities::CONTROL,
        ))
    }

    fn test_provider(
        control: Arc<FakeControl>,
    ) -> Result<
        (
            ProviderId,
            CodexProviderPort,
            CodexProviderSource,
            SharedStatus,
        ),
        crate::CodexProviderBuildError,
    > {
        test_provider_with_threads(control, &[THREAD_ID])
    }

    fn test_provider_with_threads(
        control: Arc<FakeControl>,
        thread_ids: &[&str],
    ) -> Result<
        (
            ProviderId,
            CodexProviderPort,
            CodexProviderSource,
            SharedStatus,
        ),
        crate::CodexProviderBuildError,
    > {
        let provider_id = ProviderId::new();
        let config =
            CodexProviderConfig::new(provider_id, "/tmp/ap-test", thread_ids.iter().copied())?;
        let schema = ProtocolSchema::compile()?;
        let mapper = CodexEventMapper::new(provider_id, &config.threads, config.discover_threads);
        let status = Arc::new(Mutex::new(Default::default()));
        let approvals = Arc::new(Mutex::new(ApprovalRuntimeState::new()));
        let controls = Arc::new(Mutex::new(ControlRuntimeState::new()));
        let source = CodexProviderSource::with_runtime_and_states(
            config,
            schema,
            mapper,
            Arc::clone(&status),
            Arc::clone(&approvals),
            Arc::clone(&controls),
            Box::new(FakeRuntime { control }),
        );
        let descriptor = provider_descriptor(provider_id)?;
        Ok((
            provider_id,
            CodexProviderPort::new(descriptor, approvals, controls),
            source,
            status,
        ))
    }

    fn test_discovering_provider(
        control: Arc<FakeControl>,
    ) -> Result<
        (
            ProviderId,
            CodexProviderPort,
            CodexProviderSource,
            SharedStatus,
        ),
        crate::CodexProviderBuildError,
    > {
        let provider_id = ProviderId::new();
        let config = CodexProviderConfig::discovering(provider_id, "/tmp/ap-test")?;
        let schema = ProtocolSchema::compile()?;
        let mapper = CodexEventMapper::new(provider_id, &config.threads, config.discover_threads);
        let status = Arc::new(Mutex::new(Default::default()));
        let approvals = Arc::new(Mutex::new(ApprovalRuntimeState::new()));
        let controls = Arc::new(Mutex::new(ControlRuntimeState::new()));
        let source = CodexProviderSource::with_runtime_and_states(
            config,
            schema,
            mapper,
            Arc::clone(&status),
            Arc::clone(&approvals),
            Arc::clone(&controls),
            Box::new(FakeRuntime { control }),
        );
        let descriptor = provider_descriptor(provider_id)?;
        Ok((
            provider_id,
            CodexProviderPort::new(descriptor, approvals, controls),
            source,
            status,
        ))
    }

    fn seed_lines(control: &FakeControl, lines: impl Iterator<Item = &'static str>) {
        for line in lines {
            control.push_text(line);
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> Result<(), io::Error> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if condition() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "condition was not reached",
        ))
    }

    fn native_connect(
        address: std::net::SocketAddr,
    ) -> Result<WebSocket<TcpStream>, Box<dyn Error>> {
        let uri: Uri = format!("ws://{address}{NATIVE_WEBSOCKET_PATH}").parse()?;
        let request =
            ClientRequestBuilder::new(uri).with_sub_protocol(NATIVE_WEBSOCKET_SUBPROTOCOL);
        let stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let (socket, _) = client(request, stream)?;
        Ok(socket)
    }

    fn native_send(socket: &mut WebSocket<TcpStream>, message: &NativeClientMessage) -> TestResult {
        let text = String::from_utf8(encode_client_message(message)?)?;
        socket.send(Message::text(text))?;
        Ok(())
    }

    fn native_read(
        socket: &mut WebSocket<TcpStream>,
    ) -> Result<NativeServerMessage, Box<dyn Error>> {
        loop {
            match socket.read()? {
                Message::Text(text) => return Ok(decode_server_message(text.as_bytes())?),
                Message::Ping(_) | Message::Pong(_) => socket.flush()?,
                Message::Close(_) => return Err("Native server closed unexpectedly".into()),
                Message::Binary(_) | Message::Frame(_) => {
                    return Err("Native server emitted a non-text application frame".into());
                }
            }
        }
    }

    fn native_handshake_and_subscribe(
        socket: &mut WebSocket<TcpStream>,
        provider_id: ProviderId,
        session_id: SessionId,
    ) -> TestResult {
        native_send(
            socket,
            &NativeClientMessage::Hello {
                client_id: ChannelId::new().to_string(),
                display_name: "Codex Fixture Native Client".to_owned(),
                version: Some("1.0.0-test".to_owned()),
                supported_protocol_versions: vec![V2_PROTOCOL_VERSION],
                host_run_id: None,
                session_cursors: Default::default(),
            },
        )?;
        assert!(matches!(
            native_read(socket)?,
            NativeServerMessage::Hello { .. }
        ));
        let discovery_id = ChannelId::new().to_string();
        native_send(
            socket,
            &NativeClientMessage::Discover {
                request_id: discovery_id.clone(),
            },
        )?;
        assert!(matches!(
            native_read(socket)?,
            NativeServerMessage::SyncStarted {
                provider_count: 1,
                session_count: 1,
                ..
            }
        ));
        assert!(matches!(
            native_read(socket)?,
            NativeServerMessage::Domain {
                context: NativeDeliveryContext::DiscoveryProvider { .. },
                message,
            } if matches!(message.as_ref(), ProtocolMessage::ProviderDescriptor(descriptor) if descriptor.id() == provider_id)
        ));
        assert!(matches!(
            native_read(socket)?,
            NativeServerMessage::Domain {
                context: NativeDeliveryContext::DiscoverySession { last_sequence, .. },
                message,
            } if matches!(message.as_ref(), ProtocolMessage::AgentSession(session) if session.id() == session_id)
                && last_sequence == EventSequence::FIRST
        ));
        assert!(matches!(
            native_read(socket)?,
            NativeServerMessage::SyncCompleted { ref request_id }
                if request_id == &discovery_id
        ));
        let subscription_id = ChannelId::new().to_string();
        native_send(
            socket,
            &NativeClientMessage::Subscribe {
                request_id: subscription_id.clone(),
                session_id,
            },
        )?;
        assert!(matches!(
            native_read(socket)?,
            NativeServerMessage::SubscriptionResult {
                status: NativeSubscriptionStatus::Subscribed,
                baseline_sequence: EventSequence::FIRST,
                ..
            }
        ));
        assert!(matches!(
            native_read(socket)?,
            NativeServerMessage::Domain {
                context: NativeDeliveryContext::SubscriptionSession { .. },
                message,
            } if matches!(message.as_ref(), ProtocolMessage::AgentSession(session) if session.id() == session_id)
        ));
        Ok(())
    }

    #[test]
    fn discovering_provider_follows_thread_started_without_saved_ids() -> TestResult {
        let control = Arc::new(FakeControl::default());
        let initialize = LIVE_FIXTURE
            .lines()
            .next()
            .ok_or("fixture has no initialize response")?;
        let thread_started = LIVE_FIXTURE
            .lines()
            .find(|line| line.contains("\"method\":\"thread/started\""))
            .ok_or("fixture has no thread/started notification")?;
        control.push_text(initialize);
        let (_provider_id, port, source, status) = test_discovering_provider(Arc::clone(&control))?;
        let session_id = SessionId::from_str(THREAD_ID)?;
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        let _ = host.start()?;

        assert_eq!(locked(&control.outgoing).len(), 2);
        control.push_text(thread_started);
        wait_until(|| snapshot(&status).mapped_events() == 1)?;
        assert!(host.inspect_bridge(|bridge| bridge.session_aggregate(session_id).is_some())?);
        assert_eq!(snapshot(&status).health(), CodexProviderHealth::Running);
        let _ = host.stop()?;
        Ok(())
    }

    #[test]
    #[ignore = "requires loopback socket access"]
    fn captured_codex_fixture_streams_through_native_transport() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (provider_id, provider, provider_source, codex_status) =
            test_provider(Arc::clone(&control))?;
        let session_id = SessionId::from_str(THREAD_ID)?;
        let channel_id = ChannelId::new();
        let native = NativeChannel::build(NativeChannelConfig::new(channel_id))?;
        let (channel, channel_source, native_handle) = native.into_parts();
        let mut host = RuntimeHost::new();
        host.register_provider(provider, provider_source)?;
        host.register_channel(channel, channel_source)?;
        let _ = host.start()?;
        wait_until(|| snapshot(&codex_status).mapped_events() == 1)?;

        let address = native_handle
            .snapshot()
            .local_address
            .ok_or("Native listener address is unavailable")?;
        let mut client = native_connect(address)?;
        native_handshake_and_subscribe(&mut client, provider_id, session_id)?;
        seed_lines(&control, LIVE_FIXTURE.lines().skip(2));

        let mut event_sequences = Vec::new();
        let mut final_message_seen = false;
        while event_sequences.last().copied() != Some(EventSequence::new(6)?) {
            if let NativeServerMessage::Domain {
                context: NativeDeliveryContext::LiveEvent { .. },
                message,
            } = native_read(&mut client)?
                && let ProtocolMessage::AgentEvent(event) = *message
            {
                if let AgentEventPayload::Message(message) = event.payload()
                    && message.content().as_str() == "Provider fixture completed"
                {
                    final_message_seen = true;
                }
                event_sequences.push(event.sequence());
            }
        }
        assert_eq!(
            event_sequences,
            (2_u64..=6)
                .map(EventSequence::new)
                .collect::<Result<Vec<_>, _>>()?
        );
        assert!(final_message_seen);
        wait_until(|| snapshot(&codex_status).mapped_events() == 6)?;
        client.close(None)?;
        let _ = host.stop()?;
        Ok(())
    }

    #[test]
    fn captured_stream_runs_through_runtime_host_and_restarts_without_duplicate_session()
    -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines());
        let (_provider_id, port, source, status) = test_provider(Arc::clone(&control))?;
        let session_id = SessionId::from_str(THREAD_ID)?;
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        let _ = host.start()?;

        wait_until(|| snapshot(&status).mapped_events() == 6)?;
        let aggregate = host.inspect_bridge(|bridge| {
            bridge.session_aggregate(session_id).map(|aggregate| {
                (
                    aggregate.session().state(),
                    aggregate.last_sequence().get(),
                    aggregate.latest_outcome().cloned(),
                )
            })
        })?;
        let Some((state, sequence, outcome)) = aggregate else {
            return Err("session aggregate was not created".into());
        };
        assert_eq!(state, AgentState::Completed);
        assert_eq!(sequence, 6);
        assert!(matches!(
            outcome,
            Some(SessionOutcome::Completed { summary: Some(summary) })
                if summary.as_str() == "Provider fixture completed"
        ));
        assert_eq!(snapshot(&status).validated_unmapped_frames(), 1);
        assert_eq!(locked(&control.outgoing).len(), 3);
        let _ = host.stop()?;

        let mut fixture_lines = LIVE_FIXTURE.lines();
        if let Some(initialize) = fixture_lines.next() {
            control.push_text(initialize);
        }
        if let Some(resume) = fixture_lines.next() {
            control.push_text(resume);
        }
        let _ = host.start()?;
        wait_until(|| snapshot(&status).mapped_events() == 9)?;
        let restart_state = host.inspect_bridge(|bridge| {
            (
                bridge.session_aggregates().count(),
                bridge
                    .session_aggregate(session_id)
                    .map(|aggregate| aggregate.last_sequence().get()),
            )
        })?;
        assert_eq!(restart_state, (1, Some(9)));
        let _ = host.stop()?;
        assert_eq!(*locked(&control.starts), 2);
        assert_eq!(*locked(&control.stops), 2);
        Ok(())
    }

    #[test]
    fn invalid_live_frame_marks_provider_failed_and_disconnects_session() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (_provider_id, port, source, status) = test_provider(Arc::clone(&control))?;
        let session_id = SessionId::from_str(THREAD_ID)?;
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        let _ = host.start()?;
        control.push_text(INVALID_NOTIFICATION.trim());

        wait_until(|| snapshot(&status).health() == CodexProviderHealth::Failed)?;
        let connection = host.inspect_bridge(|bridge| {
            bridge
                .session_aggregate(session_id)
                .map(|aggregate| aggregate.session().connection_state())
        })?;
        assert_eq!(connection, Some(ConnectionState::Disconnected));
        assert!(snapshot(&status).last_error().is_some());
        let _ = host.stop()?;
        Ok(())
    }

    #[test]
    fn approval_request_remains_pending_without_an_agentpulse_timeout() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (_provider_id, port, source, status) = test_provider(Arc::clone(&control))?;
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        let _ = host.start()?;
        control.push_text(SERVER_REQUEST.trim());

        wait_until(|| snapshot(&status).mapped_events() == 2)?;
        assert_eq!(snapshot(&status).health(), CodexProviderHealth::Running);
        assert_eq!(snapshot(&status).rejected_server_requests(), 0);
        let session_id = SessionId::from_str(THREAD_ID)?;
        let pending = host.inspect_bridge(|bridge| {
            bridge
                .session_aggregate(session_id)
                .map(|aggregate| aggregate.pending_interactions().count())
        })?;
        assert_eq!(pending, Some(1));
        thread::sleep(Duration::from_millis(50));
        assert_eq!(locked(&control.outgoing).len(), 3);
        let _ = host.stop()?;
        Ok(())
    }

    #[derive(Debug)]
    struct TestChannelError;

    impl fmt::Display for TestChannelError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("intentional Channel rejection")
        }
    }

    impl Error for TestChannelError {}

    struct FailingChannel {
        descriptor: ChannelDescriptor,
    }

    impl ChannelPort for FailingChannel {
        type Error = TestChannelError;

        fn descriptor(&self) -> &ChannelDescriptor {
            &self.descriptor
        }

        fn deliver_event(
            &mut self,
            _event: AgentEvent,
            _route: ChannelEventRoute,
        ) -> Result<(), Self::Error> {
            Err(TestChannelError)
        }

        fn deliver_session(&mut self, _session: AgentSession) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct NoopChannelSource;

    impl ChannelActionSource for NoopChannelSource {
        type Error = TestChannelError;

        fn start(&mut self, _actions: ChannelActionHandle) -> Result<(), Self::Error> {
            Ok(())
        }

        fn stop(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct AcceptingChannel {
        descriptor: ChannelDescriptor,
    }

    impl ChannelPort for AcceptingChannel {
        type Error = TestChannelError;

        fn descriptor(&self) -> &ChannelDescriptor {
            &self.descriptor
        }

        fn deliver_event(
            &mut self,
            _event: AgentEvent,
            _route: ChannelEventRoute,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn deliver_session(&mut self, _session: AgentSession) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct ControlLockCheckingChannel {
        descriptor: ChannelDescriptor,
        controls: SharedControlState,
        status_delivery_released_controls: Arc<Mutex<Option<bool>>>,
    }

    impl ChannelPort for ControlLockCheckingChannel {
        type Error = TestChannelError;

        fn descriptor(&self) -> &ChannelDescriptor {
            &self.descriptor
        }

        fn deliver_event(
            &mut self,
            event: AgentEvent,
            _route: ChannelEventRoute,
        ) -> Result<(), Self::Error> {
            let AgentEventPayload::Message(message) = event.payload() else {
                return Ok(());
            };
            if !message.content().as_str().starts_with("State: ") {
                return Ok(());
            }
            let released = match self.controls.try_lock() {
                Ok(_) => true,
                Err(TryLockError::WouldBlock) => false,
                Err(TryLockError::Poisoned(_)) => true,
            };
            *locked(&self.status_delivery_released_controls) = Some(released);
            Ok(())
        }

        fn deliver_session(&mut self, _session: AgentSession) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct CapturingChannelSource {
        handle: Arc<Mutex<Option<ChannelActionHandle>>>,
    }

    impl ChannelActionSource for CapturingChannelSource {
        type Error = TestChannelError;

        fn start(&mut self, actions: ChannelActionHandle) -> Result<(), Self::Error> {
            *locked(&self.handle) = Some(actions);
            Ok(())
        }

        fn stop(&mut self) -> Result<(), Self::Error> {
            *locked(&self.handle) = None;
            Ok(())
        }
    }

    #[test]
    fn status_event_delivery_does_not_hold_control_state() -> TestResult {
        let app_server = Arc::new(FakeControl::default());
        seed_lines(&app_server, LIVE_FIXTURE.lines().take(2));
        let provider_id = ProviderId::new();
        let config = CodexProviderConfig::new(provider_id, "/tmp/ap-test", [THREAD_ID])?;
        let schema = ProtocolSchema::compile()?;
        let mapper = CodexEventMapper::new(provider_id, &config.threads, config.discover_threads);
        let status = Arc::new(Mutex::new(Default::default()));
        let approvals = Arc::new(Mutex::new(ApprovalRuntimeState::new()));
        let controls = Arc::new(Mutex::new(ControlRuntimeState::new()));
        let source = CodexProviderSource::with_runtime_and_states(
            config,
            schema,
            mapper,
            Arc::clone(&status),
            Arc::clone(&approvals),
            Arc::clone(&controls),
            Box::new(FakeRuntime {
                control: app_server,
            }),
        );
        let provider = CodexProviderPort::new(
            ProviderDescriptor::new(
                provider_id,
                ProviderKind::new("codex")?,
                NonEmptyText::new("Codex Test")?,
                ProviderCapabilities::SESSION_STATE | ProviderCapabilities::CONTROL,
            ),
            approvals,
            Arc::clone(&controls),
        );
        let session_id = SessionId::from_str(THREAD_ID)?;
        let channel_id = ChannelId::new();
        let actions = Arc::new(Mutex::new(None));
        let lock_was_released = Arc::new(Mutex::new(None));
        let channel = ControlLockCheckingChannel {
            descriptor: ChannelDescriptor::new(
                channel_id,
                ChannelKind::new("test")?,
                NonEmptyText::new("Control Lock Test Channel")?,
                ChannelCapabilities::SESSION_VIEW | ChannelCapabilities::REMOTE_COMMAND,
            ),
            controls,
            status_delivery_released_controls: Arc::clone(&lock_was_released),
        };
        let mut host = RuntimeHost::new();
        host.register_provider(provider, source)?;
        host.register_channel(
            channel,
            CapturingChannelSource {
                handle: Arc::clone(&actions),
            },
        )?;
        let _ = host.start()?;
        let _ = host.subscribe(channel_id, session_id)?;
        locked(&actions)
            .as_ref()
            .ok_or("channel action handle was not started")?
            .submit_command(agentpulse_core::AgentCommand::new(
                agentpulse_core::CommandId::new(),
                session_id,
                channel_id,
                Timestamp::now_utc(),
                AgentCommandPayload::Status,
            ))?;

        wait_until(|| locked(&lock_was_released).is_some())?;
        assert_eq!(*locked(&lock_was_released), Some(true));
        let _ = host.stop()?;
        Ok(())
    }

    #[test]
    fn completed_session_starts_another_prompt() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines());
        let (_provider_id, provider, source, status) = test_provider(Arc::clone(&control))?;
        let session_id = SessionId::from_str(THREAD_ID)?;
        let channel_id = ChannelId::new();
        let actions = Arc::new(Mutex::new(None));
        let channel = AcceptingChannel {
            descriptor: ChannelDescriptor::new(
                channel_id,
                ChannelKind::new("test")?,
                NonEmptyText::new("Follow-up Prompt Channel")?,
                ChannelCapabilities::SESSION_VIEW
                    | ChannelCapabilities::REMOTE_COMMAND
                    | ChannelCapabilities::TEXT_INPUT,
            ),
        };
        let mut host = RuntimeHost::new();
        host.register_provider(provider, source)?;
        host.register_channel(
            channel,
            CapturingChannelSource {
                handle: Arc::clone(&actions),
            },
        )?;
        let _ = host.start()?;
        let _ = host.subscribe(channel_id, session_id)?;
        wait_until(|| snapshot(&status).mapped_events() == 6)?;
        let completed = host.inspect_bridge(|bridge| {
            bridge
                .session_aggregate(session_id)
                .map(|aggregate| aggregate.session().state())
        })?;
        assert_eq!(completed, Some(AgentState::Completed));

        locked(&actions)
            .as_ref()
            .ok_or("channel action handle was not started")?
            .submit_command(agentpulse_core::AgentCommand::new(
                agentpulse_core::CommandId::new(),
                session_id,
                channel_id,
                Timestamp::now_utc(),
                AgentCommandPayload::SubmitPrompt {
                    text: NonEmptyText::new("follow-up after completion")?,
                    delivery: PromptDelivery::Queue,
                },
            ))?;

        wait_until(|| {
            locked(&control.outgoing).iter().any(|frame| {
                frame.contains("\"method\":\"turn/start\"")
                    && frame.contains("follow-up after completion")
            })
        })?;
        let _ = host.stop()?;
        Ok(())
    }

    #[test]
    fn plan_mode_uses_turn_collaboration_mode_without_forged_input() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (_provider_id, provider, source, _status) = test_provider(Arc::clone(&control))?;
        let session_id = SessionId::from_str(THREAD_ID)?;
        let channel_id = ChannelId::new();
        let actions = Arc::new(Mutex::new(None));
        let channel = AcceptingChannel {
            descriptor: ChannelDescriptor::new(
                channel_id,
                ChannelKind::new("test")?,
                NonEmptyText::new("Plan Mode Channel")?,
                ChannelCapabilities::SESSION_VIEW
                    | ChannelCapabilities::REMOTE_COMMAND
                    | ChannelCapabilities::TEXT_INPUT,
            ),
        };
        let mut host = RuntimeHost::new();
        host.register_provider(provider, source)?;
        host.register_channel(
            channel,
            CapturingChannelSource {
                handle: Arc::clone(&actions),
            },
        )?;
        let _ = host.start()?;
        let _ = host.subscribe(channel_id, session_id)?;
        let handle = locked(&actions)
            .clone()
            .ok_or("channel action handle was not started")?;
        handle.submit_command(agentpulse_core::AgentCommand::new(
            agentpulse_core::CommandId::new(),
            session_id,
            channel_id,
            Timestamp::now_utc(),
            AgentCommandPayload::SetPlanMode { enabled: true },
        ))?;
        handle.submit_command(agentpulse_core::AgentCommand::new(
            agentpulse_core::CommandId::new(),
            session_id,
            channel_id,
            Timestamp::now_utc(),
            AgentCommandPayload::SubmitPrompt {
                text: NonEmptyText::new("choose a release strategy")?,
                delivery: PromptDelivery::Queue,
            },
        ))?;

        wait_until(|| {
            locked(&control.outgoing)
                .iter()
                .any(|frame| frame.contains("choose a release strategy"))
        })?;
        let frame = locked(&control.outgoing)
            .iter()
            .find(|frame| frame.contains("choose a release strategy"))
            .cloned()
            .ok_or("turn/start request was not captured")?;
        let request: serde_json::Value = serde_json::from_str(&frame)?;
        assert_eq!(request["method"], "turn/start");
        assert_eq!(
            request["params"]["input"],
            json!([{"type": "text", "text": "choose a release strategy"}])
        );
        assert_eq!(request["params"]["collaborationMode"]["mode"], "plan");
        assert_eq!(
            request["params"]["collaborationMode"]["settings"],
            json!({
                "model": "gpt-5",
                "reasoning_effort": null,
                "developer_instructions": null
            })
        );
        assert!(request["params"].get("config").is_none());
        let _ = host.stop()?;
        Ok(())
    }

    #[test]
    fn approval_response_crosses_bridge_and_waits_for_codex_resolution() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (_provider_id, port, source, status) = test_provider(Arc::clone(&control))?;
        let session_id = SessionId::from_str(THREAD_ID)?;
        let channel_id = ChannelId::new();
        let actions = Arc::new(Mutex::new(None));
        let channel = AcceptingChannel {
            descriptor: ChannelDescriptor::new(
                channel_id,
                ChannelKind::new("test")?,
                NonEmptyText::new("Approval Channel")?,
                ChannelCapabilities::SESSION_VIEW | ChannelCapabilities::APPROVAL,
            ),
        };
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        host.register_channel(
            channel,
            CapturingChannelSource {
                handle: Arc::clone(&actions),
            },
        )?;
        let _ = host.start()?;
        let _ = host.subscribe(channel_id, session_id)?;
        control.push_text(format!(
            r#"{{"id":"approval-e2e","method":"item/commandExecution/requestApproval","params":{{"itemId":"019976a4-00f2-741b-870f-21b4fb983746","command":"cargo test --workspace","cwd":"/workspace","reason":"Run tests","startedAtMs":103000,"threadId":"{THREAD_ID}","turnId":"019976a4-00f1-76c0-b845-e1509dc4e3de"}}}}"#,
        ));

        wait_until(|| snapshot(&status).mapped_events() == 2)?;
        let request = host
            .inspect_bridge(|bridge| {
                bridge
                    .session_aggregate(session_id)
                    .and_then(|aggregate| aggregate.pending_interactions().next().cloned())
            })?
            .ok_or("approval request was not reduced")?;
        let InteractionRequestPayload::Approval(approval) = request.payload() else {
            return Err("pending request was not an approval".into());
        };
        let option_id = approval
            .options()
            .iter()
            .find(|option| option.label().as_str() == "Approve for session")
            .map(|option| option.id())
            .ok_or("session approval option was not exposed")?;
        let response = InteractionResponse::new(
            request.id(),
            session_id,
            channel_id,
            Timestamp::now_utc(),
            InteractionResponsePayload::Approval(ApprovalSelection::new(option_id)),
        );
        locked(&actions)
            .as_ref()
            .ok_or("channel action handle was not started")?
            .submit_interaction_response(response)?;

        wait_until(|| {
            locked(&control.outgoing)
                .iter()
                .any(|frame| frame.contains("\"decision\":\"acceptForSession\""))
        })?;
        let still_pending = host.inspect_bridge(|bridge| {
            bridge
                .session_aggregate(session_id)
                .is_some_and(|aggregate| aggregate.pending_interaction(request.id()).is_some())
        })?;
        assert!(still_pending);

        control.push_text(format!(
            r#"{{"method":"serverRequest/resolved","params":{{"requestId":"approval-e2e","threadId":"{THREAD_ID}"}}}}"#,
        ));
        wait_until(|| {
            host.inspect_bridge(|bridge| {
                bridge
                    .session_aggregate(session_id)
                    .is_some_and(|aggregate| aggregate.pending_interaction(request.id()).is_none())
            })
            .unwrap_or(false)
        })?;
        assert_eq!(snapshot(&status).health(), CodexProviderHealth::Running);
        let _ = host.stop()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn client_proxy_routes_phone_approval_to_originating_connection() -> TestResult {
        let observer = Arc::new(FakeControl::default());
        seed_lines(&observer, LIVE_FIXTURE.lines().take(2));
        let proxy = Arc::new(ProxyTestControl::default());
        let provider_id = ProviderId::new();
        let runtime_root = std::env::temp_dir().join(format!("agentpulse-proxy-{provider_id}"));
        let config = CodexProviderConfig::new(provider_id, &runtime_root, [THREAD_ID])?;
        let proxy_socket_path = config.proxy_socket_path.clone();
        let schema = ProtocolSchema::compile()?;
        let mapper = CodexEventMapper::new(provider_id, &config.threads, config.discover_threads);
        let status = Arc::new(Mutex::new(Default::default()));
        let approvals = Arc::new(Mutex::new(ApprovalRuntimeState::new()));
        let controls = Arc::new(Mutex::new(ControlRuntimeState::new()));
        let mut source = CodexProviderSource::with_runtime_and_states(
            config,
            schema,
            mapper,
            Arc::clone(&status),
            Arc::clone(&approvals),
            Arc::clone(&controls),
            Box::new(ProxyTestRuntime {
                observer: Arc::clone(&observer),
                proxy: Arc::clone(&proxy),
                worker: None,
            }),
        );
        source.client_proxy_enabled = true;
        let port = CodexProviderPort::new(provider_descriptor(provider_id)?, approvals, controls);
        let session_id = SessionId::from_str(THREAD_ID)?;
        let channel_id = ChannelId::new();
        let actions = Arc::new(Mutex::new(None));
        let channel = AcceptingChannel {
            descriptor: ChannelDescriptor::new(
                channel_id,
                ChannelKind::new("proxy-test")?,
                NonEmptyText::new("Proxy Approval Channel")?,
                ChannelCapabilities::SESSION_VIEW | ChannelCapabilities::APPROVAL,
            ),
        };
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        host.register_channel(
            channel,
            CapturingChannelSource {
                handle: Arc::clone(&actions),
            },
        )?;
        let _ = host.start()?;
        let _ = host.subscribe(channel_id, session_id)?;

        let stream = UnixStream::connect(proxy_socket_path)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let (mut desktop, _) = tungstenite::client("ws://localhost/", stream)?;
        desktop.send(Message::Text(
            r#"{"id":77,"method":"initialize","params":{}}"#.into(),
        ))?;
        let mut approval_was_forwarded = false;
        for _ in 0..4 {
            if let Message::Text(text) = desktop.read()?
                && text.contains("\"id\":\"proxy-approval\"")
            {
                approval_was_forwarded = true;
            }
        }
        assert!(approval_was_forwarded);

        wait_until(|| {
            host.inspect_bridge(|bridge| {
                bridge
                    .session_aggregate(session_id)
                    .is_some_and(|aggregate| aggregate.pending_interactions().next().is_some())
            })
            .unwrap_or(false)
        })?;
        let request = host
            .inspect_bridge(|bridge| {
                bridge
                    .session_aggregate(session_id)
                    .and_then(|aggregate| aggregate.pending_interactions().next().cloned())
            })?
            .ok_or("proxied approval request was not reduced")?;
        let InteractionRequestPayload::Approval(approval) = request.payload() else {
            return Err("proxied interaction was not an approval".into());
        };
        let option_id = approval
            .options()
            .iter()
            .find(|option| option.label().as_str() == "Approve for session")
            .map(|option| option.id())
            .ok_or("session approval option was not exposed")?;
        let response = InteractionResponse::new(
            request.id(),
            session_id,
            channel_id,
            Timestamp::now_utc(),
            InteractionResponsePayload::Approval(ApprovalSelection::new(option_id)),
        );
        locked(&actions)
            .as_ref()
            .ok_or("channel action handle was not started")?
            .submit_interaction_response(response)?;

        wait_until(|| locked(&proxy.approval_response).is_some())?;
        let response_text = locked(&proxy.approval_response)
            .clone()
            .ok_or("proxy upstream did not capture the response")?;
        let response_value: serde_json::Value = serde_json::from_str(&response_text)?;
        assert_eq!(response_value["id"], "proxy-approval");
        assert_eq!(response_value["result"]["decision"], "acceptForSession");
        let resolved = desktop.read()?;
        assert!(matches!(
            resolved,
            Message::Text(text) if text.contains("serverRequest/resolved")
        ));
        wait_until(|| {
            host.inspect_bridge(|bridge| {
                bridge
                    .session_aggregate(session_id)
                    .is_some_and(|aggregate| aggregate.pending_interaction(request.id()).is_none())
            })
            .unwrap_or(false)
        })?;
        assert!(locked(&proxy.server_error).is_none());
        assert_eq!(snapshot(&status).health(), CodexProviderHealth::Running);
        let _ = desktop.close(None);
        let _ = host.stop()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn client_proxy_shutdown_interrupts_connected_desktop() -> TestResult {
        let observer = Arc::new(FakeControl::default());
        seed_lines(&observer, LIVE_FIXTURE.lines().take(2));
        let proxy = Arc::new(ProxyTestControl::default());
        let provider_id = ProviderId::new();
        let runtime_root = std::env::temp_dir().join(format!("agentpulse-proxy-{provider_id}"));
        let config = CodexProviderConfig::new(provider_id, &runtime_root, [THREAD_ID])?;
        let proxy_socket_path = config.proxy_socket_path.clone();
        let schema = ProtocolSchema::compile()?;
        let mapper = CodexEventMapper::new(provider_id, &config.threads, config.discover_threads);
        let status = Arc::new(Mutex::new(Default::default()));
        let approvals = Arc::new(Mutex::new(ApprovalRuntimeState::new()));
        let controls = Arc::new(Mutex::new(ControlRuntimeState::new()));
        let mut source = CodexProviderSource::with_runtime_and_states(
            config,
            schema,
            mapper,
            status,
            Arc::clone(&approvals),
            Arc::clone(&controls),
            Box::new(ProxyTestRuntime {
                observer,
                proxy,
                worker: None,
            }),
        );
        source.client_proxy_enabled = true;
        let port = CodexProviderPort::new(provider_descriptor(provider_id)?, approvals, controls);
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        let _ = host.start()?;

        let stream = UnixStream::connect(proxy_socket_path)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let (mut desktop, _) = tungstenite::client("ws://localhost/", stream)?;
        desktop.send(Message::Text(
            r#"{"id":77,"method":"initialize","params":{}}"#.into(),
        ))?;
        let response = desktop.read()?;
        assert!(matches!(response, Message::Text(text) if text.contains("\"id\":77")));

        let started = Instant::now();
        let _ = host.stop()?;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!runtime_root.exists());
        Ok(())
    }

    #[test]
    fn channel_handoff_failure_keeps_committed_sequence_and_live_reader() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (_provider_id, port, source, status) = test_provider(Arc::clone(&control))?;
        let session_id = SessionId::from_str(THREAD_ID)?;
        let channel_id = ChannelId::new();
        let channel = FailingChannel {
            descriptor: ChannelDescriptor::new(
                channel_id,
                ChannelKind::new("test")?,
                NonEmptyText::new("Failing Channel")?,
                ChannelCapabilities::SESSION_VIEW,
            ),
        };
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        host.register_channel(channel, NoopChannelSource)?;
        let _ = host.start()?;
        let _ = host.subscribe(channel_id, session_id)?;

        let mut live = LIVE_FIXTURE.lines().skip(2);
        if let Some(turn_started) = live.next() {
            control.push_text(turn_started);
        }
        if let Some(waiting) = live.next() {
            control.push_text(waiting);
        }
        wait_until(|| snapshot(&status).mapped_events() == 3)?;
        let aggregate = host.inspect_bridge(|bridge| {
            bridge
                .session_aggregate(session_id)
                .map(|aggregate| (aggregate.last_sequence().get(), aggregate.session().state()))
        })?;
        assert_eq!(aggregate, Some((3, AgentState::WaitingForInteraction)));
        assert_eq!(snapshot(&status).channel_delivery_failures(), 2);
        assert_eq!(snapshot(&status).health(), CodexProviderHealth::Running);
        let _ = host.stop()?;
        Ok(())
    }

    #[test]
    fn transport_failure_is_terminal_but_cleanup_remains_successful() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (_provider_id, port, source, status) = test_provider(Arc::clone(&control))?;
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        let _ = host.start()?;
        control.push_failure("simulated connection loss");
        wait_until(|| snapshot(&status).health() == CodexProviderHealth::Failed)?;
        let _ = host.stop()?;
        assert_eq!(*locked(&control.stops), 1);
        Ok(())
    }

    #[test]
    fn partial_resume_failure_preserves_started_session_and_marks_it_disconnected() -> TestResult {
        let control = Arc::new(FakeControl::default());
        let mut fixture = LIVE_FIXTURE.lines();
        let initialize = fixture.next().ok_or("fixture has no initialize response")?;
        let first_resume = fixture.next().ok_or("fixture has no resume response")?;
        control.push_text(initialize);
        control.push_text(first_resume);
        control.push_text(format!(
            r#"{{"id":3,"error":{{"code":-32000,"message":"thread {SECOND_THREAD_ID} was not found"}}}}"#
        ));
        let (_provider_id, port, source, status) =
            test_provider_with_threads(Arc::clone(&control), &[THREAD_ID, SECOND_THREAD_ID])?;
        let first_session_id = SessionId::from_str(THREAD_ID)?;
        let second_session_id = SessionId::from_str(SECOND_THREAD_ID)?;
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        assert!(host.start().is_err());

        let sessions = host.inspect_bridge(|bridge| {
            (
                bridge.session_aggregate(first_session_id).map(|aggregate| {
                    (
                        aggregate.last_sequence().get(),
                        aggregate.session().connection_state(),
                    )
                }),
                bridge.session_aggregate(second_session_id).is_some(),
            )
        })?;
        assert_eq!(sessions, (Some((2, ConnectionState::Disconnected)), false));
        assert_eq!(snapshot(&status).health(), CodexProviderHealth::Failed);
        assert_eq!(locked(&control.outgoing).len(), 4);
        let _ = host.stop()?;
        Ok(())
    }

    #[cfg(unix)]
    fn fake_codex_script(version: &str) -> Result<(PathBuf, PathBuf), io::Error> {
        let unique = ProviderId::new().to_string();
        let suffix = unique.chars().rev().take(8).collect::<String>();
        let root = PathBuf::from(format!("/tmp/ap{suffix}"));
        fs::create_dir(&root)?;
        let executable = root.join("codex");
        fs::write(
            &executable,
            format!("#!/bin/sh\nprintf 'codex-cli {version}\\n'\n"),
        )?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        Ok((root, executable))
    }

    #[cfg(unix)]
    #[test]
    fn verified_codex_versions_are_accepted() -> TestResult {
        for version in crate::SUPPORTED_CODEX_CLI_VERSIONS {
            let actual = format!("codex-cli {version}");
            assert_eq!(
                classify_codex_version(&actual)?,
                CodexCliCompatibility::Verified
            );
        }
        let preferred = Version::parse(crate::SUPPORTED_CODEX_CLI_VERSION)?;
        assert_eq!(
            (preferred.major, preferred.minor, preferred.patch),
            LATEST_VERIFIED_CODEX_CLI_CORE
        );
        assert_eq!(
            crate::SUPPORTED_CODEX_CLI_VERSIONS.last().copied(),
            Some(crate::SUPPORTED_CODEX_CLI_VERSION)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn newer_codex_versions_are_accepted_as_unverified() -> TestResult {
        for version in ["0.153.1", "0.154.0-beta.1", "1.0.0"] {
            let actual = format!("codex-cli {version}");
            assert_eq!(
                classify_codex_version(&actual)?,
                CodexCliCompatibility::UnverifiedNewer(Version::parse(version)?)
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn older_unknown_and_malformed_codex_versions_are_rejected() {
        for actual in [
            "codex-cli 0.151.0",
            "codex-cli 0.152.1+unverified",
            "codex-cli 0.152.1-beta.1",
            "codex-cli latest",
            "codex 0.153.0",
            "",
        ] {
            assert!(matches!(
                classify_codex_version(actual),
                Err(CodexProviderSourceError::VersionMismatch { .. })
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_rejects_version_mismatch_before_creating_runtime_directory() -> TestResult {
        let provider_id = ProviderId::new();
        let (root, executable) = fake_codex_script("0.151.0")?;
        let config = CodexProviderConfig::new(provider_id, &root, [THREAD_ID])?
            .with_codex_executable(&executable);
        let runtime_directory = config.runtime_directory.clone();
        let mut runtime = ManagedCodexRuntime::default();
        let error = runtime.start(&config);
        assert!(matches!(
            error,
            Err(CodexProviderSourceError::VersionMismatch { .. })
        ));
        assert!(!runtime_directory.exists());
        fs::remove_file(executable)?;
        fs::remove_dir(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_allows_newer_version_past_version_gate() -> TestResult {
        let provider_id = ProviderId::new();
        let (root, executable) = fake_codex_script("0.153.1")?;
        let config = CodexProviderConfig::new(provider_id, &root, [THREAD_ID])?
            .with_codex_executable(&executable);
        let runtime_directory = config.runtime_directory.clone();
        let mut runtime = ManagedCodexRuntime::default();
        let error = runtime.start(&config);
        assert!(matches!(
            error,
            Err(CodexProviderSourceError::ProcessExited { .. })
        ));
        assert!(runtime_directory.exists());
        runtime.stop(&config)?;
        assert!(!runtime_directory.exists());
        fs::remove_file(executable)?;
        fs::remove_dir(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_refuses_occupied_private_directory_without_removing_it() -> TestResult {
        let provider_id = ProviderId::new();
        let (root, executable) = fake_codex_script(crate::SUPPORTED_CODEX_CLI_VERSION)?;
        let config = CodexProviderConfig::new(provider_id, &root, [THREAD_ID])?
            .with_codex_executable(&executable);
        fs::create_dir(&config.runtime_directory)?;
        let mut runtime = ManagedCodexRuntime::default();
        let error = runtime.start(&config);
        assert!(matches!(
            error,
            Err(CodexProviderSourceError::RuntimePathOccupied { .. })
        ));
        let _ = runtime.stop(&config);
        assert!(config.runtime_directory.exists());
        fs::remove_dir(&config.runtime_directory)?;
        fs::remove_file(executable)?;
        fs::remove_dir(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_signals_process_before_waiting_for_shutdown_deadline() -> TestResult {
        let config = CodexProviderConfig::new(ProviderId::new(), "/tmp", [THREAD_ID])?
            .with_shutdown_timeout(Duration::from_secs(3));
        let child = Command::new("sleep")
            .arg("30")
            .stderr(Stdio::piped())
            .spawn()?;
        let mut runtime = ManagedCodexRuntime {
            process: Some(ManagedProcess::new(child)?),
            owns_runtime_directory: false,
        };

        let started = Instant::now();
        runtime.stop(&config)?;

        assert!(started.elapsed() < Duration::from_secs(2));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_kills_the_complete_process_group_after_deadline() -> TestResult {
        let config = CodexProviderConfig::new(ProviderId::new(), "/tmp", [THREAD_ID])?
            .with_shutdown_timeout(Duration::from_millis(100));
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("trap '' TERM; sleep 30 & printf ready; wait")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.process_group(0);
        let mut child = command.spawn()?;
        let mut ready = [0_u8; 5];
        child
            .stdout
            .take()
            .ok_or("managed process readiness pipe was unavailable")?
            .read_exact(&mut ready)?;
        assert_eq!(&ready, b"ready");
        let process_group = i32::try_from(child.id())?;
        let mut runtime = ManagedCodexRuntime {
            process: Some(ManagedProcess::new_in_own_process_group(child)?),
            owns_runtime_directory: false,
        };

        let started = Instant::now();
        runtime.stop(&config)?;

        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(2));
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(-process_group), None) {
                Err(nix::errno::Errno::ESRCH) => break,
                Ok(()) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(()) => return Err("managed process group remained alive".into()),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_websocket_transport_exchanges_text_frames() -> TestResult {
        let unique = ProviderId::new().to_string();
        let suffix = unique.chars().rev().take(8).collect::<String>();
        let socket_path = PathBuf::from(format!("/tmp/apws{suffix}.sock"));
        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let server = thread::spawn(move || -> Result<(), String> {
            let (stream, _) = listener.accept().map_err(|error| error.to_string())?;
            let mut socket = tungstenite::accept(stream).map_err(|error| error.to_string())?;
            let message = socket.read().map_err(|error| error.to_string())?;
            if message.to_text().map_err(|error| error.to_string())? != "request" {
                return Err("server received unexpected text".to_owned());
            }
            socket
                .send(Message::Text("response".into()))
                .map_err(|error| error.to_string())
        });

        let stream = UnixStream::connect(&socket_path)?;
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let (socket, _) = tungstenite::client("ws://localhost/", stream)?;
        let mut io = UnixWebSocketIo { socket };
        io.write_text("request".to_owned())?;
        let response = io.read()?;
        assert!(matches!(response, ReadOutcome::Text(text) if text == "response"));
        let server_result = server.join().map_err(|_| "server thread panicked")?;
        server_result.map_err(|error| -> Box<dyn Error> { error.into() })?;
        fs::remove_file(socket_path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the exact supported Codex CLI to be installed"]
    fn installed_codex_app_server_completes_real_initialize_handshake() -> TestResult {
        let provider_id = ProviderId::new();
        let unique = provider_id.to_string();
        let suffix = unique.chars().rev().take(8).collect::<String>();
        let root = PathBuf::from(format!("/tmp/aplive{suffix}"));
        let config = CodexProviderConfig::new(provider_id, &root, [THREAD_ID])?
            .with_startup_timeout(Duration::from_secs(5));
        let mut runtime = ManagedCodexRuntime::default();
        let operation = (|| -> TestResult {
            let mut io = runtime.start(&config)?;
            let mut protocol = ProtocolEngine::new(ProtocolSchema::compile()?);
            let (_, request) = protocol.initialize_request()?;
            io.write_text(request)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if Instant::now() >= deadline {
                    return Err("real initialize response timed out".into());
                }
                match io.read()? {
                    ReadOutcome::Timeout => {}
                    ReadOutcome::Closed => {
                        return Err("real App Server closed during initialize".into());
                    }
                    ReadOutcome::Text(text) => match protocol.parse_server_text(&text)? {
                        ServerFrame::Response {
                            expected: ExpectedResponse::Initialize,
                            ..
                        } => {
                            io.write_text(protocol.initialized_notification()?)?;
                            io.close()?;
                            return Ok(());
                        }
                        ServerFrame::Request { id, method, .. } => {
                            io.write_text(protocol.unsupported_request_response(id, &method)?)?
                        }
                        ServerFrame::Notification { .. } => {}
                        ServerFrame::Response { .. } | ServerFrame::Error { .. } => {
                            return Err("real App Server returned unexpected response".into());
                        }
                    },
                }
            }
        })();
        let cleanup = runtime.stop(&config);
        operation?;
        cleanup?;
        fs::remove_dir(root)?;
        Ok(())
    }
}
