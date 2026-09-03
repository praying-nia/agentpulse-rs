//! Real loopback Native Channel flow with an independent protocol client.

use std::{
    error::Error,
    fmt,
    net::TcpStream,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use agentpulse_bridge::{ProviderEventHandle, ProviderEventSource, ProviderPort, RuntimeHost};
use agentpulse_channel_native::{
    NATIVE_WEBSOCKET_PATH, NATIVE_WEBSOCKET_SUBPROTOCOL, NativeChannel, NativeChannelConfig,
    NativeClientMessage, NativeDeliveryContext, NativeServerMessage, NativeSubscriptionStatus,
    decode_server_message, encode_client_message,
};
use agentpulse_core::{
    AgentCommand, AgentEvent, AgentEventPayload, AgentMessage, AgentMessageLevel, AgentSession,
    AgentState, ChannelId, EventId, EventSequence, InteractionResponse, NonEmptyText,
    ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind, SessionId, Timestamp,
};
use agentpulse_protocol::{ProtocolMessage, V1_PROTOCOL_VERSION};
use tungstenite::{ClientRequestBuilder, Message, WebSocket, client, http::Uri};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn Error>>;

#[derive(Clone, Copy, Debug)]
struct ReadOnlyProviderError;

impl fmt::Display for ReadOnlyProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("provider is read-only")
    }
}

impl Error for ReadOnlyProviderError {}

struct FakeProviderPort {
    descriptor: ProviderDescriptor,
}

impl ProviderPort for FakeProviderPort {
    type Error = ReadOnlyProviderError;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        _response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        Err(ReadOnlyProviderError)
    }

    fn accept_command(&mut self, _command: AgentCommand) -> Result<(), Self::Error> {
        Err(ReadOnlyProviderError)
    }
}

#[derive(Default)]
struct FakeProviderSource {
    handle: Arc<Mutex<Option<ProviderEventHandle>>>,
}

impl ProviderEventSource for FakeProviderSource {
    type Error = ReadOnlyProviderError;

    fn start(&mut self, events: ProviderEventHandle) -> Result<(), Self::Error> {
        *locked(&self.handle) = Some(events);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        *locked(&self.handle) = None;
        Ok(())
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn timestamp(value: i128) -> Result<Timestamp, Box<dyn Error>> {
    Ok(Timestamp::from_unix_timestamp_nanos(value)?)
}

fn event(
    session_id: SessionId,
    sequence: u64,
    payload: AgentEventPayload,
) -> Result<AgentEvent, Box<dyn Error>> {
    Ok(AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::new(sequence)?,
        timestamp(100 + i128::from(sequence))?,
        payload,
    )?)
}

fn connect_client(address: std::net::SocketAddr) -> Result<WebSocket<TcpStream>, Box<dyn Error>> {
    connect_client_at_path(address, NATIVE_WEBSOCKET_PATH)
}

fn connect_client_at_path(
    address: std::net::SocketAddr,
    path: &str,
) -> Result<WebSocket<TcpStream>, Box<dyn Error>> {
    let uri: Uri = format!("ws://{address}{path}").parse()?;
    let request = ClientRequestBuilder::new(uri).with_sub_protocol(NATIVE_WEBSOCKET_SUBPROTOCOL);
    let stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let (socket, response) = client(request, stream)?;
    assert_eq!(
        response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok()),
        Some(NATIVE_WEBSOCKET_SUBPROTOCOL)
    );
    Ok(socket)
}

fn wait_until_listening(handle: &agentpulse_channel_native::NativeChannelHandle) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(2);
    while handle.snapshot().health != agentpulse_channel_native::NativeChannelHealth::Listening
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(20));
    }
    if handle.snapshot().health != agentpulse_channel_native::NativeChannelHealth::Listening {
        return Err("Native Channel did not return to listening state".into());
    }
    Ok(())
}

fn send_client(socket: &mut WebSocket<TcpStream>, message: &NativeClientMessage) -> TestResult {
    let bytes = encode_client_message(message)?;
    let text = String::from_utf8(bytes)?;
    socket.send(Message::text(text))?;
    Ok(())
}

fn read_server(socket: &mut WebSocket<TcpStream>) -> Result<NativeServerMessage, Box<dyn Error>> {
    loop {
        match socket.read()? {
            Message::Text(text) => return Ok(decode_server_message(text.as_bytes())?),
            Message::Ping(_) | Message::Pong(_) => socket.flush()?,
            Message::Close(_) => return Err("server closed before the expected frame".into()),
            Message::Binary(_) | Message::Frame(_) => {
                return Err("server sent a non-text application frame".into());
            }
        }
    }
}

fn hello(socket: &mut WebSocket<TcpStream>, client_id: String) -> TestResult {
    send_client(
        socket,
        &NativeClientMessage::Hello {
            client_id,
            display_name: "Independent Fake Native Client".to_owned(),
            version: Some("1.0.0-test".to_owned()),
            supported_protocol_versions: vec![V1_PROTOCOL_VERSION],
        },
    )?;
    assert!(matches!(
        read_server(socket)?,
        NativeServerMessage::Hello {
            protocol_version: V1_PROTOCOL_VERSION,
            ..
        }
    ));
    Ok(())
}

fn discover_and_subscribe(
    socket: &mut WebSocket<TcpStream>,
    provider_id: ProviderId,
    session_id: SessionId,
    expected_cursor: EventSequence,
) -> TestResult {
    let discovery_id = Uuid::now_v7().to_string();
    send_client(
        socket,
        &NativeClientMessage::Discover {
            request_id: discovery_id.clone(),
        },
    )?;
    assert!(matches!(
        read_server(socket)?,
        NativeServerMessage::SyncStarted {
            ref request_id,
            provider_count: 1,
            session_count: 1,
        } if request_id == &discovery_id
    ));
    assert!(matches!(
        read_server(socket)?,
        NativeServerMessage::Domain {
            context: NativeDeliveryContext::DiscoveryProvider { .. },
            message,
        } if matches!(message.as_ref(), ProtocolMessage::ProviderDescriptor(descriptor) if descriptor.id() == provider_id)
    ));
    assert!(matches!(
        read_server(socket)?,
        NativeServerMessage::Domain {
            context: NativeDeliveryContext::DiscoverySession { last_sequence, .. },
            message,
        } if matches!(message.as_ref(), ProtocolMessage::AgentSession(session) if session.id() == session_id)
            && last_sequence == expected_cursor
    ));
    assert!(matches!(
        read_server(socket)?,
        NativeServerMessage::SyncCompleted { ref request_id }
            if request_id == &discovery_id
    ));

    let subscription_id = Uuid::now_v7().to_string();
    send_client(
        socket,
        &NativeClientMessage::Subscribe {
            request_id: subscription_id.clone(),
            session_id,
        },
    )?;
    assert!(matches!(
        read_server(socket)?,
        NativeServerMessage::SubscriptionResult {
            ref request_id,
            status: NativeSubscriptionStatus::Subscribed,
            baseline_sequence,
            ..
        } if request_id == &subscription_id && baseline_sequence == expected_cursor
    ));
    assert!(matches!(
        read_server(socket)?,
        NativeServerMessage::Domain {
            context: NativeDeliveryContext::SubscriptionSession { ref request_id },
            message,
        } if request_id == &subscription_id
            && matches!(message.as_ref(), ProtocolMessage::AgentSession(session) if session.id() == session_id)
    ));
    Ok(())
}

#[test]
#[ignore = "requires loopback socket access"]
fn independent_client_discovers_subscribes_streams_and_reconnects() -> TestResult {
    let provider_id = ProviderId::new();
    let channel_id = ChannelId::new();
    let session_id = SessionId::new();
    let source_handle = Arc::new(Mutex::new(None));
    let provider = FakeProviderPort {
        descriptor: ProviderDescriptor::new(
            provider_id,
            ProviderKind::new("native-flow-test")?,
            NonEmptyText::new("Native Flow Provider")?,
            ProviderCapabilities::SESSION_STATE,
        ),
    };
    let provider_source = FakeProviderSource {
        handle: Arc::clone(&source_handle),
    };
    let parts = NativeChannel::build(NativeChannelConfig::new(channel_id))?;
    let (channel, channel_source, channel_handle) = parts.into_parts();
    let mut host = RuntimeHost::new();
    host.register_provider(provider, provider_source)?;
    host.register_channel(channel, channel_source)?;
    let _ = host.start()?;
    let provider_events = locked(&source_handle)
        .clone()
        .ok_or("Provider Event handle was not started")?;
    let session = AgentSession::builder(session_id, provider_id, timestamp(100)?).build()?;
    let _ = provider_events.publish_event(AgentEvent::new(
        EventId::new(),
        session_id,
        EventSequence::FIRST,
        timestamp(100)?,
        AgentEventPayload::SessionStarted(session),
    )?)?;
    let address = channel_handle
        .snapshot()
        .local_address
        .ok_or("Native listener did not expose its address")?;

    let mut client = connect_client(address)?;
    hello(&mut client, Uuid::now_v7().to_string())?;
    discover_and_subscribe(&mut client, provider_id, session_id, EventSequence::FIRST)?;

    let refresh_id = Uuid::now_v7().to_string();
    send_client(
        &mut client,
        &NativeClientMessage::Discover {
            request_id: refresh_id.clone(),
        },
    )?;
    assert!(matches!(
        read_server(&mut client)?,
        NativeServerMessage::SyncStarted {
            ref request_id,
            provider_count: 1,
            session_count: 1,
        } if request_id == &refresh_id
    ));
    assert!(matches!(
        read_server(&mut client)?,
        NativeServerMessage::Domain {
            context: NativeDeliveryContext::DiscoveryProvider { ref request_id },
            ..
        } if request_id == &refresh_id
    ));
    assert!(matches!(
        read_server(&mut client)?,
        NativeServerMessage::Domain {
            context: NativeDeliveryContext::DiscoverySession { ref request_id, .. },
            ..
        } if request_id == &refresh_id
    ));
    assert!(matches!(
        read_server(&mut client)?,
        NativeServerMessage::SyncCompleted { ref request_id }
            if request_id == &refresh_id
    ));

    let _ = provider_events.publish_event(event(
        session_id,
        2,
        AgentEventPayload::Message(AgentMessage::new(
            AgentMessageLevel::Info,
            NonEmptyText::new("live Native message")?,
        )),
    )?)?;
    assert!(matches!(
        read_server(&mut client)?,
        NativeServerMessage::Domain {
            context: NativeDeliveryContext::LiveEvent { .. },
            message,
        } if matches!(message.as_ref(), ProtocolMessage::AgentEvent(event) if event.sequence() == EventSequence::new(2)?)
    ));

    let _ = provider_events.publish_event(event(
        session_id,
        3,
        AgentEventPayload::StateChanged(AgentState::Running),
    )?)?;
    assert!(matches!(
        read_server(&mut client)?,
        NativeServerMessage::Domain {
            context: NativeDeliveryContext::LiveEvent { .. },
            message,
        } if matches!(message.as_ref(), ProtocolMessage::AgentEvent(event) if event.sequence() == EventSequence::new(3)?)
    ));
    assert!(matches!(
        read_server(&mut client)?,
        NativeServerMessage::Domain {
            context: NativeDeliveryContext::LiveSession,
            message,
        } if matches!(message.as_ref(), ProtocolMessage::AgentSession(session) if session.state() == AgentState::Running)
    ));

    client.close(None)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while host.inspect_bridge(|bridge| bridge.is_subscribed(channel_id, session_id))?
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!host.inspect_bridge(|bridge| bridge.is_subscribed(channel_id, session_id))?);

    let mut reconnected = connect_client(address)?;
    hello(&mut reconnected, Uuid::now_v7().to_string())?;
    discover_and_subscribe(
        &mut reconnected,
        provider_id,
        session_id,
        EventSequence::new(3)?,
    )?;
    reconnected.close(None)?;
    let _ = host.stop()?;
    let snapshot = channel_handle.snapshot();
    assert!(snapshot.connections >= 2);
    assert!(snapshot.disconnects >= 1);
    Ok(())
}

#[test]
#[ignore = "requires loopback socket access"]
fn listener_rejects_wrong_path_busy_client_and_incompatible_hello() -> TestResult {
    let parts = NativeChannel::build(NativeChannelConfig::new(ChannelId::new()))?;
    let (channel, channel_source, channel_handle) = parts.into_parts();
    let mut host = RuntimeHost::new();
    host.register_channel(channel, channel_source)?;
    let _ = host.start()?;
    let address = channel_handle
        .snapshot()
        .local_address
        .ok_or("Native listener did not expose its address")?;

    assert!(connect_client_at_path(address, "/wrong").is_err());
    wait_until_listening(&channel_handle)?;

    let mut primary = connect_client(address)?;
    hello(&mut primary, Uuid::now_v7().to_string())?;
    let mut competing = connect_client(address)?;
    assert!(matches!(
        read_server(&mut competing)?,
        NativeServerMessage::Error {
            code: agentpulse_channel_native::NativeErrorCode::ConnectionBusy,
            recoverable: false,
            ..
        }
    ));
    primary.close(None)?;
    wait_until_listening(&channel_handle)?;

    let mut incompatible = connect_client(address)?;
    send_client(
        &mut incompatible,
        &NativeClientMessage::Hello {
            client_id: Uuid::now_v7().to_string(),
            display_name: "Incompatible Native Client".to_owned(),
            version: None,
            supported_protocol_versions: vec![V1_PROTOCOL_VERSION + 1],
        },
    )?;
    assert!(matches!(
        read_server(&mut incompatible)?,
        NativeServerMessage::Error {
            code: agentpulse_channel_native::NativeErrorCode::InvalidHandshake,
            recoverable: false,
            ..
        }
    ));

    let _ = host.stop()?;
    let stopped = channel_handle.snapshot();
    assert_eq!(
        stopped.health,
        agentpulse_channel_native::NativeChannelHealth::Stopped
    );
    assert!(stopped.local_address.is_none());
    assert!(stopped.client_id.is_none());
    Ok(())
}
