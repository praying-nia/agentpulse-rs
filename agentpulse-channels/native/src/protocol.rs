//! Strict Native Transport v3 control and incremental domain-delivery codec.

use std::{collections::BTreeMap, str::FromStr};

use agentpulse_core::{
    AgentCommand, ChannelCapabilities, ChannelDescriptor, ChannelEventRoute, EventSequence,
    InteractionResponse, InteractionRoute, SessionId,
};
use agentpulse_protocol::{ProtocolMessage, V2_PROTOCOL_VERSION, decode_json, encode_json};
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
        /// Host run observed by the cached client state, when any.
        host_run_id: Option<String>,
        /// Last contiguous Event cursor retained for each Session.
        session_cursors: BTreeMap<SessionId, u64>,
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
    /// Submits one response to an interaction in an actively subscribed Session.
    SubmitInteractionResponse {
        /// UUIDv7 transport request correlation identity.
        request_id: String,
        /// Strict AgentPulse domain response.
        response: InteractionResponse,
    },
    /// Submits one typed command to an actively subscribed Session.
    SubmitCommand {
        /// UUIDv7 transport request correlation identity.
        request_id: String,
        /// Strict AgentPulse domain command.
        command: AgentCommand,
    },
}

/// Context attached to one nested AgentPulse JSON v2 domain envelope.
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
    /// One interaction captured in the same subscription baseline.
    SubscriptionInteraction {
        /// The matching subscription request.
        request_id: String,
        /// Centralized route for the receiving Channel.
        route: NativeEventRoute,
    },
    /// A live normalized Event and its centralized interaction route.
    LiveEvent {
        /// The route decision consumed by the Native Channel.
        route: NativeEventRoute,
    },
    /// A current Session view emitted after a state-changing live Event.
    LiveSession,
}

/// Event presentation and interaction metadata carried to Native clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeEventRoute {
    /// The Event contains no interaction request.
    ObserveOnly,
    /// The Event contains an interaction that must not expose input controls.
    InteractionReadOnly,
    /// The Event contains an interaction that may accept a response.
    InteractionInteractive,
}

impl NativeEventRoute {
    pub(crate) fn from_core(route: ChannelEventRoute) -> Result<Self, NativeProtocolError> {
        match route {
            ChannelEventRoute::ObserveOnly => Ok(Self::ObserveOnly),
            ChannelEventRoute::Interaction(InteractionRoute::ReadOnly) => {
                Ok(Self::InteractionReadOnly)
            }
            ChannelEventRoute::Interaction(InteractionRoute::Interactive) => {
                Ok(Self::InteractionInteractive)
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
    /// A bounded historical page was delivered; another request is required.
    CatchingUp,
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
    /// An internal runtime operation failed.
    Internal,
    /// The complete Provider-to-Channel capability route is unavailable.
    CapabilityUnavailable,
    /// The interaction no longer exists in the current Session.
    InteractionNotPending,
    /// The submitting client does not own an active Session subscription.
    SessionNotSubscribed,
    /// The Provider declined an otherwise valid handoff.
    ProviderRejected,
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
        /// UUIDv7 identity for this in-memory Host run.
        host_run_id: String,
        /// Whether the server accepted the client-provided cursors.
        resume_accepted: bool,
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
    /// Carries one unchanged AgentPulse JSON v2 domain envelope.
    Domain {
        /// Native delivery metadata outside the domain protocol.
        context: NativeDeliveryContext,
        /// The validated domain message.
        message: Box<ProtocolMessage>,
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
        /// Number of pending interaction frames in this baseline.
        pending_interaction_count: usize,
        /// Number of historical Event frames in this synchronization page.
        event_count: usize,
        /// Whether prior client state for this Session must be discarded.
        reset: bool,
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
    /// Confirms that an interaction response was accepted by the Provider queue.
    InteractionResponseResult {
        /// Matching transport request.
        request_id: String,
        /// Target Session.
        session_id: SessionId,
        /// Correlated AgentPulse interaction identifier.
        interaction_id: String,
    },
    /// Confirms that a command was accepted by the Provider queue.
    CommandResult {
        /// Matching transport request.
        request_id: String,
        /// Target Session.
        session_id: SessionId,
        /// Correlated AgentPulse command identifier.
        command_id: String,
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

/// Encodes one validated client control message as strict Native Transport v3 JSON.
pub fn encode_client_message(
    message: &NativeClientMessage,
) -> Result<Vec<u8>, NativeProtocolError> {
    validate_client_message(message)?;
    encode_envelope(ClientMessageDto::from_semantic(message)?)
}

/// Decodes and validates one strict Native Transport v3 client control frame.
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

/// Decodes and validates one strict Native Transport v3 server frame.
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
#[serde(deny_unknown_fields)]
struct SessionCursorDto {
    session_id: String,
    last_sequence: String,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_run_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        session_cursors: Vec<SessionCursorDto>,
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
    SubmitInteractionResponse {
        request_id: String,
        response: Value,
    },
    SubmitCommand {
        request_id: String,
        command: Value,
    },
}

impl ClientMessageDto {
    fn from_semantic(message: &NativeClientMessage) -> Result<Self, NativeProtocolError> {
        Ok(match message {
            NativeClientMessage::Hello {
                client_id,
                display_name,
                version,
                supported_protocol_versions,
                host_run_id,
                session_cursors,
            } => Self::ClientHello {
                client_id: client_id.clone(),
                display_name: display_name.clone(),
                version: version.clone(),
                supported_protocol_versions: supported_protocol_versions.clone(),
                host_run_id: host_run_id.clone(),
                session_cursors: session_cursors
                    .iter()
                    .map(|(session_id, last_sequence)| SessionCursorDto {
                        session_id: session_id.to_string(),
                        last_sequence: last_sequence.to_string(),
                    })
                    .collect(),
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
            NativeClientMessage::SubmitInteractionResponse {
                request_id,
                response,
            } => Self::SubmitInteractionResponse {
                request_id: request_id.clone(),
                response: encode_domain_value(&ProtocolMessage::InteractionResponse(
                    response.clone(),
                ))?,
            },
            NativeClientMessage::SubmitCommand {
                request_id,
                command,
            } => Self::SubmitCommand {
                request_id: request_id.clone(),
                command: encode_domain_value(&ProtocolMessage::AgentCommand(command.clone()))?,
            },
        })
    }

    fn into_semantic(self) -> Result<NativeClientMessage, NativeProtocolError> {
        match self {
            Self::ClientHello {
                client_id,
                display_name,
                version,
                supported_protocol_versions,
                host_run_id,
                session_cursors,
            } => Ok(NativeClientMessage::Hello {
                client_id,
                display_name,
                version,
                supported_protocol_versions,
                host_run_id,
                session_cursors: parse_session_cursors(session_cursors)?,
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
            Self::SubmitInteractionResponse {
                request_id,
                response,
            } => {
                let ProtocolMessage::InteractionResponse(response) = decode_domain_value(response)?
                else {
                    return Err(NativeProtocolError::InvalidDomainContext {
                        context: "submit_interaction_response",
                        actual: "non-interaction_response",
                    });
                };
                Ok(NativeClientMessage::SubmitInteractionResponse {
                    request_id,
                    response,
                })
            }
            Self::SubmitCommand {
                request_id,
                command,
            } => {
                let ProtocolMessage::AgentCommand(command) = decode_domain_value(command)? else {
                    return Err(NativeProtocolError::InvalidDomainContext {
                        context: "submit_command",
                        actual: "non-agent_command",
                    });
                };
                Ok(NativeClientMessage::SubmitCommand {
                    request_id,
                    command,
                })
            }
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
        host_run_id: String,
        resume_accepted: bool,
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
        pending_interaction_count: usize,
        event_count: usize,
        reset: bool,
    },
    UnsubscriptionResult {
        request_id: String,
        session_id: String,
        status: UnsubscriptionStatusDto,
    },
    InteractionResponseResult {
        request_id: String,
        session_id: String,
        interaction_id: String,
    },
    CommandResult {
        request_id: String,
        session_id: String,
        command_id: String,
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
                host_run_id,
                resume_accepted,
            } => Self::ServerHello {
                connection_id: connection_id.clone(),
                channel: encode_domain_value(&ProtocolMessage::ChannelDescriptor(channel.clone()))?,
                protocol_version: *protocol_version,
                max_frame_bytes: *max_frame_bytes,
                ping_interval_seconds: *ping_interval_seconds,
                idle_timeout_seconds: *idle_timeout_seconds,
                host_run_id: host_run_id.clone(),
                resume_accepted: *resume_accepted,
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
                pending_interaction_count,
                event_count,
                reset,
            } => Self::SubscriptionResult {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                status: (*status).into(),
                baseline_sequence: baseline_sequence.get().to_string(),
                pending_interaction_count: *pending_interaction_count,
                event_count: *event_count,
                reset: *reset,
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
            NativeServerMessage::InteractionResponseResult {
                request_id,
                session_id,
                interaction_id,
            } => Self::InteractionResponseResult {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                interaction_id: interaction_id.clone(),
            },
            NativeServerMessage::CommandResult {
                request_id,
                session_id,
                command_id,
            } => Self::CommandResult {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                command_id: command_id.clone(),
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
                host_run_id,
                resume_accepted,
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
                    host_run_id,
                    resume_accepted,
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
                message: Box::new(decode_domain_value(domain)?),
            }),
            Self::SyncCompleted { request_id } => {
                Ok(NativeServerMessage::SyncCompleted { request_id })
            }
            Self::SubscriptionResult {
                request_id,
                session_id,
                status,
                baseline_sequence,
                pending_interaction_count,
                event_count,
                reset,
            } => Ok(NativeServerMessage::SubscriptionResult {
                request_id,
                session_id: parse_session_id(session_id)?,
                status: status.into(),
                baseline_sequence: parse_sequence(baseline_sequence)?,
                pending_interaction_count,
                event_count,
                reset,
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
            Self::InteractionResponseResult {
                request_id,
                session_id,
                interaction_id,
            } => Ok(NativeServerMessage::InteractionResponseResult {
                request_id,
                session_id: parse_session_id(session_id)?,
                interaction_id,
            }),
            Self::CommandResult {
                request_id,
                session_id,
                command_id,
            } => Ok(NativeServerMessage::CommandResult {
                request_id,
                session_id: parse_session_id(session_id)?,
                command_id,
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
    SubscriptionInteraction {
        request_id: String,
        route: EventRouteDto,
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
            NativeDeliveryContext::SubscriptionInteraction { request_id, route } => {
                Self::SubscriptionInteraction {
                    request_id: request_id.clone(),
                    route: (*route).into(),
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
            Self::SubscriptionInteraction { request_id, route } => {
                Ok(NativeDeliveryContext::SubscriptionInteraction {
                    request_id,
                    route: route.into(),
                })
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
    InteractionInteractive,
}

impl From<NativeEventRoute> for EventRouteDto {
    fn from(value: NativeEventRoute) -> Self {
        match value {
            NativeEventRoute::ObserveOnly => Self::ObserveOnly,
            NativeEventRoute::InteractionReadOnly => Self::InteractionReadOnly,
            NativeEventRoute::InteractionInteractive => Self::InteractionInteractive,
        }
    }
}

impl From<EventRouteDto> for NativeEventRoute {
    fn from(value: EventRouteDto) -> Self {
        match value {
            EventRouteDto::ObserveOnly => Self::ObserveOnly,
            EventRouteDto::InteractionReadOnly => Self::InteractionReadOnly,
            EventRouteDto::InteractionInteractive => Self::InteractionInteractive,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SubscriptionStatusDto {
    CatchingUp,
    Subscribed,
    AlreadySubscribed,
}

impl From<NativeSubscriptionStatus> for SubscriptionStatusDto {
    fn from(value: NativeSubscriptionStatus) -> Self {
        match value {
            NativeSubscriptionStatus::CatchingUp => Self::CatchingUp,
            NativeSubscriptionStatus::Subscribed => Self::Subscribed,
            NativeSubscriptionStatus::AlreadySubscribed => Self::AlreadySubscribed,
        }
    }
}

impl From<SubscriptionStatusDto> for NativeSubscriptionStatus {
    fn from(value: SubscriptionStatusDto) -> Self {
        match value {
            SubscriptionStatusDto::CatchingUp => Self::CatchingUp,
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
    Internal,
    CapabilityUnavailable,
    InteractionNotPending,
    SessionNotSubscribed,
    ProviderRejected,
}

impl From<NativeErrorCode> for ErrorCodeDto {
    fn from(value: NativeErrorCode) -> Self {
        match value {
            NativeErrorCode::ConnectionBusy => Self::ConnectionBusy,
            NativeErrorCode::InvalidHandshake => Self::InvalidHandshake,
            NativeErrorCode::InvalidRequest => Self::InvalidRequest,
            NativeErrorCode::SessionNotDiscovered => Self::SessionNotDiscovered,
            NativeErrorCode::SessionNotFound => Self::SessionNotFound,
            NativeErrorCode::Internal => Self::Internal,
            NativeErrorCode::CapabilityUnavailable => Self::CapabilityUnavailable,
            NativeErrorCode::InteractionNotPending => Self::InteractionNotPending,
            NativeErrorCode::SessionNotSubscribed => Self::SessionNotSubscribed,
            NativeErrorCode::ProviderRejected => Self::ProviderRejected,
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
            ErrorCodeDto::Internal => Self::Internal,
            ErrorCodeDto::CapabilityUnavailable => Self::CapabilityUnavailable,
            ErrorCodeDto::InteractionNotPending => Self::InteractionNotPending,
            ErrorCodeDto::SessionNotSubscribed => Self::SessionNotSubscribed,
            ErrorCodeDto::ProviderRejected => Self::ProviderRejected,
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
            host_run_id,
            session_cursors,
        } => {
            validate_uuid_v7("client_id", client_id)?;
            validate_nonblank("display_name", display_name)?;
            if let Some(version) = version {
                validate_nonblank("version", version)?;
            }
            if let Some(host_run_id) = host_run_id {
                validate_uuid_v7("host_run_id", host_run_id)?;
            }
            if host_run_id.is_none() && !session_cursors.is_empty() {
                return invalid(
                    "session_cursors",
                    "stored Session cursors require host_run_id",
                );
            }
            if session_cursors.values().any(|sequence| *sequence == 0) {
                return invalid("session_cursors", "stored Session cursors must be positive");
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
        | NativeClientMessage::Unsubscribe { request_id, .. }
        | NativeClientMessage::SubmitInteractionResponse { request_id, .. }
        | NativeClientMessage::SubmitCommand { request_id, .. } => {
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
            host_run_id,
            ..
        } => {
            validate_uuid_v7("connection_id", connection_id)?;
            validate_uuid_v7("host_run_id", host_run_id)?;
            let expected_capabilities = ChannelCapabilities::NOTIFICATION
                | ChannelCapabilities::SESSION_VIEW
                | ChannelCapabilities::REALTIME_SYNC
                | ChannelCapabilities::APPROVAL
                | ChannelCapabilities::FORM_INPUT
                | ChannelCapabilities::TEXT_INPUT
                | ChannelCapabilities::REMOTE_COMMAND;
            if channel.kind().as_str() != "native" {
                return invalid("channel.kind", "server must identify a native Channel");
            }
            if channel.capabilities() != expected_capabilities {
                return invalid(
                    "channel.capabilities",
                    "Native v3 requires its complete interactive command capability set",
                );
            }
            if *protocol_version != V2_PROTOCOL_VERSION {
                return invalid("protocol_version", "server must select AgentPulse JSON v2");
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
        NativeServerMessage::InteractionResponseResult {
            request_id,
            interaction_id,
            ..
        } => {
            validate_uuid_v7("request_id", request_id)?;
            validate_uuid_v7("interaction_id", interaction_id)?;
        }
        NativeServerMessage::CommandResult {
            request_id,
            command_id,
            ..
        } => {
            validate_uuid_v7("request_id", request_id)?;
            validate_uuid_v7("command_id", command_id)?;
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
        NativeDeliveryContext::SubscriptionInteraction { request_id, .. } => {
            validate_uuid_v7("request_id", request_id)?;
            (
                "subscription_interaction",
                matches!(message, ProtocolMessage::InteractionRequest(_)),
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

fn parse_session_cursors(
    cursors: Vec<SessionCursorDto>,
) -> Result<BTreeMap<SessionId, u64>, NativeProtocolError> {
    let mut parsed = BTreeMap::new();
    for cursor in cursors {
        let session_id = parse_session_id(cursor.session_id)?;
        let last_sequence =
            cursor
                .last_sequence
                .parse::<u64>()
                .map_err(|_| NativeProtocolError::InvalidField {
                    field: "last_sequence",
                    reason: "expected a canonical positive decimal u64".to_owned(),
                })?;
        if last_sequence == 0 || cursor.last_sequence != last_sequence.to_string() {
            return invalid("last_sequence", "expected a canonical positive decimal u64");
        }
        if parsed.insert(session_id, last_sequence).is_some() {
            return invalid("session_cursors", "must not contain duplicate Session IDs");
        }
    }
    Ok(parsed)
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
        ProtocolMessage::InteractionRequest(_) => "interaction_request",
        ProtocolMessage::InteractionResponse(_) => "interaction_response",
        ProtocolMessage::AgentCommand(_) => "agent_command",
        _ => "future_domain_message",
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, str::FromStr};

    use agentpulse_core::{
        AgentSession, ApprovalOptionId, ApprovalSelection, ChannelCapabilities, ChannelDescriptor,
        ChannelId, ChannelKind, InteractionId, InteractionResponse, InteractionResponsePayload,
        NonEmptyText, ProviderCapabilities, ProviderDescriptor, ProviderId, ProviderKind,
        SessionId, Timestamp,
    };

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    const CLIENT_ID: &str = "019976a4-00f4-7db3-996a-9ee0158f7499";
    const REQUEST_ID: &str = "019976a4-00f5-7876-a127-aa91d21546af";
    const CHANNEL_ID: &str = "019976a4-00f6-7d6d-b866-90ddbec53537";
    const PROVIDER_ID: &str = "019976a4-00f7-7ab4-b04d-41a21b57c2ab";
    const SESSION_ID: &str = "019976a4-00f0-7312-b36c-d01f9c5c06f6";
    const INTERACTION_ID: &str = "019976a4-00f8-7025-9f29-55c05d8d9120";
    const OPTION_ID: &str = "019976a4-00f9-7211-a5d4-e68ac2f32176";

    fn channel_descriptor() -> Result<ChannelDescriptor, Box<dyn Error>> {
        Ok(ChannelDescriptor::new(
            ChannelId::from_str(CHANNEL_ID)?,
            ChannelKind::new("native")?,
            NonEmptyText::new("Native Local")?,
            ChannelCapabilities::NOTIFICATION
                | ChannelCapabilities::SESSION_VIEW
                | ChannelCapabilities::REALTIME_SYNC
                | ChannelCapabilities::APPROVAL
                | ChannelCapabilities::FORM_INPUT
                | ChannelCapabilities::TEXT_INPUT
                | ChannelCapabilities::REMOTE_COMMAND,
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
                supported_protocol_versions: vec![V2_PROTOCOL_VERSION],
                host_run_id: None,
                session_cursors: BTreeMap::new(),
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
            NativeClientMessage::SubmitInteractionResponse {
                request_id: REQUEST_ID.to_owned(),
                response: InteractionResponse::new(
                    InteractionId::from_str(INTERACTION_ID)?,
                    SessionId::from_str(SESSION_ID)?,
                    ChannelId::from_str(CHANNEL_ID)?,
                    Timestamp::from_unix_timestamp_nanos(200)?,
                    InteractionResponsePayload::Approval(ApprovalSelection::new(
                        ApprovalOptionId::from_str(OPTION_ID)?,
                    )),
                ),
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
                protocol_version: V2_PROTOCOL_VERSION,
                max_frame_bytes: 1024 * 1024,
                ping_interval_seconds: 15,
                idle_timeout_seconds: 45,
                host_run_id: REQUEST_ID.to_owned(),
                resume_accepted: false,
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
                message: Box::new(ProtocolMessage::ProviderDescriptor(provider_descriptor()?)),
            },
            NativeServerMessage::Domain {
                context: NativeDeliveryContext::DiscoverySession {
                    request_id: REQUEST_ID.to_owned(),
                    last_sequence: EventSequence::FIRST,
                },
                message: Box::new(ProtocolMessage::AgentSession(session()?)),
            },
            NativeServerMessage::SubscriptionResult {
                request_id: REQUEST_ID.to_owned(),
                session_id: SessionId::from_str(SESSION_ID)?,
                status: NativeSubscriptionStatus::Subscribed,
                baseline_sequence: EventSequence::FIRST,
                pending_interaction_count: 0,
                event_count: 1,
                reset: true,
            },
            NativeServerMessage::InteractionResponseResult {
                request_id: REQUEST_ID.to_owned(),
                session_id: SessionId::from_str(SESSION_ID)?,
                interaction_id: INTERACTION_ID.to_owned(),
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
            "{{\"native_transport_version\":3,\"message\":{{\"type\":\"discover_sessions\",\"request_id\":\"{REQUEST_ID}\",\"extra\":true}}}}"
        );
        assert!(matches!(
            decode_client_message(unknown.as_bytes()),
            Err(NativeProtocolError::Json { .. })
        ));
        let wrong_version = format!(
            "{{\"native_transport_version\":1,\"message\":{{\"type\":\"discover_sessions\",\"request_id\":\"{REQUEST_ID}\"}}}}"
        );
        assert!(matches!(
            decode_client_message(wrong_version.as_bytes()),
            Err(NativeProtocolError::UnsupportedVersion { received: 1, .. })
        ));
        let invalid_context = NativeServerMessage::Domain {
            context: NativeDeliveryContext::LiveSession,
            message: Box::new(ProtocolMessage::ProviderDescriptor(provider_descriptor()?)),
        };
        assert!(matches!(
            encode_server_message(&invalid_context),
            Err(NativeProtocolError::InvalidDomainContext { .. })
        ));
        assert!(matches!(
            NativeEventRoute::from_core(ChannelEventRoute::Interaction(
                InteractionRoute::Interactive
            )),
            Ok(NativeEventRoute::InteractionInteractive)
        ));
        Ok(())
    }
}
