//! Managed Codex App Server process and Provider Source lifecycle.

use std::{
    collections::VecDeque,
    io::{self, Read},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, MutexGuard, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use agentpulse_bridge::{ProviderEventHandle, ProviderEventSource};
use tungstenite::{Message, WebSocket};

#[cfg(unix)]
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixStream},
};

use crate::{
    CodexProviderConfig, CodexProviderHealth, CodexProviderSourceError,
    mapper::{CodexEventMapper, MappingDisposition},
    protocol::{ExpectedResponse, ProtocolEngine, ProtocolSchema, ServerFrame},
    status::{SharedStatus, lock_status},
};

const IO_POLL_INTERVAL: Duration = Duration::from_millis(200);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const STDERR_LIMIT: usize = 64 * 1024;

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
    runtime: Box<dyn AppServerRuntime>,
    stop_sender: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<WorkerExit>>,
    resources_acquired: bool,
}

impl CodexProviderSource {
    pub(crate) fn new(
        config: CodexProviderConfig,
        schema: ProtocolSchema,
        mapper: CodexEventMapper,
        status: SharedStatus,
    ) -> Self {
        Self::with_runtime(
            config,
            schema,
            mapper,
            status,
            Box::new(ManagedCodexRuntime::default()),
        )
    }

    pub(crate) fn with_runtime(
        config: CodexProviderConfig,
        schema: ProtocolSchema,
        mapper: CodexEventMapper,
        status: SharedStatus,
        runtime: Box<dyn AppServerRuntime>,
    ) -> Self {
        Self {
            config,
            schema,
            mapper: Arc::new(Mutex::new(mapper)),
            status,
            runtime,
            stop_sender: None,
            worker: None,
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
        let worker = thread::Builder::new()
            .name("agentpulse-codex-reader".to_owned())
            .spawn(move || run_worker(io, protocol, mapper, status, worker_events, stop_receiver))
            .map_err(|error| {
                self.record_start_failure(
                    CodexProviderSourceError::runtime("reader thread spawn", error),
                    &events,
                )
            })?;
        self.stop_sender = Some(stop_sender);
        self.worker = Some(worker);
        lock_status(&self.status).health = CodexProviderHealth::Running;
        Ok(())
    }

    fn stop_inner(&mut self) -> Result<(), CodexProviderSourceError> {
        let mut cleanup_failures = Vec::new();
        if let Some(sender) = self.stop_sender.take() {
            let _ = sender.send(());
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
            ServerFrame::Request { id, method } => {
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

fn run_worker(
    mut io: Box<dyn AppServerIo>,
    mut protocol: ProtocolEngine,
    mapper: Arc<Mutex<CodexEventMapper>>,
    status: SharedStatus,
    events: ProviderEventHandle,
    stop_receiver: mpsc::Receiver<()>,
) -> WorkerExit {
    loop {
        if stop_receiver.try_recv().is_ok() {
            return WorkerExit::Stopped(io.close().map_err(|error| error.to_string()));
        }
        let result = match io.read() {
            Ok(ReadOutcome::Timeout) => continue,
            Ok(ReadOutcome::Closed) => Err(CodexProviderSourceError::transport(
                "App Server closed the WebSocket",
            )),
            Ok(ReadOutcome::Text(text)) => {
                process_live_frame(&mut *io, &mut protocol, &mapper, &status, &events, &text)
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            if stop_receiver.try_recv().is_ok() {
                return WorkerExit::Stopped(io.close().map_err(|close| close.to_string()));
            }
            lock_mapper(&mapper).disconnect_all(&events, &status);
            let message = error.to_string();
            let mut current = lock_status(&status);
            current.health = CodexProviderHealth::Failed;
            current.last_error = Some(message.clone());
            return WorkerExit::Failed;
        }
    }
}

fn process_live_frame(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    events: &ProviderEventHandle,
    text: &str,
) -> Result<(), CodexProviderSourceError> {
    let frame = protocol.parse_server_text(text)?;
    lock_status(status).validated_frames += 1;
    match frame {
        ServerFrame::Notification { method, params } => {
            let disposition = lock_mapper(mapper).notification(&method, &params, events, status)?;
            if disposition == MappingDisposition::ValidatedUnmapped {
                lock_status(status).validated_unmapped_frames += 1;
            }
            Ok(())
        }
        ServerFrame::Request { id, method } => {
            io.write_text(protocol.unsupported_request_response(id, &method)?)?;
            lock_status(status).rejected_server_requests += 1;
            Ok(())
        }
        ServerFrame::Response { expected, .. } | ServerFrame::Error { expected, .. } => {
            Err(CodexProviderSourceError::protocol(format!(
                "unexpected live {} response",
                expected.method()
            )))
        }
    }
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
            verify_codex_version(config)?;
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
                .arg(&config.remote_uri)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            let child = command
                .spawn()
                .map_err(|error| CodexProviderSourceError::runtime("process launch", error))?;
            self.process = Some(ManagedProcess::new(child)?);

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
fn verify_codex_version(config: &CodexProviderConfig) -> Result<(), CodexProviderSourceError> {
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
    let expected_output = format!("codex-cli {}", crate::SUPPORTED_CODEX_CLI_VERSION);
    if actual == expected_output {
        Ok(())
    } else {
        Err(CodexProviderSourceError::VersionMismatch {
            expected: crate::SUPPORTED_CODEX_CLI_VERSION,
            actual,
        })
    }
}

struct ManagedProcess {
    child: Child,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    stderr_thread: Option<JoinHandle<()>>,
    stderr_done: mpsc::Receiver<()>,
}

impl ManagedProcess {
    fn new(mut child: Child) -> Result<Self, CodexProviderSourceError> {
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
            stderr,
            stderr_thread: Some(stderr_thread),
            stderr_done,
        })
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
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
        str::FromStr,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    #[cfg(unix)]
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, net::UnixListener},
        path::PathBuf,
    };

    use agentpulse_bridge::{ChannelActionHandle, ChannelActionSource, ChannelPort, RuntimeHost};
    use agentpulse_core::{
        AgentEvent, AgentSession, AgentState, ChannelCapabilities, ChannelDescriptor,
        ChannelEventRoute, ChannelId, ChannelKind, ConnectionState, NonEmptyText,
        ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind, SessionId,
        SessionOutcome,
    };

    use super::*;
    use crate::{CodexProviderPort, status::snapshot};

    type TestResult = Result<(), Box<dyn Error>>;

    const THREAD_ID: &str = "019976a4-00f0-7312-b36c-d01f9c5c06f6";
    const SECOND_THREAD_ID: &str = "019976a4-00f4-7561-a2a4-156c98eb31bc";
    const LIVE_FIXTURE: &str = include_str!("../tests/fixtures/live_success.jsonl");
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
            ProviderCapabilities::SESSION_STATE,
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
        let mapper = CodexEventMapper::new(provider_id, &config.threads);
        let status = Arc::new(Mutex::new(Default::default()));
        let source = CodexProviderSource::with_runtime(
            config,
            schema,
            mapper,
            Arc::clone(&status),
            Box::new(FakeRuntime { control }),
        );
        let descriptor = provider_descriptor(provider_id)?;
        Ok((
            provider_id,
            CodexProviderPort::new(descriptor),
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
    fn server_request_receives_read_only_error_without_stopping_stream() -> TestResult {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (_provider_id, port, source, status) = test_provider(Arc::clone(&control))?;
        let mut host = RuntimeHost::new();
        host.register_provider(port, source)?;
        let _ = host.start()?;
        control.push_text(SERVER_REQUEST.trim());

        wait_until(|| snapshot(&status).rejected_server_requests() == 1)?;
        assert_eq!(snapshot(&status).health(), CodexProviderHealth::Running);
        let outgoing = locked(&control.outgoing);
        assert!(outgoing.iter().any(|frame| frame.contains("-32601")));
        drop(outgoing);
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
                        ServerFrame::Request { id, method } => {
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
