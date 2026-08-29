//! Runtime Host ownership, ingress, and Adapter lifecycle contracts.

use std::{
    error::Error,
    fmt,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard, Weak},
    thread::{self, ThreadId},
};

use agentpulse_core::{
    AgentCommand, AgentEvent, ChannelId, InteractionResponse, ProviderId, SessionAggregateConfig,
    SessionId,
};

use crate::{
    Bridge, ChannelActionError, ChannelActionSink, ChannelDiscoverySnapshot, ChannelPort,
    EndpointRegistrationError, ProviderEventError, ProviderEventReport, ProviderEventSink,
    ProviderPort, SubscribeOutcome, SubscriptionError, UnsubscribeOutcome,
    registry::BoxAdapterError,
};

/// A Provider-side execution source that publishes normalized Events.
///
/// The Host supplies a fresh, Provider-bound handle for every start cycle.
/// Implementations may retain and clone that handle for their own workers.
/// `stop` must be safe after either a successful or failed `start` because a
/// failed start may still have acquired resources that require cleanup.
pub trait ProviderEventSource: Send {
    /// The Adapter-specific lifecycle error.
    type Error: Error + Send + Sync + 'static;

    /// Starts the Source with the current generation's controlled ingress.
    fn start(&mut self, events: ProviderEventHandle) -> Result<(), Self::Error>;

    /// Stops the Source and releases its execution resources.
    fn stop(&mut self) -> Result<(), Self::Error>;
}

/// A Channel-side execution source that submits normalized user Actions.
///
/// The Host supplies a fresh, Channel-bound handle for every start cycle.
/// Implementations may retain and clone that handle for their own workers.
/// `stop` must be safe after either a successful or failed `start` because a
/// failed start may still have acquired resources that require cleanup.
pub trait ChannelActionSource: Send {
    /// The Adapter-specific lifecycle error.
    type Error: Error + Send + Sync + 'static;

    /// Starts the Source with the current generation's controlled ingress.
    fn start(&mut self, actions: ChannelActionHandle) -> Result<(), Self::Error>;

    /// Stops the Source and releases its execution resources.
    fn stop(&mut self) -> Result<(), Self::Error>;

    /// Chooses whether Host stop retains or clears this Channel's subscriptions.
    fn subscription_scope(&self) -> ChannelSubscriptionScope {
        ChannelSubscriptionScope::Persistent
    }
}

/// Controls how a Channel's subscriptions cross RuntimeHost generations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ChannelSubscriptionScope {
    /// Preserve subscriptions across explicit stop/start cycles.
    #[default]
    Persistent,
    /// Clear subscriptions whenever the Channel Source generation stops.
    SourceGeneration,
}

/// Identifies one Host-owned runtime endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeEndpointId {
    /// A Provider endpoint.
    Provider(ProviderId),
    /// A Channel endpoint.
    Channel(ChannelId),
}

impl RuntimeEndpointId {
    /// Returns the Provider ID when this identifies a Provider.
    #[must_use]
    pub const fn provider_id(self) -> Option<ProviderId> {
        match self {
            Self::Provider(provider_id) => Some(provider_id),
            Self::Channel(_) => None,
        }
    }

    /// Returns the Channel ID when this identifies a Channel.
    #[must_use]
    pub const fn channel_id(self) -> Option<ChannelId> {
        match self {
            Self::Provider(_) => None,
            Self::Channel(channel_id) => Some(channel_id),
        }
    }
}

impl fmt::Display for RuntimeEndpointId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(provider_id) => write!(formatter, "Provider {provider_id}"),
            Self::Channel(channel_id) => write!(formatter, "Channel {channel_id}"),
        }
    }
}

/// The externally visible lifecycle state of the Runtime Host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeHostState {
    /// No Adapter Source is running and registration is allowed.
    Stopped,
    /// A start cycle has completed, possibly with isolated Adapter failures.
    Started,
    /// At least one Adapter failed to stop and must be retried.
    StopFailed,
}

impl fmt::Display for RuntimeHostState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("stopped"),
            Self::Started => formatter.write_str("started"),
            Self::StopFailed => formatter.write_str("stop failed"),
        }
    }
}

/// The current lifecycle state of one Adapter Source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AdapterLifecycleState {
    /// The Source is stopped.
    Stopped,
    /// The Source started successfully and its current ingress is active.
    Running,
    /// The Source failed to start and its ingress is inactive.
    StartFailed,
    /// The Source failed to stop and its ingress remains inactive.
    StopFailed,
}

/// Identifies the lifecycle callback that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AdapterLifecyclePhase {
    /// The Source `start` callback.
    Start,
    /// The Source `stop` callback.
    Stop,
}

impl fmt::Display for AdapterLifecyclePhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => formatter.write_str("start"),
            Self::Stop => formatter.write_str("stop"),
        }
    }
}

/// A lifecycle callback failure for one Adapter Source.
#[derive(Debug)]
pub struct AdapterLifecycleError {
    endpoint: RuntimeEndpointId,
    phase: AdapterLifecyclePhase,
    source: BoxAdapterError,
}

impl AdapterLifecycleError {
    fn new(
        endpoint: RuntimeEndpointId,
        phase: AdapterLifecyclePhase,
        source: BoxAdapterError,
    ) -> Self {
        Self {
            endpoint,
            phase,
            source,
        }
    }

    /// Returns the Adapter endpoint whose callback failed.
    #[must_use]
    pub const fn endpoint(&self) -> RuntimeEndpointId {
        self.endpoint
    }

    /// Returns the lifecycle callback that failed.
    #[must_use]
    pub const fn phase(&self) -> AdapterLifecyclePhase {
        self.phase
    }
}

impl fmt::Display for AdapterLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} callback failed: {}",
            self.endpoint, self.phase, self.source
        )
    }
}

impl Error for AdapterLifecycleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// The result of one Adapter during a Host lifecycle operation.
#[derive(Debug)]
#[non_exhaustive]
pub enum AdapterLifecycleResult {
    /// The requested callback completed successfully.
    Succeeded {
        /// The Adapter endpoint.
        endpoint: RuntimeEndpointId,
    },
    /// No callback was required because this Adapter was already stopped.
    Skipped {
        /// The Adapter endpoint.
        endpoint: RuntimeEndpointId,
        /// The state that made the callback unnecessary.
        state: AdapterLifecycleState,
    },
    /// The requested callback failed.
    Failed(AdapterLifecycleError),
}

impl AdapterLifecycleResult {
    /// Returns the Adapter endpoint represented by this result.
    #[must_use]
    pub const fn endpoint(&self) -> RuntimeEndpointId {
        match self {
            Self::Succeeded { endpoint } | Self::Skipped { endpoint, .. } => *endpoint,
            Self::Failed(error) => error.endpoint(),
        }
    }

    /// Returns whether this result contains a lifecycle failure.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        !matches!(self, Self::Failed(_))
    }

    /// Borrows the lifecycle failure, when present.
    #[must_use]
    pub const fn error(&self) -> Option<&AdapterLifecycleError> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Succeeded { .. } | Self::Skipped { .. } => None,
        }
    }
}

/// The Host-level result of a lifecycle operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeLifecycleOutcome {
    /// A start cycle was attempted.
    Started,
    /// The Host was already started and no callbacks ran.
    AlreadyStarted,
    /// Every required stop callback completed.
    Stopped,
    /// The Host was already stopped and no callbacks ran.
    AlreadyStopped,
    /// At least one required stop callback failed.
    StopFailed,
}

/// Identifies a Host lifecycle operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeLifecycleOperation {
    /// Starting Adapter Sources.
    Start,
    /// Stopping Adapter Sources.
    Stop,
}

/// The complete ordered report for one Host lifecycle operation.
#[derive(Debug)]
pub struct RuntimeLifecycleReport {
    operation: RuntimeLifecycleOperation,
    outcome: RuntimeLifecycleOutcome,
    adapters: Vec<AdapterLifecycleResult>,
}

impl RuntimeLifecycleReport {
    fn new(
        operation: RuntimeLifecycleOperation,
        outcome: RuntimeLifecycleOutcome,
        adapters: Vec<AdapterLifecycleResult>,
    ) -> Self {
        Self {
            operation,
            outcome,
            adapters,
        }
    }

    /// Returns the requested Host lifecycle operation.
    #[must_use]
    pub const fn operation(&self) -> RuntimeLifecycleOperation {
        self.operation
    }

    /// Returns the Host-level operation outcome.
    #[must_use]
    pub const fn outcome(&self) -> RuntimeLifecycleOutcome {
        self.outcome
    }

    /// Borrows per-Adapter results in callback order.
    #[must_use]
    pub fn adapters(&self) -> &[AdapterLifecycleResult] {
        &self.adapters
    }

    /// Returns whether at least one Adapter callback failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.adapters.iter().any(|result| !result.is_success())
    }

    fn first_failure(&self) -> Option<&AdapterLifecycleError> {
        self.adapters.iter().find_map(AdapterLifecycleResult::error)
    }
}

/// An error raised by a Host lifecycle operation.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeLifecycleError {
    /// A prior stop failure must be cleaned up before another start.
    CleanupRequired,
    /// Subscription cleanup could not access the Bridge safely.
    BridgeAccess(RuntimeAccessError),
    /// One or more Adapter callbacks failed; the report includes every attempt.
    AdapterFailures {
        /// The complete ordered lifecycle report.
        report: RuntimeLifecycleReport,
    },
}

impl RuntimeLifecycleError {
    /// Borrows the complete report for Adapter callback failures.
    #[must_use]
    pub const fn report(&self) -> Option<&RuntimeLifecycleReport> {
        match self {
            Self::AdapterFailures { report } => Some(report),
            Self::CleanupRequired | Self::BridgeAccess(_) => None,
        }
    }
}

impl fmt::Display for RuntimeLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CleanupRequired => {
                formatter.write_str("failed Adapter cleanup must complete before restart")
            }
            Self::AdapterFailures { report } => write!(
                formatter,
                "{} of {} Adapter lifecycle callbacks failed",
                report
                    .adapters()
                    .iter()
                    .filter(|result| !result.is_success())
                    .count(),
                report.adapters().len()
            ),
            Self::BridgeAccess(source) => source.fmt(formatter),
        }
    }
}

impl Error for RuntimeLifecycleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::AdapterFailures { report } => report
                .first_failure()
                .map(|source| source as &(dyn Error + 'static)),
            Self::CleanupRequired => None,
            Self::BridgeAccess(source) => Some(source),
        }
    }
}

/// An error raised while registering a paired Port and Source with a Host.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeRegistrationError {
    /// Runtime registration was attempted while the Host was not stopped.
    HostNotStopped {
        /// The current Host state.
        state: RuntimeHostState,
    },
    /// The Bridge rejected the endpoint identity.
    Endpoint(EndpointRegistrationError),
    /// Registration attempted to re-enter an active Bridge operation.
    BridgeAccess(RuntimeAccessError),
}

impl fmt::Display for RuntimeRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostNotStopped { state } => {
                write!(
                    formatter,
                    "cannot register an Adapter while the Host is {state}"
                )
            }
            Self::Endpoint(source) => source.fmt(formatter),
            Self::BridgeAccess(source) => source.fmt(formatter),
        }
    }
}

impl Error for RuntimeRegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Endpoint(source) => Some(source),
            Self::BridgeAccess(source) => Some(source),
            Self::HostNotStopped { .. } => None,
        }
    }
}

impl From<EndpointRegistrationError> for RuntimeRegistrationError {
    fn from(source: EndpointRegistrationError) -> Self {
        Self::Endpoint(source)
    }
}

impl From<RuntimeAccessError> for RuntimeRegistrationError {
    fn from(source: RuntimeAccessError) -> Self {
        Self::BridgeAccess(source)
    }
}

/// An error raised when a Bridge operation synchronously re-enters the Host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeAccessError {
    /// The current thread already owns this Host's Bridge operation.
    ReentrantBridgeAccess,
}

impl fmt::Display for RuntimeAccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReentrantBridgeAccess => {
                formatter.write_str("synchronous re-entry into the Runtime Host is not allowed")
            }
        }
    }
}

impl Error for RuntimeAccessError {}

/// An error raised by a Host-owned subscription operation.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeSubscriptionError {
    /// The Host rejected synchronous Bridge re-entry.
    BridgeAccess(RuntimeAccessError),
    /// The Bridge rejected the subscription change.
    Subscription(SubscriptionError),
}

impl fmt::Display for RuntimeSubscriptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BridgeAccess(source) => source.fmt(formatter),
            Self::Subscription(source) => source.fmt(formatter),
        }
    }
}

impl Error for RuntimeSubscriptionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BridgeAccess(source) => Some(source),
            Self::Subscription(source) => Some(source),
        }
    }
}

impl From<RuntimeAccessError> for RuntimeSubscriptionError {
    fn from(source: RuntimeAccessError) -> Self {
        Self::BridgeAccess(source)
    }
}

impl From<SubscriptionError> for RuntimeSubscriptionError {
    fn from(source: SubscriptionError) -> Self {
        Self::Subscription(source)
    }
}

/// An error raised while publishing through a controlled Provider ingress.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProviderEventIngressError {
    /// The caller supplied a Provider ID different from the handle binding.
    ProviderMismatch {
        /// The Provider bound to this handle.
        expected: ProviderId,
        /// The Provider supplied by the caller.
        actual: ProviderId,
    },
    /// The handle's start generation is not active.
    Inactive {
        /// The Provider bound to this handle.
        provider_id: ProviderId,
    },
    /// The owning Runtime Host has been released.
    HostDropped {
        /// The Provider bound to this handle.
        provider_id: ProviderId,
    },
    /// The ingress attempted synchronous Bridge re-entry.
    BridgeAccess(RuntimeAccessError),
    /// The Bridge rejected the normalized Provider Event.
    Bridge(ProviderEventError),
}

impl fmt::Display for ProviderEventIngressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderMismatch { expected, actual } => write!(
                formatter,
                "Provider Event handle is bound to {expected}, not {actual}"
            ),
            Self::Inactive { provider_id } => {
                write!(formatter, "Provider {provider_id} ingress is inactive")
            }
            Self::HostDropped { provider_id } => {
                write!(
                    formatter,
                    "Provider {provider_id} Runtime Host has been released"
                )
            }
            Self::BridgeAccess(source) => source.fmt(formatter),
            Self::Bridge(source) => source.fmt(formatter),
        }
    }
}

impl Error for ProviderEventIngressError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BridgeAccess(source) => Some(source),
            Self::Bridge(source) => Some(source),
            Self::ProviderMismatch { .. } | Self::Inactive { .. } | Self::HostDropped { .. } => {
                None
            }
        }
    }
}

/// An error raised while submitting through a controlled Channel ingress.
#[derive(Debug)]
#[non_exhaustive]
pub enum ChannelActionIngressError {
    /// The caller supplied a Channel ID different from the handle binding.
    ChannelMismatch {
        /// The Channel bound to this handle.
        expected: ChannelId,
        /// The Channel supplied by the caller.
        actual: ChannelId,
    },
    /// The handle's start generation is not active.
    Inactive {
        /// The Channel bound to this handle.
        channel_id: ChannelId,
    },
    /// The owning Runtime Host has been released.
    HostDropped {
        /// The Channel bound to this handle.
        channel_id: ChannelId,
    },
    /// The ingress attempted synchronous Bridge re-entry.
    BridgeAccess(RuntimeAccessError),
    /// The Bridge rejected the normalized Channel Action.
    Bridge(ChannelActionError),
    /// The Bridge rejected a Channel-owned subscription operation.
    Subscription(SubscriptionError),
}

impl fmt::Display for ChannelActionIngressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelMismatch { expected, actual } => write!(
                formatter,
                "Channel Action handle is bound to {expected}, not {actual}"
            ),
            Self::Inactive { channel_id } => {
                write!(formatter, "Channel {channel_id} ingress is inactive")
            }
            Self::HostDropped { channel_id } => {
                write!(
                    formatter,
                    "Channel {channel_id} Runtime Host has been released"
                )
            }
            Self::BridgeAccess(source) => source.fmt(formatter),
            Self::Bridge(source) => source.fmt(formatter),
            Self::Subscription(source) => source.fmt(formatter),
        }
    }
}

impl Error for ChannelActionIngressError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BridgeAccess(source) => Some(source),
            Self::Bridge(source) => Some(source),
            Self::Subscription(source) => Some(source),
            Self::ChannelMismatch { .. } | Self::Inactive { .. } | Self::HostDropped { .. } => None,
        }
    }
}

/// A cloneable, generation-scoped Provider Event ingress handle.
///
/// The handle is permanently revoked when its start cycle fails or stops. A
/// later Host restart supplies a new handle and never reactivates old clones.
#[derive(Clone)]
pub struct ProviderEventHandle {
    provider_id: ProviderId,
    bridge: Weak<SharedBridge>,
    permit: Arc<IngressPermit>,
}

impl ProviderEventHandle {
    fn new(
        provider_id: ProviderId,
        bridge: Weak<SharedBridge>,
        permit: Arc<IngressPermit>,
    ) -> Self {
        Self {
            provider_id,
            bridge,
            permit,
        }
    }

    /// Returns the Provider identity bound to this handle.
    #[must_use]
    pub const fn provider_id(&self) -> ProviderId {
        self.provider_id
    }

    /// Publishes one normalized Event synchronously through the Bridge.
    pub fn publish_event(
        &self,
        event: AgentEvent,
    ) -> Result<ProviderEventReport, ProviderEventIngressError> {
        let bridge = self
            .bridge
            .upgrade()
            .ok_or(ProviderEventIngressError::HostDropped {
                provider_id: self.provider_id,
            })?;
        let _permit = self
            .permit
            .enter()
            .ok_or(ProviderEventIngressError::Inactive {
                provider_id: self.provider_id,
            })?;
        let mut bridge = bridge
            .access()
            .map_err(ProviderEventIngressError::BridgeAccess)?;
        bridge
            .handle_provider_event(self.provider_id, event)
            .map_err(ProviderEventIngressError::Bridge)
    }
}

impl fmt::Debug for ProviderEventHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderEventHandle")
            .field("provider_id", &self.provider_id)
            .finish_non_exhaustive()
    }
}

impl ProviderEventSink for ProviderEventHandle {
    type Error = ProviderEventIngressError;

    fn publish_event(
        &mut self,
        provider_id: ProviderId,
        event: AgentEvent,
    ) -> Result<(), Self::Error> {
        if provider_id != self.provider_id {
            return Err(ProviderEventIngressError::ProviderMismatch {
                expected: self.provider_id,
                actual: provider_id,
            });
        }
        let _ = ProviderEventHandle::publish_event(self, event)?;
        Ok(())
    }
}

/// A cloneable, generation-scoped Channel Action ingress handle.
///
/// The handle is permanently revoked when its start cycle fails or stops. A
/// later Host restart supplies a new handle and never reactivates old clones.
#[derive(Clone)]
pub struct ChannelActionHandle {
    channel_id: ChannelId,
    bridge: Weak<SharedBridge>,
    permit: Arc<IngressPermit>,
}

impl ChannelActionHandle {
    fn new(channel_id: ChannelId, bridge: Weak<SharedBridge>, permit: Arc<IngressPermit>) -> Self {
        Self {
            channel_id,
            bridge,
            permit,
        }
    }

    /// Returns the Channel identity bound to this handle.
    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// Captures the registered Providers and current Sessions for discovery.
    pub fn discovery_snapshot(
        &self,
    ) -> Result<ChannelDiscoverySnapshot, ChannelActionIngressError> {
        self.with_bridge_access(|bridge| Ok(bridge.discovery_snapshot()))
    }

    /// Subscribes this Channel to one current Session.
    pub fn subscribe(
        &self,
        session_id: SessionId,
    ) -> Result<SubscribeOutcome, ChannelActionIngressError> {
        self.with_bridge_access(|bridge| {
            bridge
                .subscribe(self.channel_id, session_id)
                .map_err(ChannelActionIngressError::Subscription)
        })
    }

    /// Cancels this Channel's subscription to one current Session.
    pub fn unsubscribe(
        &self,
        session_id: SessionId,
    ) -> Result<UnsubscribeOutcome, ChannelActionIngressError> {
        self.with_bridge_access(|bridge| {
            bridge
                .unsubscribe(self.channel_id, session_id)
                .map_err(ChannelActionIngressError::Subscription)
        })
    }

    /// Submits one normalized Interaction Response synchronously.
    pub fn submit_interaction_response(
        &self,
        response: InteractionResponse,
    ) -> Result<(), ChannelActionIngressError> {
        self.with_bridge(|bridge| bridge.handle_interaction_response(self.channel_id, response))
    }

    /// Submits one normalized Agent Command synchronously.
    pub fn submit_command(&self, command: AgentCommand) -> Result<(), ChannelActionIngressError> {
        self.with_bridge(|bridge| bridge.handle_command(self.channel_id, command))
    }

    fn with_bridge(
        &self,
        operation: impl FnOnce(&mut Bridge) -> Result<(), ChannelActionError>,
    ) -> Result<(), ChannelActionIngressError> {
        self.with_bridge_access(|bridge| {
            operation(bridge).map_err(ChannelActionIngressError::Bridge)
        })
    }

    fn with_bridge_access<R>(
        &self,
        operation: impl FnOnce(&mut Bridge) -> Result<R, ChannelActionIngressError>,
    ) -> Result<R, ChannelActionIngressError> {
        let bridge = self
            .bridge
            .upgrade()
            .ok_or(ChannelActionIngressError::HostDropped {
                channel_id: self.channel_id,
            })?;
        let _permit = self
            .permit
            .enter()
            .ok_or(ChannelActionIngressError::Inactive {
                channel_id: self.channel_id,
            })?;
        let mut bridge = bridge
            .access()
            .map_err(ChannelActionIngressError::BridgeAccess)?;
        operation(&mut bridge)
    }
}

impl fmt::Debug for ChannelActionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelActionHandle")
            .field("channel_id", &self.channel_id)
            .finish_non_exhaustive()
    }
}

impl ChannelActionSink for ChannelActionHandle {
    type Error = ChannelActionIngressError;

    fn submit_interaction_response(
        &mut self,
        channel_id: ChannelId,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        if channel_id != self.channel_id {
            return Err(ChannelActionIngressError::ChannelMismatch {
                expected: self.channel_id,
                actual: channel_id,
            });
        }
        ChannelActionHandle::submit_interaction_response(self, response)
    }

    fn submit_command(
        &mut self,
        channel_id: ChannelId,
        command: AgentCommand,
    ) -> Result<(), Self::Error> {
        if channel_id != self.channel_id {
            return Err(ChannelActionIngressError::ChannelMismatch {
                expected: self.channel_id,
                actual: channel_id,
            });
        }
        ChannelActionHandle::submit_command(self, command)
    }
}

/// A synchronous owner for a Bridge and paired Adapter Port/Source instances.
///
/// The Host keeps the Bridge alive across stop/start cycles, serializes ingress
/// from independent Sources, and never lets Source-held handles own the Bridge.
pub struct RuntimeHost {
    state: RuntimeHostState,
    adapters: Vec<RegisteredAdapter>,
    bridge: Arc<SharedBridge>,
}

impl RuntimeHost {
    /// Creates an empty Runtime Host using default Aggregate settings.
    #[must_use]
    pub fn new() -> Self {
        Self::with_session_config(SessionAggregateConfig::default())
    }

    /// Creates an empty Runtime Host using explicit Aggregate settings.
    #[must_use]
    pub fn with_session_config(session_config: SessionAggregateConfig) -> Self {
        Self {
            state: RuntimeHostState::Stopped,
            adapters: Vec::new(),
            bridge: Arc::new(SharedBridge::new(Bridge::with_session_config(
                session_config,
            ))),
        }
    }

    /// Registers one paired Provider Port and Event Source.
    pub fn register_provider<P, S>(
        &mut self,
        provider: P,
        source: S,
    ) -> Result<(), RuntimeRegistrationError>
    where
        P: ProviderPort + 'static,
        S: ProviderEventSource + 'static,
    {
        self.ensure_registration_allowed()?;
        let provider_id = provider.descriptor().id();
        self.bridge.access()?.register_provider(provider)?;
        self.adapters
            .push(RegisteredAdapter::provider(provider_id, source));
        Ok(())
    }

    /// Registers one paired Channel Port and Action Source.
    pub fn register_channel<C, S>(
        &mut self,
        channel: C,
        source: S,
    ) -> Result<(), RuntimeRegistrationError>
    where
        C: ChannelPort + 'static,
        S: ChannelActionSource + 'static,
    {
        self.ensure_registration_allowed()?;
        let channel_id = channel.descriptor().id();
        self.bridge.access()?.register_channel(channel)?;
        self.adapters
            .push(RegisteredAdapter::channel(channel_id, source));
        Ok(())
    }

    /// Returns the current Host lifecycle state.
    #[must_use]
    pub const fn state(&self) -> RuntimeHostState {
        self.state
    }

    /// Returns one registered Adapter's current lifecycle state.
    #[must_use]
    pub fn adapter_state(&self, endpoint: RuntimeEndpointId) -> Option<AdapterLifecycleState> {
        self.adapters
            .iter()
            .find(|adapter| adapter.endpoint() == endpoint)
            .map(RegisteredAdapter::state)
    }

    /// Iterates over Adapter identities and states in registration order.
    pub fn adapter_states(
        &self,
    ) -> impl ExactSizeIterator<Item = (RuntimeEndpointId, AdapterLifecycleState)> + '_ {
        self.adapters
            .iter()
            .map(|adapter| (adapter.endpoint(), adapter.state()))
    }

    /// Runs a read-only inspection while the Host serializes Bridge access.
    pub fn inspect_bridge<R>(
        &self,
        inspect: impl FnOnce(&Bridge) -> R,
    ) -> Result<R, RuntimeAccessError> {
        let bridge = self.bridge.access()?;
        Ok(inspect(&bridge))
    }

    /// Subscribes a registered Channel to an existing Session.
    pub fn subscribe(
        &self,
        channel_id: ChannelId,
        session_id: SessionId,
    ) -> Result<SubscribeOutcome, RuntimeSubscriptionError> {
        self.bridge
            .access()?
            .subscribe(channel_id, session_id)
            .map_err(RuntimeSubscriptionError::Subscription)
    }

    /// Cancels a registered Channel's subscription to an existing Session.
    pub fn unsubscribe(
        &self,
        channel_id: ChannelId,
        session_id: SessionId,
    ) -> Result<UnsubscribeOutcome, RuntimeSubscriptionError> {
        self.bridge
            .access()?
            .unsubscribe(channel_id, session_id)
            .map_err(RuntimeSubscriptionError::Subscription)
    }

    /// Starts every Adapter Source in registration order.
    ///
    /// Failures are isolated and returned with the complete report. Any Event
    /// accepted before a Source reports a start failure remains reduced in the
    /// Bridge, while that failed Source's handle is immediately revoked.
    pub fn start(&mut self) -> Result<RuntimeLifecycleReport, RuntimeLifecycleError> {
        match self.state {
            RuntimeHostState::Started => {
                return Ok(RuntimeLifecycleReport::new(
                    RuntimeLifecycleOperation::Start,
                    RuntimeLifecycleOutcome::AlreadyStarted,
                    Vec::new(),
                ));
            }
            RuntimeHostState::StopFailed => {
                return Err(RuntimeLifecycleError::CleanupRequired);
            }
            RuntimeHostState::Stopped => {}
        }

        let mut results = Vec::with_capacity(self.adapters.len());
        for adapter in &mut self.adapters {
            results.push(match adapter.start(&self.bridge) {
                Ok(()) => AdapterLifecycleResult::Succeeded {
                    endpoint: adapter.endpoint(),
                },
                Err(error) => AdapterLifecycleResult::Failed(error),
            });
        }
        self.state = RuntimeHostState::Started;

        let report = RuntimeLifecycleReport::new(
            RuntimeLifecycleOperation::Start,
            RuntimeLifecycleOutcome::Started,
            results,
        );
        if report.has_failures() {
            Err(RuntimeLifecycleError::AdapterFailures { report })
        } else {
            Ok(report)
        }
    }

    /// Revokes every ingress and stops Adapter Sources in reverse order.
    ///
    /// A partial failure leaves the Host in `StopFailed`. Calling `stop` again
    /// retries only the Sources whose previous stop did not complete.
    pub fn stop(&mut self) -> Result<RuntimeLifecycleReport, RuntimeLifecycleError> {
        if self.state == RuntimeHostState::Stopped {
            return Ok(RuntimeLifecycleReport::new(
                RuntimeLifecycleOperation::Stop,
                RuntimeLifecycleOutcome::AlreadyStopped,
                Vec::new(),
            ));
        }

        self.revoke_all_ingress();
        self.clear_source_generation_subscriptions()
            .map_err(RuntimeLifecycleError::BridgeAccess)?;
        let mut results = Vec::with_capacity(self.adapters.len());
        for adapter in self.adapters.iter_mut().rev() {
            let endpoint = adapter.endpoint();
            if adapter.state() == AdapterLifecycleState::Stopped {
                results.push(AdapterLifecycleResult::Skipped {
                    endpoint,
                    state: AdapterLifecycleState::Stopped,
                });
                continue;
            }
            results.push(match adapter.stop() {
                Ok(()) => AdapterLifecycleResult::Succeeded { endpoint },
                Err(error) => AdapterLifecycleResult::Failed(error),
            });
        }

        let has_failures = results.iter().any(|result| !result.is_success());
        self.state = if has_failures {
            RuntimeHostState::StopFailed
        } else {
            RuntimeHostState::Stopped
        };
        let outcome = if has_failures {
            RuntimeLifecycleOutcome::StopFailed
        } else {
            RuntimeLifecycleOutcome::Stopped
        };
        let report = RuntimeLifecycleReport::new(RuntimeLifecycleOperation::Stop, outcome, results);
        if has_failures {
            Err(RuntimeLifecycleError::AdapterFailures { report })
        } else {
            Ok(report)
        }
    }

    fn ensure_registration_allowed(&self) -> Result<(), RuntimeRegistrationError> {
        if self.state == RuntimeHostState::Stopped {
            Ok(())
        } else {
            Err(RuntimeRegistrationError::HostNotStopped { state: self.state })
        }
    }

    fn revoke_all_ingress(&self) {
        for adapter in &self.adapters {
            adapter.revoke_ingress();
        }
    }

    fn clear_source_generation_subscriptions(&self) -> Result<(), RuntimeAccessError> {
        let channel_ids = self
            .adapters
            .iter()
            .filter_map(RegisteredAdapter::source_generation_channel)
            .collect::<Vec<_>>();
        if channel_ids.is_empty() {
            return Ok(());
        }

        let mut bridge = self.bridge.access()?;
        for channel_id in channel_ids {
            let _ = bridge.unsubscribe_channel(channel_id);
        }
        Ok(())
    }
}

impl Default for RuntimeHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RuntimeHost {
    fn drop(&mut self) {
        self.revoke_all_ingress();
        let _ = self.clear_source_generation_subscriptions();
        for adapter in self.adapters.iter_mut().rev() {
            if adapter.state() != AdapterLifecycleState::Stopped {
                let _ = adapter.stop();
            }
        }
        while self.adapters.pop().is_some() {}
    }
}

struct SharedBridge {
    bridge: Mutex<Bridge>,
    owner: Mutex<Option<ThreadId>>,
}

impl SharedBridge {
    fn new(bridge: Bridge) -> Self {
        Self {
            bridge: Mutex::new(bridge),
            owner: Mutex::new(None),
        }
    }

    fn access(&self) -> Result<BridgeAccess<'_>, RuntimeAccessError> {
        let current = thread::current().id();
        {
            let owner = lock_recover(&self.owner);
            if owner.as_ref() == Some(&current) {
                return Err(RuntimeAccessError::ReentrantBridgeAccess);
            }
        }

        let bridge = lock_recover(&self.bridge);
        *lock_recover(&self.owner) = Some(current);
        Ok(BridgeAccess {
            shared: self,
            bridge,
        })
    }
}

struct BridgeAccess<'a> {
    shared: &'a SharedBridge,
    bridge: MutexGuard<'a, Bridge>,
}

impl Deref for BridgeAccess<'_> {
    type Target = Bridge;

    fn deref(&self) -> &Self::Target {
        &self.bridge
    }
}

impl DerefMut for BridgeAccess<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bridge
    }
}

impl Drop for BridgeAccess<'_> {
    fn drop(&mut self) {
        *lock_recover(&self.shared.owner) = None;
    }
}

struct IngressPermit {
    active: Mutex<bool>,
}

impl IngressPermit {
    fn active() -> Self {
        Self {
            active: Mutex::new(true),
        }
    }

    fn enter(&self) -> Option<MutexGuard<'_, bool>> {
        let active = lock_recover(&self.active);
        if *active { Some(active) } else { None }
    }

    fn revoke(&self) {
        *lock_recover(&self.active) = false;
    }
}

struct PermitRevoker(Option<Arc<IngressPermit>>);

impl PermitRevoker {
    fn new(permit: Arc<IngressPermit>) -> Self {
        Self(Some(permit))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for PermitRevoker {
    fn drop(&mut self) {
        if let Some(permit) = self.0.as_ref() {
            permit.revoke();
        }
    }
}

trait ErasedProviderEventSource: Send {
    fn start(&mut self, events: ProviderEventHandle) -> Result<(), BoxAdapterError>;
    fn stop(&mut self) -> Result<(), BoxAdapterError>;
}

impl<S> ErasedProviderEventSource for S
where
    S: ProviderEventSource,
{
    fn start(&mut self, events: ProviderEventHandle) -> Result<(), BoxAdapterError> {
        ProviderEventSource::start(self, events)
            .map_err(|source| Box::new(source) as BoxAdapterError)
    }

    fn stop(&mut self) -> Result<(), BoxAdapterError> {
        ProviderEventSource::stop(self).map_err(|source| Box::new(source) as BoxAdapterError)
    }
}

trait ErasedChannelActionSource: Send {
    fn start(&mut self, actions: ChannelActionHandle) -> Result<(), BoxAdapterError>;
    fn stop(&mut self) -> Result<(), BoxAdapterError>;
}

impl<S> ErasedChannelActionSource for S
where
    S: ChannelActionSource,
{
    fn start(&mut self, actions: ChannelActionHandle) -> Result<(), BoxAdapterError> {
        ChannelActionSource::start(self, actions)
            .map_err(|source| Box::new(source) as BoxAdapterError)
    }

    fn stop(&mut self) -> Result<(), BoxAdapterError> {
        ChannelActionSource::stop(self).map_err(|source| Box::new(source) as BoxAdapterError)
    }
}

enum RegisteredSource {
    Provider {
        provider_id: ProviderId,
        source: Box<dyn ErasedProviderEventSource>,
    },
    Channel {
        channel_id: ChannelId,
        source: Box<dyn ErasedChannelActionSource>,
        subscription_scope: ChannelSubscriptionScope,
    },
}

impl RegisteredSource {
    fn endpoint(&self) -> RuntimeEndpointId {
        match self {
            Self::Provider { provider_id, .. } => RuntimeEndpointId::Provider(*provider_id),
            Self::Channel { channel_id, .. } => RuntimeEndpointId::Channel(*channel_id),
        }
    }

    fn start(
        &mut self,
        bridge: Weak<SharedBridge>,
        permit: Arc<IngressPermit>,
    ) -> Result<(), BoxAdapterError> {
        match self {
            Self::Provider {
                provider_id,
                source,
            } => source.start(ProviderEventHandle::new(*provider_id, bridge, permit)),
            Self::Channel {
                channel_id, source, ..
            } => source.start(ChannelActionHandle::new(*channel_id, bridge, permit)),
        }
    }

    fn stop(&mut self) -> Result<(), BoxAdapterError> {
        match self {
            Self::Provider { source, .. } => source.stop(),
            Self::Channel { source, .. } => source.stop(),
        }
    }

    fn source_generation_channel(&self) -> Option<ChannelId> {
        match self {
            Self::Channel {
                channel_id,
                subscription_scope: ChannelSubscriptionScope::SourceGeneration,
                ..
            } => Some(*channel_id),
            Self::Provider { .. }
            | Self::Channel {
                subscription_scope: ChannelSubscriptionScope::Persistent,
                ..
            } => None,
        }
    }
}

struct RegisteredAdapter {
    source: RegisteredSource,
    state: AdapterLifecycleState,
    permit: Option<Arc<IngressPermit>>,
}

impl RegisteredAdapter {
    fn provider<S>(provider_id: ProviderId, source: S) -> Self
    where
        S: ProviderEventSource + 'static,
    {
        Self {
            source: RegisteredSource::Provider {
                provider_id,
                source: Box::new(source),
            },
            state: AdapterLifecycleState::Stopped,
            permit: None,
        }
    }

    fn channel<S>(channel_id: ChannelId, source: S) -> Self
    where
        S: ChannelActionSource + 'static,
    {
        let subscription_scope = source.subscription_scope();
        Self {
            source: RegisteredSource::Channel {
                channel_id,
                source: Box::new(source),
                subscription_scope,
            },
            state: AdapterLifecycleState::Stopped,
            permit: None,
        }
    }

    fn endpoint(&self) -> RuntimeEndpointId {
        self.source.endpoint()
    }

    const fn state(&self) -> AdapterLifecycleState {
        self.state
    }

    fn start(&mut self, bridge: &Arc<SharedBridge>) -> Result<(), AdapterLifecycleError> {
        let endpoint = self.endpoint();
        let permit = Arc::new(IngressPermit::active());
        self.permit = Some(Arc::clone(&permit));
        self.state = AdapterLifecycleState::StartFailed;
        let mut revoker = PermitRevoker::new(Arc::clone(&permit));
        match self.source.start(Arc::downgrade(bridge), permit) {
            Ok(()) => {
                revoker.disarm();
                self.state = AdapterLifecycleState::Running;
                Ok(())
            }
            Err(source) => Err(AdapterLifecycleError::new(
                endpoint,
                AdapterLifecyclePhase::Start,
                source,
            )),
        }
    }

    fn stop(&mut self) -> Result<(), AdapterLifecycleError> {
        let endpoint = self.endpoint();
        self.revoke_ingress();
        self.state = AdapterLifecycleState::StopFailed;
        match self.source.stop() {
            Ok(()) => {
                self.state = AdapterLifecycleState::Stopped;
                Ok(())
            }
            Err(source) => Err(AdapterLifecycleError::new(
                endpoint,
                AdapterLifecyclePhase::Stop,
                source,
            )),
        }
    }

    fn revoke_ingress(&self) {
        if let Some(permit) = self.permit.as_ref() {
            permit.revoke();
        }
    }

    fn source_generation_channel(&self) -> Option<ChannelId> {
        self.source.source_generation_channel()
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
