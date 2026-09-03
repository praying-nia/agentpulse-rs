//! Native Channel Source lifecycle and connection state machine.

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use agentpulse_bridge::{
    ChannelActionError, ChannelActionHandle, ChannelActionIngressError, ChannelActionSource,
    ChannelSubscriptionScope, SubscribeOutcome, SubscriptionError, UnsubscribeOutcome,
};
use agentpulse_core::{ChannelDescriptor, SessionId};
use agentpulse_protocol::{ProtocolMessage, V1_PROTOCOL_VERSION};
use agentpulse_transport::{
    LoopbackWebSocket, LoopbackWebSocketError, LoopbackWebSocketListener, TlsWebSocket,
    TlsWebSocketListener, TransportRead,
};
use tungstenite::protocol::frame::coding::CloseCode;
use uuid::Uuid;

use crate::{
    NativeChannelConfig, NativeChannelHealth, NativeChannelSourceError, NativeClientMessage,
    NativeDeliveryContext, NativeErrorCode, NativeProtocolError, NativeServerMessage,
    NativeSubscriptionStatus, NativeUnsubscriptionStatus, decode_client_message,
    encode_server_message,
    port::lock_delivery,
    state::{ActiveClient, PendingSubscription, SharedDeliveryState},
    status::{SharedStatus, lock_status},
};

struct Worker {
    stop: Arc<AtomicBool>,
    completion: Receiver<Result<(), String>>,
    join: Option<JoinHandle<()>>,
}

/// RuntimeHost-owned Source that serves one active local Native client.
pub struct NativeChannelSource {
    config: NativeChannelConfig,
    descriptor: ChannelDescriptor,
    state: SharedDeliveryState,
    status: SharedStatus,
    worker: Option<Worker>,
}

impl NativeChannelSource {
    pub(crate) fn new(
        config: NativeChannelConfig,
        descriptor: ChannelDescriptor,
        state: SharedDeliveryState,
        status: SharedStatus,
    ) -> Self {
        Self {
            config,
            descriptor,
            state,
            status,
            worker: None,
        }
    }

    fn stop_worker(&mut self) -> Result<(), NativeChannelSourceError> {
        let Some(worker) = self.worker.as_mut() else {
            let mut status = lock_status(&self.status);
            status.health = NativeChannelHealth::Stopped;
            status.local_address = None;
            status.client_id = None;
            return Ok(());
        };
        worker.stop.store(true, Ordering::Release);
        let completed = worker
            .completion
            .recv_timeout(self.config.shutdown_timeout)
            .map_err(|_| NativeChannelSourceError::ShutdownTimeout)?;
        let join = worker.join.take();
        if let Some(join) = join {
            join.join()
                .map_err(|_| NativeChannelSourceError::WorkerPanicked)?;
        }
        self.worker = None;
        let mut status = lock_status(&self.status);
        status.local_address = None;
        status.client_id = None;
        match completed {
            Ok(()) => {
                status.health = NativeChannelHealth::Stopped;
                Ok(())
            }
            Err(message) => {
                status.health = NativeChannelHealth::Failed;
                Err(NativeChannelSourceError::WorkerFailed { message })
            }
        }
    }
}

impl ChannelActionSource for NativeChannelSource {
    type Error = NativeChannelSourceError;

    fn start(&mut self, actions: ChannelActionHandle) -> Result<(), Self::Error> {
        if self.worker.is_some() {
            return Err(NativeChannelSourceError::AlreadyRunning);
        }
        let listener = if let Some(config) = self.config.tls_transport_config()? {
            NativeListener::Tls(TlsWebSocketListener::bind(config)?)
        } else {
            NativeListener::Loopback(LoopbackWebSocketListener::bind(
                self.config.transport_config()?,
            )?)
        };
        let local_address = listener.local_address();
        {
            let mut status = lock_status(&self.status);
            status.health = NativeChannelHealth::Listening;
            status.local_address = Some(local_address);
            status.client_id = None;
            status.last_error = None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (completion_tx, completion_rx) = mpsc::channel();
        let config = self.config.clone();
        let descriptor = self.descriptor.clone();
        let state = Arc::clone(&self.state);
        let status = Arc::clone(&self.status);
        let join = thread::Builder::new()
            .name("agentpulse-native-channel".to_owned())
            .spawn(move || {
                let result = run_worker(
                    listener,
                    actions,
                    config,
                    descriptor,
                    state,
                    status,
                    worker_stop,
                );
                let _ = completion_tx.send(result);
            });
        let join = match join {
            Ok(join) => join,
            Err(source) => {
                let mut status = lock_status(&self.status);
                status.health = NativeChannelHealth::Failed;
                status.local_address = None;
                status.last_error = Some(source.to_string());
                return Err(NativeChannelSourceError::WorkerSpawn { source });
            }
        };
        self.worker = Some(Worker {
            stop,
            completion: completion_rx,
            join: Some(join),
        });
        Ok(())
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.stop_worker()
    }

    fn subscription_scope(&self) -> ChannelSubscriptionScope {
        ChannelSubscriptionScope::SourceGeneration
    }
}

impl Drop for NativeChannelSource {
    fn drop(&mut self) {
        let _ = self.stop_worker();
    }
}

struct Connection {
    socket: NativeWebSocket,
    handshaken: bool,
    accepted_at: Instant,
    last_activity: Instant,
    last_ping: Instant,
}

enum NativeListener {
    Loopback(LoopbackWebSocketListener),
    Tls(TlsWebSocketListener),
}

impl NativeListener {
    fn local_address(&self) -> std::net::SocketAddr {
        match self {
            Self::Loopback(listener) => listener.local_address(),
            Self::Tls(listener) => listener.local_address(),
        }
    }

    fn try_accept(&self) -> Result<Option<NativeWebSocket>, LoopbackWebSocketError> {
        match self {
            Self::Loopback(listener) => listener
                .try_accept()
                .map(|socket| socket.map(|socket| NativeWebSocket::Loopback(Box::new(socket)))),
            Self::Tls(listener) => listener
                .try_accept()
                .map(|socket| socket.map(|socket| NativeWebSocket::Tls(Box::new(socket)))),
        }
    }
}

enum NativeWebSocket {
    Loopback(Box<LoopbackWebSocket>),
    Tls(Box<TlsWebSocket>),
}

impl NativeWebSocket {
    fn authenticated_client_id(&self) -> Option<&str> {
        match self {
            Self::Loopback(_) => None,
            Self::Tls(socket) => socket.authenticated_client_id(),
        }
    }

    fn remains_authorized(&self) -> bool {
        match self {
            Self::Loopback(_) => true,
            Self::Tls(socket) => socket.remains_authorized(),
        }
    }

    fn read(&mut self) -> Result<TransportRead, LoopbackWebSocketError> {
        match self {
            Self::Loopback(socket) => socket.read(),
            Self::Tls(socket) => socket.read(),
        }
    }

    fn send_text(&mut self, text: String) -> Result<(), LoopbackWebSocketError> {
        match self {
            Self::Loopback(socket) => socket.send_text(text),
            Self::Tls(socket) => socket.send_text(text),
        }
    }

    fn send_ping(&mut self) -> Result<(), LoopbackWebSocketError> {
        match self {
            Self::Loopback(socket) => socket.send_ping(),
            Self::Tls(socket) => socket.send_ping(),
        }
    }

    fn close(
        &mut self,
        code: CloseCode,
        reason: impl Into<String>,
    ) -> Result<(), LoopbackWebSocketError> {
        let reason = reason.into();
        match self {
            Self::Loopback(socket) => socket.close(code, reason),
            Self::Tls(socket) => socket.close(code, reason),
        }
    }
}

fn run_worker(
    listener: NativeListener,
    actions: ChannelActionHandle,
    config: NativeChannelConfig,
    descriptor: ChannelDescriptor,
    state: SharedDeliveryState,
    status: SharedStatus,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut connection: Option<Connection> = None;
    while !stop.load(Ordering::Acquire) {
        match listener.try_accept() {
            Ok(Some(mut socket)) if connection.is_some() => {
                let _ = send_direct(
                    &mut socket,
                    &NativeServerMessage::Error {
                        request_id: None,
                        code: NativeErrorCode::ConnectionBusy,
                        message: "another Native client is already connected".to_owned(),
                        recoverable: false,
                    },
                );
                let _ = socket.close(CloseCode::Policy, "Native Channel is busy");
            }
            Ok(Some(socket)) => {
                let now = Instant::now();
                connection = Some(Connection {
                    socket,
                    handshaken: false,
                    accepted_at: now,
                    last_activity: now,
                    last_ping: now,
                });
            }
            Ok(None) => {}
            Err(error) => {
                let diagnostic = error.to_string();
                record_error(&status, diagnostic.clone());
                if !matches!(
                    error,
                    agentpulse_transport::LoopbackWebSocketError::Handshake { .. }
                ) {
                    if let Some(mut active) = connection.take() {
                        let _ = active
                            .socket
                            .close(CloseCode::Error, "Native listener failed");
                    }
                    disconnect(&actions, &state, &status);
                    let mut snapshot = lock_status(&status);
                    snapshot.health = NativeChannelHealth::Failed;
                    snapshot.local_address = None;
                    snapshot.client_id = None;
                    return Err(diagnostic);
                }
            }
        }

        let Some(active) = connection.as_mut() else {
            thread::sleep(config.io_poll_interval);
            continue;
        };

        if !active.socket.remains_authorized() {
            let diagnostic = "Native device credential was revoked".to_owned();
            let _ = active.socket.close(CloseCode::Policy, &diagnostic);
            record_error(&status, diagnostic);
            disconnect(&actions, &state, &status);
            connection = None;
            continue;
        }

        if let Some(reason) = lock_delivery(&state).abort_reason.clone() {
            record_error(&status, reason.clone());
            let _ = active.socket.close(CloseCode::Error, reason);
            disconnect(&actions, &state, &status);
            connection = None;
            continue;
        }

        while let Some(frame) = lock_delivery(&state).outgoing.pop_front() {
            if let Err(error) = active.socket.send_text(frame) {
                record_error(&status, error.to_string());
                disconnect(&actions, &state, &status);
                connection = None;
                break;
            }
            lock_status(&status).frames_sent += 1;
        }
        let Some(active) = connection.as_mut() else {
            continue;
        };

        let now = Instant::now();
        if !active.handshaken && now.duration_since(active.accepted_at) >= config.handshake_timeout
        {
            let _ = send_direct(
                &mut active.socket,
                &protocol_error(
                    None,
                    NativeErrorCode::InvalidHandshake,
                    "client hello timed out",
                    false,
                ),
            );
            let _ = active
                .socket
                .close(CloseCode::Policy, "client hello timed out");
            disconnect(&actions, &state, &status);
            connection = None;
            continue;
        }
        if now.duration_since(active.last_activity) >= config.idle_timeout {
            let _ = active
                .socket
                .close(CloseCode::Away, "Native client timed out");
            disconnect(&actions, &state, &status);
            connection = None;
            continue;
        }
        if active.handshaken && now.duration_since(active.last_ping) >= config.ping_interval {
            if let Err(error) = active.socket.send_ping() {
                record_error(&status, error.to_string());
                disconnect(&actions, &state, &status);
                connection = None;
                continue;
            }
            active.last_ping = now;
        }

        match active.socket.read() {
            Ok(TransportRead::Text(text)) => {
                active.last_activity = Instant::now();
                lock_status(&status).frames_received += 1;
                let message = match decode_client_message(text.as_bytes()) {
                    Ok(message) => message,
                    Err(error) => {
                        let diagnostic = error.to_string();
                        let _ = send_direct(
                            &mut active.socket,
                            &protocol_error(
                                None,
                                NativeErrorCode::InvalidRequest,
                                &diagnostic,
                                false,
                            ),
                        );
                        let _ = active.socket.close(CloseCode::Protocol, diagnostic);
                        record_error(&status, error.to_string());
                        disconnect(&actions, &state, &status);
                        connection = None;
                        continue;
                    }
                };
                let authenticated_client_id =
                    active.socket.authenticated_client_id().map(str::to_owned);
                let result = if active.handshaken {
                    process_ready_message(message, &actions, &config, &state, &status)
                } else {
                    process_hello(
                        message,
                        authenticated_client_id.as_deref(),
                        &descriptor,
                        &config,
                        &state,
                        &status,
                    )
                };
                match result {
                    Ok(()) => active.handshaken = true,
                    Err(ConnectionFailure::Recoverable(error)) => {
                        if let Err(send_error) = enqueue_control(&state, *error) {
                            record_error(&status, send_error.to_string());
                        }
                    }
                    Err(ConnectionFailure::Fatal(error)) => {
                        let diagnostic = error.to_string();
                        let _ = send_direct(
                            &mut active.socket,
                            &protocol_error(
                                None,
                                NativeErrorCode::InvalidHandshake,
                                &diagnostic,
                                false,
                            ),
                        );
                        let _ = active.socket.close(CloseCode::Policy, diagnostic);
                        record_error(&status, error.to_string());
                        disconnect(&actions, &state, &status);
                        connection = None;
                    }
                }
            }
            Ok(TransportRead::Pong | TransportRead::Control) => {
                active.last_activity = Instant::now();
            }
            Ok(TransportRead::Timeout) => {}
            Ok(TransportRead::Closed) => {
                disconnect(&actions, &state, &status);
                connection = None;
            }
            Err(error) => {
                record_error(&status, error.to_string());
                let _ = active.socket.close(CloseCode::Protocol, error.to_string());
                disconnect(&actions, &state, &status);
                connection = None;
            }
            Ok(_) => {
                let diagnostic = "unsupported future WebSocket read outcome".to_owned();
                record_error(&status, diagnostic.clone());
                let _ = active.socket.close(CloseCode::Protocol, diagnostic);
                disconnect(&actions, &state, &status);
                connection = None;
            }
        }
    }

    if let Some(mut active) = connection {
        let _ = active
            .socket
            .close(CloseCode::Away, "Native Channel stopped");
    }
    disconnect(&actions, &state, &status);
    Ok(())
}

enum ConnectionFailure {
    Recoverable(Box<NativeServerMessage>),
    Fatal(NativeProtocolError),
}

impl From<NativeProtocolError> for ConnectionFailure {
    fn from(error: NativeProtocolError) -> Self {
        Self::Fatal(error)
    }
}

fn process_hello(
    message: NativeClientMessage,
    authenticated_client_id: Option<&str>,
    descriptor: &ChannelDescriptor,
    config: &NativeChannelConfig,
    state: &SharedDeliveryState,
    status: &SharedStatus,
) -> Result<(), ConnectionFailure> {
    let NativeClientMessage::Hello {
        client_id,
        supported_protocol_versions,
        ..
    } = message
    else {
        return Err(ConnectionFailure::Fatal(
            NativeProtocolError::InvalidField {
                field: "first client message",
                reason: "expected client_hello".to_owned(),
            },
        ));
    };
    if let Some(authenticated_client_id) = authenticated_client_id
        && authenticated_client_id != client_id
    {
        return Err(ConnectionFailure::Fatal(
            NativeProtocolError::InvalidField {
                field: "client_id",
                reason: "client hello identity does not match the authenticated upgrade".to_owned(),
            },
        ));
    }
    if !supported_protocol_versions.contains(&V1_PROTOCOL_VERSION) {
        return Err(ConnectionFailure::Fatal(
            NativeProtocolError::InvalidField {
                field: "supported_protocol_versions",
                reason: "AgentPulse JSON protocol v1 is required".to_owned(),
            },
        ));
    }
    {
        let mut delivery = lock_delivery(state);
        delivery.clear_connection();
        delivery.client = Some(ActiveClient {
            discovered: BTreeSet::new(),
            subscriptions: BTreeSet::new(),
            pending: None,
        });
    }
    enqueue_control(
        state,
        NativeServerMessage::Hello {
            connection_id: Uuid::now_v7().to_string(),
            channel: descriptor.clone(),
            protocol_version: V1_PROTOCOL_VERSION,
            max_frame_bytes: config.max_frame_bytes,
            ping_interval_seconds: config.ping_interval.as_secs(),
            idle_timeout_seconds: config.idle_timeout.as_secs(),
        },
    )
    .map_err(ConnectionFailure::Fatal)?;
    let mut snapshot = lock_status(status);
    snapshot.health = NativeChannelHealth::Connected;
    snapshot.client_id = Some(client_id);
    snapshot.connections += 1;
    Ok(())
}

fn process_ready_message(
    message: NativeClientMessage,
    actions: &ChannelActionHandle,
    config: &NativeChannelConfig,
    state: &SharedDeliveryState,
    status: &SharedStatus,
) -> Result<(), ConnectionFailure> {
    match message {
        NativeClientMessage::Hello { .. } => Err(ConnectionFailure::Fatal(
            NativeProtocolError::InvalidField {
                field: "client_hello",
                reason: "client hello may only be sent once".to_owned(),
            },
        )),
        NativeClientMessage::Discover { request_id } => {
            process_discover(request_id, actions, config, state, status)
        }
        NativeClientMessage::Subscribe {
            request_id,
            session_id,
        } => process_subscribe(request_id, session_id, actions, state, status),
        NativeClientMessage::Unsubscribe {
            request_id,
            session_id,
        } => process_unsubscribe(request_id, session_id, actions, state),
        NativeClientMessage::SubmitInteractionResponse {
            request_id,
            response,
        } => process_interaction_response(request_id, response, actions, state),
    }
}

fn process_discover(
    request_id: String,
    actions: &ChannelActionHandle,
    _config: &NativeChannelConfig,
    state: &SharedDeliveryState,
    status: &SharedStatus,
) -> Result<(), ConnectionFailure> {
    {
        let delivery = lock_delivery(state);
        let Some(client) = delivery.client.as_ref() else {
            return Err(internal_error(
                Some(request_id),
                "client state is unavailable",
            ));
        };
        if client.pending.is_some() {
            return Err(ConnectionFailure::Recoverable(Box::new(protocol_error(
                Some(request_id),
                NativeErrorCode::InvalidRequest,
                "wait for the active subscription baseline before discovery",
                true,
            ))));
        }
    }
    let snapshot = actions
        .discovery_snapshot()
        .map_err(|error| runtime_failure(Some(request_id.clone()), error))?;
    let mut frames = Vec::with_capacity(snapshot.providers().len() + snapshot.sessions().len() + 2);
    frames.push(server_text(&NativeServerMessage::SyncStarted {
        request_id: request_id.clone(),
        provider_count: snapshot.providers().len(),
        session_count: snapshot.sessions().len(),
    })?);
    for provider in snapshot.providers() {
        frames.push(server_text(&NativeServerMessage::Domain {
            context: NativeDeliveryContext::DiscoveryProvider {
                request_id: request_id.clone(),
            },
            message: Box::new(ProtocolMessage::ProviderDescriptor(provider.clone())),
        })?);
    }
    let discovered = snapshot
        .sessions()
        .iter()
        .map(|entry| entry.session().id())
        .collect::<BTreeSet<_>>();
    for entry in snapshot.sessions() {
        frames.push(server_text(&NativeServerMessage::Domain {
            context: NativeDeliveryContext::DiscoverySession {
                request_id: request_id.clone(),
                last_sequence: entry.last_sequence(),
            },
            message: Box::new(ProtocolMessage::AgentSession(entry.session().clone())),
        })?);
    }
    frames.push(server_text(&NativeServerMessage::SyncCompleted {
        request_id,
    })?);
    let mut delivery = lock_delivery(state);
    delivery
        .enqueue_batch(frames)
        .map_err(|capacity| ConnectionFailure::Fatal(queue_error(capacity)))?;
    if let Some(client) = delivery.client.as_mut() {
        client.discovered = discovered;
    }
    lock_status(status).discoveries += 1;
    Ok(())
}

fn process_subscribe(
    request_id: String,
    session_id: SessionId,
    actions: &ChannelActionHandle,
    state: &SharedDeliveryState,
    status: &SharedStatus,
) -> Result<(), ConnectionFailure> {
    {
        let mut delivery = lock_delivery(state);
        let Some(client) = delivery.client.as_mut() else {
            return Err(internal_error(
                Some(request_id),
                "client state is unavailable",
            ));
        };
        if !client.discovered.contains(&session_id) {
            return Err(ConnectionFailure::Recoverable(Box::new(protocol_error(
                Some(request_id),
                NativeErrorCode::SessionNotDiscovered,
                "Session was not present in the latest discovery snapshot",
                true,
            ))));
        }
        if client.pending.is_some() {
            return Err(ConnectionFailure::Recoverable(Box::new(protocol_error(
                Some(request_id),
                NativeErrorCode::InvalidRequest,
                "another subscription is being synchronized",
                true,
            ))));
        }
        client.pending = Some(PendingSubscription {
            request_id: request_id.clone(),
            session_id,
            frames: Default::default(),
        });
    }

    let outcome = match actions.subscribe(session_id) {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(client) = lock_delivery(state).client.as_mut() {
                client.pending = None;
            }
            return Err(runtime_failure(Some(request_id), error));
        }
    };
    if let Some(client) = lock_delivery(state).client.as_mut() {
        let _ = client.subscriptions.insert(session_id);
    }
    let (subscription_status, baseline_sequence, pending_interaction_count) = match outcome {
        SubscribeOutcome::Subscribed {
            session_view_delivered: true,
            baseline_sequence,
            pending_interaction_count,
        } => (
            NativeSubscriptionStatus::Subscribed,
            baseline_sequence,
            pending_interaction_count,
        ),
        SubscribeOutcome::AlreadySubscribed { current_sequence } => (
            NativeSubscriptionStatus::AlreadySubscribed,
            current_sequence,
            0,
        ),
        SubscribeOutcome::Subscribed {
            session_view_delivered: false,
            ..
        } => {
            let _ = actions.unsubscribe(session_id);
            if let Some(client) = lock_delivery(state).client.as_mut() {
                client.pending = None;
                let _ = client.subscriptions.remove(&session_id);
            }
            return Err(internal_error(
                Some(request_id),
                "Native Channel subscription did not deliver a Session baseline",
            ));
        }
        _ => {
            let _ = actions.unsubscribe(session_id);
            if let Some(client) = lock_delivery(state).client.as_mut() {
                client.pending = None;
                let _ = client.subscriptions.remove(&session_id);
            }
            return Err(internal_error(
                Some(request_id),
                "unsupported future subscription outcome",
            ));
        }
    };
    let result_frame = server_text(&NativeServerMessage::SubscriptionResult {
        request_id,
        session_id,
        status: subscription_status,
        baseline_sequence,
        pending_interaction_count,
    })?;
    let mut delivery = lock_delivery(state);
    let Some(client) = delivery.client.as_mut() else {
        return Err(internal_error(
            None,
            "client disconnected during subscription",
        ));
    };
    let pending = client
        .pending
        .take()
        .ok_or_else(|| internal_error(None, "subscription synchronization state disappeared"))?;
    let mut frames = Vec::with_capacity(pending.frames.len() + 1);
    frames.push(result_frame);
    frames.extend(pending.frames);
    delivery
        .enqueue_batch(frames)
        .map_err(|capacity| ConnectionFailure::Fatal(queue_error(capacity)))?;
    if subscription_status == NativeSubscriptionStatus::Subscribed {
        lock_status(status).subscriptions += 1;
    }
    Ok(())
}

fn process_interaction_response(
    request_id: String,
    response: agentpulse_core::InteractionResponse,
    actions: &ChannelActionHandle,
    state: &SharedDeliveryState,
) -> Result<(), ConnectionFailure> {
    let session_id = response.session_id();
    let interaction_id = response.request_id().to_string();
    let subscribed = lock_delivery(state)
        .client
        .as_ref()
        .is_some_and(|client| client.subscriptions.contains(&session_id));
    if !subscribed {
        return Err(ConnectionFailure::Recoverable(Box::new(protocol_error(
            Some(request_id),
            NativeErrorCode::SessionNotSubscribed,
            "the current Native connection is not subscribed to the response Session",
            true,
        ))));
    }

    actions
        .submit_interaction_response(response)
        .map_err(|error| interaction_response_failure(request_id.clone(), error))?;
    enqueue_control(
        state,
        NativeServerMessage::InteractionResponseResult {
            request_id,
            session_id,
            interaction_id,
        },
    )
    .map_err(ConnectionFailure::Fatal)
}

fn process_unsubscribe(
    request_id: String,
    session_id: SessionId,
    actions: &ChannelActionHandle,
    state: &SharedDeliveryState,
) -> Result<(), ConnectionFailure> {
    let outcome = actions
        .unsubscribe(session_id)
        .map_err(|error| runtime_failure(Some(request_id.clone()), error))?;
    let status = match outcome {
        UnsubscribeOutcome::Unsubscribed => NativeUnsubscriptionStatus::Unsubscribed,
        UnsubscribeOutcome::NotSubscribed => NativeUnsubscriptionStatus::NotSubscribed,
        _ => {
            return Err(internal_error(
                Some(request_id),
                "unsupported future unsubscription outcome",
            ));
        }
    };
    {
        let mut delivery = lock_delivery(state);
        if let Some(client) = delivery.client.as_mut() {
            let _ = client.subscriptions.remove(&session_id);
        }
    }
    enqueue_control(
        state,
        NativeServerMessage::UnsubscriptionResult {
            request_id,
            session_id,
            status,
        },
    )
    .map_err(ConnectionFailure::Fatal)
}

fn disconnect(actions: &ChannelActionHandle, state: &SharedDeliveryState, status: &SharedStatus) {
    let subscriptions = lock_delivery(state)
        .client
        .as_ref()
        .map(|client| client.subscriptions.iter().copied().collect::<Vec<_>>())
        .unwrap_or_default();
    for session_id in subscriptions {
        let _ = actions.unsubscribe(session_id);
    }
    lock_delivery(state).clear_connection();
    let mut snapshot = lock_status(status);
    if snapshot.health == NativeChannelHealth::Connected {
        snapshot.disconnects += 1;
    }
    snapshot.health = NativeChannelHealth::Listening;
    snapshot.client_id = None;
}

fn enqueue_control(
    state: &SharedDeliveryState,
    message: NativeServerMessage,
) -> Result<(), NativeProtocolError> {
    let text = server_text(&message)?;
    lock_delivery(state).enqueue(text).map_err(queue_error)
}

fn send_direct(socket: &mut NativeWebSocket, message: &NativeServerMessage) -> Result<(), String> {
    let text = server_text(message).map_err(|error| error.to_string())?;
    socket.send_text(text).map_err(|error| error.to_string())
}

fn server_text(message: &NativeServerMessage) -> Result<String, NativeProtocolError> {
    let bytes = encode_server_message(message)?;
    String::from_utf8(bytes).map_err(|error| NativeProtocolError::InvalidField {
        field: "encoded server frame",
        reason: error.to_string(),
    })
}

fn protocol_error(
    request_id: Option<String>,
    code: NativeErrorCode,
    message: &str,
    recoverable: bool,
) -> NativeServerMessage {
    NativeServerMessage::Error {
        request_id,
        code,
        message: message.to_owned(),
        recoverable,
    }
}

fn internal_error(request_id: Option<String>, message: &str) -> ConnectionFailure {
    ConnectionFailure::Recoverable(Box::new(protocol_error(
        request_id,
        NativeErrorCode::Internal,
        message,
        true,
    )))
}

fn runtime_failure(
    request_id: Option<String>,
    error: ChannelActionIngressError,
) -> ConnectionFailure {
    let code = if matches!(
        error,
        ChannelActionIngressError::Subscription(SubscriptionError::SessionNotFound { .. })
    ) {
        NativeErrorCode::SessionNotFound
    } else {
        NativeErrorCode::Internal
    };
    ConnectionFailure::Recoverable(Box::new(protocol_error(
        request_id,
        code,
        &error.to_string(),
        true,
    )))
}

fn interaction_response_failure(
    request_id: String,
    error: ChannelActionIngressError,
) -> ConnectionFailure {
    let code = match &error {
        ChannelActionIngressError::Bridge(ChannelActionError::InteractionNotPending { .. }) => {
            NativeErrorCode::InteractionNotPending
        }
        ChannelActionIngressError::Bridge(ChannelActionError::ChannelNotSubscribed { .. }) => {
            NativeErrorCode::SessionNotSubscribed
        }
        ChannelActionIngressError::Bridge(ChannelActionError::CapabilityRoute(_)) => {
            NativeErrorCode::CapabilityUnavailable
        }
        ChannelActionIngressError::Bridge(ChannelActionError::ProviderHandoff { .. }) => {
            NativeErrorCode::ProviderRejected
        }
        _ => NativeErrorCode::Internal,
    };
    ConnectionFailure::Recoverable(Box::new(protocol_error(
        Some(request_id),
        code,
        &error.to_string(),
        true,
    )))
}

fn queue_error(capacity: usize) -> NativeProtocolError {
    NativeProtocolError::InvalidField {
        field: "outbound queue",
        reason: format!("reached its {capacity}-frame limit"),
    }
}

fn record_error(status: &SharedStatus, message: String) {
    lock_status(status).last_error = Some(message);
}
