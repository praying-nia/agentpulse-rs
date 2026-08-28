//! Provider- and channel-neutral domain types for AgentPulse.
//!
//! This crate owns semantic models and their local invariants. It deliberately
//! contains no wire-format, persistence, async-runtime, provider, or channel
//! concerns.
//!
//! # Example
//!
//! ```
//! use agentpulse_core::{AgentSession, AgentState, ProviderId, SessionId, Timestamp};
//!
//! let session = AgentSession::builder(
//!     SessionId::new(),
//!     ProviderId::new(),
//!     Timestamp::now_utc(),
//! )
//! .state(AgentState::Running)
//! .build()?;
//!
//! assert_eq!(session.state(), AgentState::Running);
//! # Ok::<(), agentpulse_core::DomainError>(())
//! ```

mod aggregate;
mod capability;
mod command;
mod endpoint;
mod error;
mod event;
mod id;
mod interaction;
mod plan;
mod progress;
mod routing;
mod session;
mod value;

pub use aggregate::{
    ActiveToolCall, ApplyOutcome, ReduceError, SessionAggregate, SessionAggregateConfig,
    SnapshotKind,
};
pub use capability::{ChannelCapabilities, ProviderCapabilities};
pub use command::{AgentCommand, AgentCommandPayload};
pub use endpoint::{ChannelDescriptor, ProviderDescriptor};
pub use error::DomainError;
pub use event::{
    AgentEvent, AgentEventPayload, AgentMessage, AgentMessageLevel, SessionOutcome, ToolActivity,
    ToolOutcome,
};
pub use id::{
    ChannelId, ChoiceOptionId, CommandId, EventId, InteractionId, PlanItemId, ProviderId,
    SessionId, ToolCallId,
};
pub use interaction::{
    ApprovalDecision, ApprovalRequest, ApprovalScope, ChoiceOption, ChoiceRequest, ChoiceSelection,
    InteractionRequest, InteractionRequestPayload, InteractionResponse, InteractionResponsePayload,
    TextInputRequest,
};
pub use plan::{PlanItem, PlanItemStatus, PlanSnapshot};
pub use progress::{DeterminateProgress, ProgressSnapshot, ProgressValue};
pub use routing::{CapabilityRouteError, CapabilityRouter, ChannelEventRoute, InteractionRoute};
pub use session::{AgentSession, AgentSessionBuilder, AgentState, ConnectionState};
pub use value::{
    ChannelKind, EventSequence, ExternalId, NonEmptyText, ProviderKind, Revision, Timestamp,
    WorkspaceRef,
};
