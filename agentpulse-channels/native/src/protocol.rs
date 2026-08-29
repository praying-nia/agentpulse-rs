//! Strict Native Transport v1 control and domain-delivery codec.

use std::str::FromStr;

use agentpulse_core::{
    ChannelCapabilities, ChannelDescriptor, ChannelEventRoute, EventSequence, InteractionRoute,
    SessionId,
};
use agentpulse_protocol::{ProtocolMessage, V1_PROTOCOL_VERSION, decode_json, encode_json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{NATIVE_TRANSPORT_VERSION, NativeProtocolError};

/// Client-originated Native Transport control messages.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeClientMessage {
    /// Opens one application-level connection after the WebSocket upgrade.
    Hello {
        /// Self-declared UUIDv7 client instance identity.
        client_id: String,
        /// Nonblank user-facing client name.
        display_name: String,
        /// Optional nonblank client build version.
        version: Option<String>,
        /// Unique domain protocol versions understood by the client.
        supported_protocol_versions: Vec<u16>,
    },
    /// Requests a stable Provider and Session discovery snapshot.
    Discover {
        /// UUIDv7 request correlation identity.
        request_id: String,
    },
    /// Requests live delivery for one discovered Session.
    Subscribe {
        /// UUIDv7 request correlation identity.
        request_id: String,
        /// Target AgentPulse Session.
        session_id: SessionId,
    },
    /// Stops live delivery for one subscribed Session.
    Unsubscribe {
        /// UUIDv7 request correlation identity.
        request_id: String,
        /// Target AgentPulse Session.
        session_id: SessionId,
    },
}

/// Context attached to one nested AgentPulse JSON v1 domain envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeDeliveryContext {
    /// A Provider Descriptor from one discovery request.
    DiscoveryProvider {
        /// The matching discovery request.
        request_id: String,
    },
    /// A Session view from one discovery request.
    DiscoverySession {
        /// The matching discovery request.
        request_id: String,
        /// Last Event represented by this Session Aggregate at snapshot time.
        last_sequence: EventSequence,
    },
    /// The current Session baseline for a successful subscription.
    SubscriptionSession {
        /// The matching subscription request.
        request_id: String,
    },
    /// A live normalized Event and its centralized read-only route.
    LiveEvent {
        /// The route decision consumed by the Native Channel.
        route: NativeEventRoute,
    },
    /// A current Session view emitted after a state-changing live Event.
    LiveSession,
}

/// Read-only event presentation metadata carried to Native clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeEventRoute {
    /// The Event contains no interaction request.
    ObserveOnly,
    /// The Event contains an interaction that must not expose input controls.
    InteractionReadOnly,
}

impl NativeEventRoute {
    pub(crate) fn from_core(route: ChannelEventRoute) -> Result<Self, NativeProtocolError> {
        match route {
            ChannelEventRoute::ObserveOnly => Ok(Self::ObserveOnly),
            ChannelEventRoute::Interaction(InteractionRoute::ReadOnly) => {
                Ok(Self::InteractionReadOnly)
            }
            ChannelEventRoute::Interaction(InteractionRoute::Interactive) => {
                Err(NativeProtocolError::InvalidField {
                    field: "event route",
                    reason: "read-only Native Channel cannot carry an interactive route".to_owned(),
                })
            }
            _ => Err(NativeProtocolError::InvalidField {
                field: "event route",
                reason: "unsupported future route variant".to_owned(),
            }),
        }
    }
}

/// Result of an idempotent Session subscription request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeSubscriptionStatus {
    /// A new subscription and baseline were established.
    Subscribed,
    /// This connection already owned the subscription.
    AlreadySubscribed,
}

/// Result of an idempotent Session unsubscription request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeUnsubscriptionStatus {
    /// The live subscription was removed.
    Unsubscribed,
    /// This connection did not own an active subscription.
    NotSubscribed,
}

/// Stable Native Transport error codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeErrorCode {
    /// Another client already owns the configured Channel connection.
    ConnectionBusy,
    /// The application handshake was missing, repeated, or incompatible.
    InvalidHandshake,
    /// A well-formed request violated the connection state machine.
    InvalidRequest,
    /// The requested Session was not present in the latest discovery snapshot.
    SessionNotDiscovered,
    /// RuntimeHost no longer contains the requested Session.
    SessionNotFound,
    /// The Channel intentionally rejects user Action submission.
    ReadOnly,
    /// An internal runtime operation failed.
    Internal,
}

/// Server-originated Native Transport control or domain-delivery messages.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeServerMessage {
    /// Completes the application handshake and identifies the Channel endpoint.
    Hello {
        /// UUIDv7 identity unique to this accepted connection.
        connection_id: String,
        /// The configured Native Channel descriptor.
        channel: ChannelDescriptor,
        /// Selected AgentPulse domain protocol version.
        protocol_version: u16,
        /// Maximum complete WebSocket text frame size.
        max_frame_bytes: usize,
        /// Server Ping interval in whole seconds.
        ping_interval_seconds: u64,
        /// Connection idle timeout in whole seconds.
        idle_timeout_seconds: u64,
    },
    /// Begins one stable discovery batch.
    SyncStarted {
        /// Matching discovery request.
        request_id: String,
        /// Number of Provider Descriptor frames in the batch.
        provider_count: usize,
        /// Number of Session frames in the batch.
        session_count: usize,
    },
    /// Carries one unchanged AgentPulse JSON v1 domain envelope.
    Domain {
        /// Native delivery metadata outside the domain protocol.
        context: NativeDeliveryContext,
        /// The validated domain message.
        message: ProtocolMessage,
    },
    /// Completes one discovery batch.
    SyncCompleted {
        /// Matching discovery request.
        request_id: String,
    },
    /// Reports an idempotent subscription result and live-stream cursor.
    SubscriptionResult {
        /// Matching subscription request.
        request_id: String,
        /// Target Session.
        session_id: SessionId,
        /// Whether this request created or repeated the subscription.
        status: NativeSubscriptionStatus,
        /// Last Event already represented by the synchronized baseline.
        baseline_sequence: EventSequence,
    },
    /// Reports an idempotent unsubscription result.
    UnsubscriptionResult {
        /// Matching unsubscription request.
        request_id: String,
        /// Target Session.
        session_id: SessionId,
        /// Whether an active subscription was removed.
        status: NativeUnsubscriptionStatus,
    },
    /// Reports a connection-level or request-level failure.
    Error {
        /// Matching request when the failure was request-scoped.
        request_id: Option<String>,
        /// Stable programmatic error code.
        code: NativeErrorCode,
        /// Bounded diagnostic safe to show to a user.
        message: String,
        /// Whether the connection may continue issuing requests.
        recoverable: bool,
    },
}

/// Encodes one validated client control message as strict Native Transport v1 JSON.
pub fn encode_client_message(
    message: &NativeClientMessage,
) -> Result<Vec<u8>, NativeProtocolError> {
    validate_client_message(message)?;
    encode_envelope(ClientMessageDto::from_semantic(message))
}

/// Decodes and validates one strict Native Transport v1 client control frame.
pub fn decode_client_message(input: &[u8]) -> Result<NativeClientMessage, NativeProtocolError> {
    let dto: Envelope<ClientMessageDto> = decode_envelope(input)?;
    let message = dto.message.into_semantic()?;
    validate_client_message(&message)?;
    Ok(message)
}

/// Encodes one validated server control or domain-delivery message.
pub fn encode_server_message(
    message: &NativeServerMessage,
) -> Result<Vec<u8>, NativeProtocolError> {
    validate_server_message(message)?;
    encode_envelope(ServerMessageDto::from_semantic(message)?)
}

/// Decodes and validates one strict Native Transport v1 server frame.
pub fn decode_server_message(input: &[u8]) -> Result<NativeServerMessage, NativeProtocolError> {
    let dto: Envelope<ServerMessageDto> = decode_envelope(input)?;
    let message = dto.message.into_semantic()?;
    validate_server_message(&message)?;
    Ok(message)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    native_transport_version: u16,
    message: T,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientMessageDto {
    ClientHello {
        client_id: String,
        display_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        supported_protocol_versions: Vec<u16>,
    },
    DiscoverSessions {
        request_id: String,
    },
    SubscribeSession {
        request_id: String,
        session_id: String,
    },
    UnsubscribeSession {
        request_id: String,
        session_id: String,
    },
}

impl ClientMessageDto {
    fn from_semantic(message: &NativeClientMessage) -> Self {
        match message {
            NativeClientMessage::Hello {
                client_id,
                display_name,
                version,
                supported_protocol_versions,
            } => Self::ClientHello {
                client_id: client_id.clone(),
                display_name: display_name.clone(),
                version: version.clone(),
                supported_protocol_versions: supported_protocol_versions.clone(),
            },
            NativeClientMessage::Discover { request_id } => Self::DiscoverSessions {
                request_id: request_id.clone(),
            },
            NativeClientMessage::Subscribe {
                request_id,
                session_id,
            } => Self::SubscribeSession {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
            },
            NativeClientMessage::Unsubscribe {
                request_id,
                session_id,
            } => Self::UnsubscribeSession {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
            },
        }
    }

    fn into_semantic(self) -> Result<NativeClientMessage, NativeProtocolError> {
        match self {
            Self::ClientHello {
                client_id,
                display_name,
                version,
                supported_protocol_versions,
            } => Ok(NativeClientMessage::Hello {
                client_id,
                display_name,
                version,
                supported_protocol_versions,
            }),
            Self::DiscoverSessions { request_id } => {
                Ok(NativeClientMessage::Discover { request_id })
            }
            Self::SubscribeSession {
                request_id,
                session_id,
            } => Ok(NativeClientMessage::Subscribe {
                request_id,
                session_id: parse_session_id(session_id)?,
            }),
            Self::UnsubscribeSession {
                request_id,
                session_id,
            } => Ok(NativeClientMessage::Unsubscribe {
                request_id,
                session_id: parse_session_id(session_id)?,
            }),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ServerMessageDto {
    ServerHello {
        connection_id: String,
        channel: Value,
        protocol_version: u16,
        max_frame_bytes: usize,
        ping_interval_seconds: u64,
        idle_timeout_seconds: u64,
    },
    SyncStarted {
        request_id: String,
        provider_count: usize,
        session_count: usize,
    },
    DomainMessage {
        context: DeliveryContextDto,
        domain: Value,
    },
    SyncCompleted {
        request_id: String,
    },
    SubscriptionResult {
        request_id: String,
        session_id: String,
        status: SubscriptionStatusDto,
        baseline_sequence: String,
    },
    UnsubscriptionResult {
        request_id: String,
        session_id: String,
        status: UnsubscriptionStatusDto,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        code: ErrorCodeDto,
        message: String,
        recoverable: bool,
    },
}

impl ServerMessageDto {
    fn from_semantic(message: &NativeServerMessage) -> Result<Self, NativeProtocolError> {
        Ok(match message {
            NativeServerMessage::Hello {
                connection_id,
                channel,
                protocol_version,
                max_frame_bytes,
                ping_interval_seconds,
                idle_timeout_seconds,
            } => Self::ServerHello {
                connection_id: connection_id.clone(),
                channel: encode_domain_value(&ProtocolMessage::ChannelDescriptor(channel.clone()))?,
                protocol_version: *protocol_version,
                max_frame_bytes: *max_frame_bytes,
                ping_interval_seconds: *ping_interval_seconds,
                idle_timeout_seconds: *idle_timeout_seconds,
            },
            NativeServerMessage::SyncStarted {
                request_id,
                provider_count,
                session_count,
            } => Self::SyncStarted {
                request_id: request_id.clone(),
                provider_count: *provider_count,
                session_count: *session_count,
            },
            NativeServerMessage::Domain { context, message } => Self::DomainMessage {
                context: DeliveryContextDto::from_semantic(context),
                domain: encode_domain_value(message)?,
            },
            NativeServerMessage::SyncCompleted { request_id } => Self::SyncCompleted {
                request_id: request_id.clone(),
            },
            NativeServerMessage::SubscriptionResult {
                request_id,
                session_id,
                status,
                baseline_sequence,
            } => Self::SubscriptionResult {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                status: (*status).into(),
                baseline_sequence: baseline_sequence.get().to_string(),
            },
            NativeServerMessage::UnsubscriptionResult {
                request_id,
                session_id,
                status,
            } => Self::UnsubscriptionResult {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                status: (*status).into(),
            },
            NativeServerMessage::Error {
                request_id,
                code,
                message,
                recoverable,
            } => Self::Error {
                request_id: request_id.clone(),
                code: (*code).into(),
                message: message.clone(),
                recoverable: *recoverable,
            },
        })
    }

    fn into_semantic(self) -> Result<NativeServerMessage, NativeProtocolError> {
        match self {
            Self::ServerHello {
                connection_id,
                channel,
                protocol_version,
                max_frame_bytes,
                ping_interval_seconds,
                idle_timeout_seconds,
            } => {
                let ProtocolMessage::ChannelDescriptor(channel) = decode_domain_value(channel)?
                else {
                    return Err(NativeProtocolError::InvalidDomainContext {
                        context: "server_hello",
                        actual: "non-channel_descriptor",
                    });
                };
                Ok(NativeServerMessage::Hello {
                    connection_id,
                    channel,
                    protocol_version,
                    max_frame_bytes,
                    ping_interval_seconds,
                    idle_timeout_seconds,
                })
            }
            Self::SyncStarted {
                request_id,
                provider_count,
                session_count,
            } => Ok(NativeServerMessage::SyncStarted {
                request_id,
                provider_count,
                session_count,
            }),
            Self::DomainMessage { context, domain } => Ok(NativeServerMessage::Domain {
                context: context.into_semantic()?,
                message: decode_domain_value(domain)?,
            }),
            Self::SyncCompleted { request_id } => {
                Ok(NativeServerMessage::SyncCompleted { request_id })
            }
            Self::SubscriptionResult {
                request_id,
                session_id,
                status,
                baseline_sequence,
            } => Ok(NativeServerMessage::SubscriptionResult {
                request_id,
                session_id: parse_session_id(session_id)?,
                status: status.into(),
                baseline_sequence: parse_sequence(baseline_sequence)?,
            }),
            Self::UnsubscriptionResult {
                request_id,
                session_id,
                status,
            } => Ok(NativeServerMessage::UnsubscriptionResult {
                request_id,
                session_id: parse_session_id(session_id)?,
                status: status.into(),
            }),
            Self::Error {
                request_id,
                code,
                message,
                recoverable,
            } => Ok(NativeServerMessage::Error {
                request_id,
                code: code.into(),
                message,
                recoverable,
            }),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum DeliveryContextDto {
    DiscoveryProvider {
        request_id: String,
    },
    DiscoverySession {
        request_id: String,
        last_sequence: String,
    },
    SubscriptionSession {
        request_id: String,
    },
    LiveEvent {
        route: EventRouteDto,
    },
    LiveSession,
}

impl DeliveryContextDto {
    fn from_semantic(context: &NativeDeliveryContext) -> Self {
        match context {
            NativeDeliveryContext::DiscoveryProvider { request_id } => Self::DiscoveryProvider {
                request_id: request_id.clone(),
            },
            NativeDeliveryContext::DiscoverySession {
                request_id,
                last_sequence,
            } => Self::DiscoverySession {
                request_id: request_id.clone(),
                last_sequence: last_sequence.get().to_string(),
            },
            NativeDeliveryContext::SubscriptionSession { request_id } => {
                Self::SubscriptionSession {
                    request_id: request_id.clone(),
                }
            }
            NativeDeliveryContext::LiveEvent { route } => Self::LiveEvent {
                route: (*route).into(),
            },
            NativeDeliveryContext::LiveSession => Self::LiveSession,
        }
    }

    fn into_semantic(self) -> Result<NativeDeliveryContext, NativeProtocolError> {
        match self {
            Self::DiscoveryProvider { request_id } => {
                Ok(NativeDeliveryContext::DiscoveryProvider { request_id })
            }
            Self::DiscoverySession {
                request_id,
                last_sequence,
            } => Ok(NativeDeliveryContext::DiscoverySession {
                request_id,
                last_sequence: parse_sequence(last_sequence)?,
            }),
            Self::SubscriptionSession { request_id } => {
                Ok(NativeDeliveryContext::SubscriptionSession { request_id })
            }
            Self::LiveEvent { route } => Ok(NativeDeliveryContext::LiveEvent {
                route: route.into(),
            }),
            Self::LiveSession => Ok(NativeDeliveryContext::LiveSession),
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EventRouteDto {
    ObserveOnly,
    InteractionReadOnly,
}

impl From<NativeEventRoute> for EventRouteDto {
    fn from(value: NativeEventRoute) -> Self {
        match value {
            NativeEventRoute::ObserveOnly => Self::ObserveOnly,
            NativeEventRoute::InteractionReadOnly => Self::InteractionReadOnly,
        }
    }
}

impl From<EventRouteDto> for NativeEventRoute {
    fn from(value: EventRouteDto) -> Self {
        match value {
            EventRouteDto::ObserveOnly => Self::ObserveOnly,
            EventRouteDto::InteractionReadOnly => Self::InteractionReadOnly,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SubscriptionStatusDto {
    Subscribed,
    AlreadySubscribed,
}

impl From<NativeSubscriptionStatus> for SubscriptionStatusDto {
    fn from(value: NativeSubscriptionStatus) -> Self {
        match value {
            NativeSubscriptionStatus::Subscribed => Self::Subscribed,
            NativeSubscriptionStatus::AlreadySubscribed => Self::AlreadySubscribed,
        }
    }
}

impl From<SubscriptionStatusDto> for NativeSubscriptionStatus {
    fn from(value: SubscriptionStatusDto) -> Self {
        match value {
            SubscriptionStatusDto::Subscribed => Self::Subscribed,
            SubscriptionStatusDto::AlreadySubscribed => Self::AlreadySubscribed,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UnsubscriptionStatusDto {
    Unsubscribed,
    NotSubscribed,
}

impl From<NativeUnsubscriptionStatus> for UnsubscriptionStatusDto {
    fn from(value: NativeUnsubscriptionStatus) -> Self {
        match value {
            NativeUnsubscriptionStatus::Unsubscribed => Self::Unsubscribed,
            NativeUnsubscriptionStatus::NotSubscribed => Self::NotSubscribed,
        }
    }
}

impl From<UnsubscriptionStatusDto> for NativeUnsubscriptionStatus {
    fn from(value: UnsubscriptionStatusDto) -> Self {
        match value {
            UnsubscriptionStatusDto::Unsubscribed => Self::Unsubscribed,
            UnsubscriptionStatusDto::NotSubscribed => Self::NotSubscribed,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCodeDto {
    ConnectionBusy,
    InvalidHandshake,
    InvalidRequest,
    SessionNotDiscovered,
    SessionNotFound,
    ReadOnly,
    Internal,
}

impl From<NativeErrorCode> for ErrorCodeDto {
    fn from(value: NativeErrorCode) -> Self {
        match value {
            NativeErrorCode::ConnectionBusy => Self::ConnectionBusy,
            NativeErrorCode::InvalidHandshake => Self::InvalidHandshake,
            NativeErrorCode::InvalidRequest => Self::InvalidRequest,
            NativeErrorCode::SessionNotDiscovered => Self::SessionNotDiscovered,
            NativeErrorCode::SessionNotFound => Self::SessionNotFound,
            NativeErrorCode::ReadOnly => Self::ReadOnly,
            NativeErrorCode::Internal => Self::Internal,
        }
    }
}

impl From<ErrorCodeDto> for NativeErrorCode {
    fn from(value: ErrorCodeDto) -> Self {
        match value {
            ErrorCodeDto::ConnectionBusy => Self::ConnectionBusy,
            ErrorCodeDto::InvalidHandshake => Self::InvalidHandshake,
            ErrorCodeDto::InvalidRequest => Self::InvalidRequest,
            ErrorCodeDto::SessionNotDiscovered => Self::SessionNotDiscovered,
            ErrorCodeDto::SessionNotFound => Self::SessionNotFound,
            ErrorCodeDto::ReadOnly => Self::ReadOnly,
            ErrorCodeDto::Internal => Self::Internal,
        }
    }
}

fn encode_envelope<T: Serialize>(message: T) -> Result<Vec<u8>, NativeProtocolError> {
    serde_json::to_vec(&Envelope {
        native_transport_version: NATIVE_TRANSPORT_VERSION,
        message,
    })
    .map_err(|source| NativeProtocolError::Json { source })
}

fn decode_envelope<T>(input: &[u8]) -> Result<Envelope<T>, NativeProtocolError>
where
    T: for<'de> Deserialize<'de>,
{
    let value: Value =
        serde_json::from_slice(input).map_err(|source| NativeProtocolError::Json { source })?;
    let received = value
        .get("native_transport_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| NativeProtocolError::InvalidField {
            field: "native_transport_version",
            reason: "expected an unsigned JSON integer".to_owned(),
        })?;
    if received != u64::from(NATIVE_TRANSPORT_VERSION) {
        return Err(NativeProtocolError::UnsupportedVersion {
            received,
            supported: NATIVE_TRANSPORT_VERSION,
        });
    }
    serde_json::from_value(value).map_err(|source| NativeProtocolError::Json { source })
}

fn validate_client_message(message: &NativeClientMessage) -> Result<(), NativeProtocolError> {
    match message {
        NativeClientMessage::Hello {
            client_id,
            display_name,
            version,
            supported_protocol_versions,
        } => {
            validate_uuid_v7("client_id", client_id)?;
            validate_nonblank("display_name", display_name)?;
            if let Some(version) = version {
                validate_nonblank("version", version)?;
            }
            if supported_protocol_versions.is_empty() {
                return Err(NativeProtocolError::InvalidField {
                    field: "supported_protocol_versions",
                    reason: "must not be empty".to_owned(),
                });
            }
            let mut unique = supported_protocol_versions.clone();
            unique.sort_unstable();
            unique.dedup();
            if unique.len() != supported_protocol_versions.len() {
                return Err(NativeProtocolError::InvalidField {
                    field: "supported_protocol_versions",
                    reason: "must not contain duplicates".to_owned(),
                });
            }
        }
        NativeClientMessage::Discover { request_id }
        | NativeClientMessage::Subscribe { request_id, .. }
        | NativeClientMessage::Unsubscribe { request_id, .. } => {
            validate_uuid_v7("request_id", request_id)?;
        }
    }
    Ok(())
}

fn validate_server_message(message: &NativeServerMessage) -> Result<(), NativeProtocolError> {
    match message {
        NativeServerMessage::Hello {
            connection_id,
            channel,
            protocol_version,
            max_frame_bytes,
            ping_interval_seconds,
            idle_timeout_seconds,
            ..
        } => {
            validate_uuid_v7("connection_id", connection_id)?;
            let expected_capabilities = ChannelCapabilities::NOTIFICATION
                | ChannelCapabilities::SESSION_VIEW
                | ChannelCapabilities::REALTIME_SYNC;
            if channel.kind().as_str() != "native" {
                return invalid("channel.kind", "server must identify a native Channel");
            }
            if channel.capabilities() != expected_capabilities {
                return invalid(
                    "channel.capabilities",
                    "Native v1 requires exactly notification, session_view, and realtime_sync",
                );
            }
            if *protocol_version != V1_PROTOCOL_VERSION {
                return invalid("protocol_version", "server must select AgentPulse JSON v1");
            }
            if *max_frame_bytes == 0 || *ping_interval_seconds == 0 || *idle_timeout_seconds == 0 {
                return invalid(
                    "server_hello limits",
                    "all limits must be greater than zero",
                );
            }
        }
        NativeServerMessage::SyncStarted { request_id, .. }
        | NativeServerMessage::SyncCompleted { request_id }
        | NativeServerMessage::SubscriptionResult { request_id, .. }
        | NativeServerMessage::UnsubscriptionResult { request_id, .. } => {
            validate_uuid_v7("request_id", request_id)?;
        }
        NativeServerMessage::Domain { context, message } => {
            validate_delivery_context(context, message)?;
        }
        NativeServerMessage::Error {
            request_id,
            message,
            ..
        } => {
            if let Some(request_id) = request_id {
                validate_uuid_v7("request_id", request_id)?;
            }
            validate_nonblank("error.message", message)?;
        }
    }
    Ok(())
}

fn validate_delivery_context(
    context: &NativeDeliveryContext,
    message: &ProtocolMessage,
) -> Result<(), NativeProtocolError> {
    let (context_name, valid) = match context {
        NativeDeliveryContext::DiscoveryProvider { request_id } => {
            validate_uuid_v7("request_id", request_id)?;
            (
                "discovery_provider",
                matches!(message, ProtocolMessage::ProviderDescriptor(_)),
            )
        }
        NativeDeliveryContext::DiscoverySession { request_id, .. } => {
            validate_uuid_v7("request_id", request_id)?;
            (
                "discovery_session",
                matches!(message, ProtocolMessage::AgentSession(_)),
            )
        }
        NativeDeliveryContext::SubscriptionSession { request_id } => {
            validate_uuid_v7("request_id", request_id)?;
            (
                "subscription_session",
                matches!(message, ProtocolMessage::AgentSession(_)),
            )
        }
        NativeDeliveryContext::LiveEvent { .. } => (
            "live_event",
            matches!(message, ProtocolMessage::AgentEvent(_)),
        ),
        NativeDeliveryContext::LiveSession => (
            "live_session",
            matches!(message, ProtocolMessage::AgentSession(_)),
        ),
    };
    if valid {
        Ok(())
    } else {
        Err(NativeProtocolError::InvalidDomainContext {
            context: context_name,
            actual: protocol_message_type(message),
        })
    }
}

fn validate_uuid_v7(field: &'static str, value: &str) -> Result<(), NativeProtocolError> {
    let parsed = Uuid::parse_str(value).map_err(|error| NativeProtocolError::InvalidField {
        field,
        reason: error.to_string(),
    })?;
    if parsed.get_version_num() != 7 || parsed.to_string() != value {
        return invalid(field, "expected a canonical lowercase UUIDv7 string");
    }
    Ok(())
}

fn validate_nonblank(field: &'static str, value: &str) -> Result<(), NativeProtocolError> {
    if value.trim().is_empty() {
        invalid(field, "must not be blank")
    } else {
        Ok(())
    }
}

fn invalid<T>(field: &'static str, reason: &str) -> Result<T, NativeProtocolError> {
    Err(NativeProtocolError::InvalidField {
        field,
        reason: reason.to_owned(),
    })
}

fn parse_session_id(value: String) -> Result<SessionId, NativeProtocolError> {
    SessionId::from_str(&value).map_err(|error| NativeProtocolError::InvalidField {
        field: "session_id",
        reason: error.to_string(),
    })
}

fn parse_sequence(value: String) -> Result<EventSequence, NativeProtocolError> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return invalid(
            "event sequence",
            "expected a canonical unsigned decimal string",
        );
    }
    let parsed = value
        .parse::<u64>()
        .map_err(|error| NativeProtocolError::InvalidField {
            field: "event sequence",
            reason: error.to_string(),
        })?;
    EventSequence::new(parsed).map_err(|error| NativeProtocolError::InvalidField {
        field: "event sequence",
        reason: error.to_string(),
    })
}

fn encode_domain_value(message: &ProtocolMessage) -> Result<Value, NativeProtocolError> {
    let bytes = encode_json(message)?;
    serde_json::from_slice(&bytes).map_err(|source| NativeProtocolError::Json { source })
}

fn decode_domain_value(value: Value) -> Result<ProtocolMessage, NativeProtocolError> {
    let bytes =
        serde_json::to_vec(&value).map_err(|source| NativeProtocolError::Json { source })?;
    decode_json(&bytes).map_err(Into::into)
}

const fn protocol_message_type(message: &ProtocolMessage) -> &'static str {
    match message {
        ProtocolMessage::ProviderDescriptor(_) => "provider_descriptor",
        ProtocolMessage::ChannelDescriptor(_) => "channel_descriptor",
        ProtocolMessage::AgentSession(_) => "agent_session",
        ProtocolMessage::AgentEvent(_) => "agent_event",
        ProtocolMessage::InteractionResponse(_) => "interaction_response",
        ProtocolMessage::AgentCommand(_) => "agent_command",
        _ => "future_domain_message",
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, str::FromStr};

    use agentpulse_core::{
        AgentSession, ChannelCapabilities, ChannelDescriptor, ChannelId, ChannelKind, NonEmptyText,
        ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind, SessionId, Timestamp,
    };

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    const CLIENT_ID: &str = "019976a4-00f4-7db3-996a-9ee0158f7499";
    const REQUEST_ID: &str = "019976a4-00f5-7876-a127-aa91d21546af";
    const CHANNEL_ID: &str = "019976a4-00f6-7d6d-b866-90ddbec53537";
    const PROVIDER_ID: &str = "019976a4-00f7-7ab4-b04d-41a21b57c2ab";
    const SESSION_ID: &str = "019976a4-00f0-7312-b36c-d01f9c5c06f6";

    fn channel_descriptor() -> Result<ChannelDescriptor, Box<dyn Error>> {
        Ok(ChannelDescriptor::new(
            ChannelId::from_str(CHANNEL_ID)?,
            ChannelKind::new("native")?,
            NonEmptyText::new("Native Local")?,
            ChannelCapabilities::NOTIFICATION
                | ChannelCapabilities::SESSION_VIEW
                | ChannelCapabilities::REALTIME_SYNC,
        ))
    }

    fn provider_descriptor() -> Result<ProviderDescriptor, Box<dyn Error>> {
        Ok(ProviderDescriptor::new(
            ProviderId::from_str(PROVIDER_ID)?,
            ProviderKind::new("codex")?,
            NonEmptyText::new("Codex")?,
            ProviderCapabilities::SESSION_STATE,
        ))
    }

    fn session() -> Result<AgentSession, Box<dyn Error>> {
        Ok(AgentSession::builder(
            SessionId::from_str(SESSION_ID)?,
            ProviderId::from_str(PROVIDER_ID)?,
            Timestamp::from_unix_timestamp_nanos(100)?,
        )
        .build()?)
    }

    #[test]
    fn client_control_messages_round_trip_strictly() -> TestResult {
        let messages = [
            NativeClientMessage::Hello {
                client_id: CLIENT_ID.to_owned(),
                display_name: "Fixture Native Client".to_owned(),
                version: Some("1.0.0".to_owned()),
                supported_protocol_versions: vec![V1_PROTOCOL_VERSION],
            },
            NativeClientMessage::Discover {
                request_id: REQUEST_ID.to_owned(),
            },
            NativeClientMessage::Subscribe {
                request_id: REQUEST_ID.to_owned(),
                session_id: SessionId::from_str(SESSION_ID)?,
            },
            NativeClientMessage::Unsubscribe {
                request_id: REQUEST_ID.to_owned(),
                session_id: SessionId::from_str(SESSION_ID)?,
            },
        ];
        for message in messages {
            let encoded = encode_client_message(&message)?;
            assert_eq!(decode_client_message(&encoded)?, message);
        }
        Ok(())
    }

    #[test]
    fn server_control_and_nested_domain_messages_round_trip() -> TestResult {
        let messages = [
            NativeServerMessage::Hello {
                connection_id: CLIENT_ID.to_owned(),
                channel: channel_descriptor()?,
                protocol_version: V1_PROTOCOL_VERSION,
                max_frame_bytes: 1024 * 1024,
                ping_interval_seconds: 15,
                idle_timeout_seconds: 45,
            },
            NativeServerMessage::SyncStarted {
                request_id: REQUEST_ID.to_owned(),
                provider_count: 1,
                session_count: 1,
            },
            NativeServerMessage::Domain {
                context: NativeDeliveryContext::DiscoveryProvider {
                    request_id: REQUEST_ID.to_owned(),
                },
                message: ProtocolMessage::ProviderDescriptor(provider_descriptor()?),
            },
            NativeServerMessage::Domain {
                context: NativeDeliveryContext::DiscoverySession {
                    request_id: REQUEST_ID.to_owned(),
                    last_sequence: EventSequence::FIRST,
                },
                message: ProtocolMessage::AgentSession(session()?),
            },
            NativeServerMessage::SubscriptionResult {
                request_id: REQUEST_ID.to_owned(),
                session_id: SessionId::from_str(SESSION_ID)?,
                status: NativeSubscriptionStatus::Subscribed,
                baseline_sequence: EventSequence::FIRST,
            },
        ];
        for message in messages {
            let encoded = encode_server_message(&message)?;
            assert_eq!(decode_server_message(&encoded)?, message);
        }
        Ok(())
    }

    #[test]
    fn codec_rejects_unknown_fields_versions_and_invalid_domain_contexts() -> TestResult {
        let unknown = format!(
            "{{\"native_transport_version\":1,\"message\":{{\"type\":\"discover_sessions\",\"request_id\":\"{REQUEST_ID}\",\"extra\":true}}}}"
        );
        assert!(matches!(
            decode_client_message(unknown.as_bytes()),
            Err(NativeProtocolError::Json { .. })
        ));
        let wrong_version = format!(
            "{{\"native_transport_version\":2,\"message\":{{\"type\":\"discover_sessions\",\"request_id\":\"{REQUEST_ID}\"}}}}"
        );
        assert!(matches!(
            decode_client_message(wrong_version.as_bytes()),
            Err(NativeProtocolError::UnsupportedVersion { received: 2, .. })
        ));
        let invalid_context = NativeServerMessage::Domain {
            context: NativeDeliveryContext::LiveSession,
            message: ProtocolMessage::ProviderDescriptor(provider_descriptor()?),
        };
        assert!(matches!(
            encode_server_message(&invalid_context),
            Err(NativeProtocolError::InvalidDomainContext { .. })
        ));
        assert!(matches!(
            NativeEventRoute::from_core(ChannelEventRoute::Interaction(
                InteractionRoute::Interactive
            )),
            Err(NativeProtocolError::InvalidField { .. })
        ));
        Ok(())
    }
}
