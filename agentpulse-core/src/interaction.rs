//! Correlated interaction requests and responses.

use std::collections::HashSet;

use crate::{
    ApprovalOptionId, ChannelCapabilities, ChannelId, ChoiceOptionId, DomainError, InteractionId,
    NonEmptyText, ProviderCapabilities, SessionId, Timestamp,
};

/// The user-visible effect of one Provider-issued approval option.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ApprovalDisposition {
    /// Allows the requested operation, possibly with the option's described policy effect.
    Approve,
    /// Rejects the requested operation while allowing the run to continue.
    Reject,
    /// Rejects the requested operation and cancels the active run.
    Cancel,
}

/// Distinguishes a new command from input written to an existing process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ApprovalCommandKind {
    /// A command that has not started yet.
    Command,
    /// Input sent to an already running process.
    WriteStdin,
}

/// Network target associated with a command approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalNetworkContext {
    host: NonEmptyText,
    protocol: NonEmptyText,
}

impl ApprovalNetworkContext {
    /// Creates a network approval context.
    #[must_use]
    pub const fn new(host: NonEmptyText, protocol: NonEmptyText) -> Self {
        Self { host, protocol }
    }

    /// Borrows the requested host.
    #[must_use]
    pub const fn host(&self) -> &NonEmptyText {
        &self.host
    }

    /// Borrows the requested network protocol.
    #[must_use]
    pub const fn protocol(&self) -> &NonEmptyText {
        &self.protocol
    }
}

/// The kind of one proposed file change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ApprovalFileChangeKind {
    /// Creates a file.
    Add,
    /// Deletes a file.
    Delete,
    /// Updates an existing file.
    Update,
}

/// One exact file change shown for approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalFileChange {
    path: NonEmptyText,
    kind: ApprovalFileChangeKind,
    diff: String,
}

impl ApprovalFileChange {
    /// Creates a proposed file change. An empty diff is preserved exactly.
    #[must_use]
    pub const fn new(path: NonEmptyText, kind: ApprovalFileChangeKind, diff: String) -> Self {
        Self { path, kind, diff }
    }

    /// Borrows the affected path.
    #[must_use]
    pub const fn path(&self) -> &NonEmptyText {
        &self.path
    }

    /// Returns the proposed change kind.
    #[must_use]
    pub const fn kind(&self) -> ApprovalFileChangeKind {
        self.kind
    }

    /// Borrows the exact unified diff supplied by the Provider.
    #[must_use]
    pub fn diff(&self) -> &str {
        &self.diff
    }
}

/// Structured content of an approval request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ApprovalSubject {
    /// A command, terminal input, or network operation.
    Command {
        /// Whether this is a command or terminal input.
        kind: ApprovalCommandKind,
        /// Exact command when supplied by the Provider.
        command: Option<NonEmptyText>,
        /// Exact working directory when supplied by the Provider.
        cwd: Option<NonEmptyText>,
        /// Provider explanation for the request.
        reason: Option<NonEmptyText>,
        /// Optional network target under review.
        network: Option<ApprovalNetworkContext>,
    },
    /// One or more proposed file changes.
    FileChange {
        /// Exact changes in Provider display order.
        changes: Vec<ApprovalFileChange>,
        /// Optional root requested for session-scoped write access.
        grant_root: Option<NonEmptyText>,
        /// Provider explanation for the request.
        reason: Option<NonEmptyText>,
    },
}

/// One opaque, Provider-issued choice in an approval request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalOption {
    id: ApprovalOptionId,
    disposition: ApprovalDisposition,
    label: NonEmptyText,
    description: Option<NonEmptyText>,
}

impl ApprovalOption {
    /// Creates an approval option.
    #[must_use]
    pub const fn new(
        id: ApprovalOptionId,
        disposition: ApprovalDisposition,
        label: NonEmptyText,
    ) -> Self {
        Self {
            id,
            disposition,
            label,
            description: None,
        }
    }

    /// Adds an exact user-facing explanation of the option's effect.
    #[must_use]
    pub fn with_description(mut self, description: NonEmptyText) -> Self {
        self.description = Some(description);
        self
    }

    /// Returns the opaque option identifier.
    #[must_use]
    pub const fn id(&self) -> ApprovalOptionId {
        self.id
    }

    /// Returns the broad user-visible disposition.
    #[must_use]
    pub const fn disposition(&self) -> ApprovalDisposition {
        self.disposition
    }

    /// Borrows the option label.
    #[must_use]
    pub const fn label(&self) -> &NonEmptyText {
        &self.label
    }

    /// Borrows the optional effect description.
    #[must_use]
    pub const fn description(&self) -> Option<&NonEmptyText> {
        self.description.as_ref()
    }
}

/// A structured approval request and its exact Provider-issued options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalRequest {
    subject: ApprovalSubject,
    options: Vec<ApprovalOption>,
    unavailable_reason: Option<NonEmptyText>,
}

impl ApprovalRequest {
    /// Creates an actionable request with at least one uniquely identified option.
    pub fn actionable(
        subject: ApprovalSubject,
        options: Vec<ApprovalOption>,
    ) -> Result<Self, DomainError> {
        if options.is_empty() {
            return Err(DomainError::EmptyCollection {
                field: "approval options",
            });
        }

        let mut unique = HashSet::with_capacity(options.len());
        for option in &options {
            if !unique.insert(option.id()) {
                return Err(DomainError::DuplicateId {
                    field: "approval options",
                    value: option.id().to_string(),
                });
            }
        }

        Ok(Self {
            subject,
            options,
            unavailable_reason: None,
        })
    }

    /// Creates a visible but non-actionable request with an explicit reason.
    #[must_use]
    pub const fn unavailable(subject: ApprovalSubject, reason: NonEmptyText) -> Self {
        Self {
            subject,
            options: Vec::new(),
            unavailable_reason: Some(reason),
        }
    }

    /// Borrows the structured operation under review.
    #[must_use]
    pub const fn subject(&self) -> &ApprovalSubject {
        &self.subject
    }

    /// Borrows the offered options in display order.
    #[must_use]
    pub fn options(&self) -> &[ApprovalOption] {
        &self.options
    }

    /// Borrows the reason this request is read-only, when it is unavailable.
    #[must_use]
    pub const fn unavailable_reason(&self) -> Option<&NonEmptyText> {
        self.unavailable_reason.as_ref()
    }

    /// Returns whether the request exposes a response action.
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        !self.options.is_empty()
    }

    fn contains(&self, option_id: ApprovalOptionId) -> bool {
        self.options.iter().any(|option| option.id() == option_id)
    }
}

/// One selectable option in a choice request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChoiceOption {
    id: ChoiceOptionId,
    label: NonEmptyText,
    description: Option<NonEmptyText>,
}

impl ChoiceOption {
    /// Creates a choice option.
    #[must_use]
    pub const fn new(id: ChoiceOptionId, label: NonEmptyText) -> Self {
        Self {
            id,
            label,
            description: None,
        }
    }

    /// Adds an optional user-facing description.
    #[must_use]
    pub fn with_description(mut self, description: NonEmptyText) -> Self {
        self.description = Some(description);
        self
    }

    /// Returns the option identifier.
    #[must_use]
    pub const fn id(&self) -> ChoiceOptionId {
        self.id
    }

    /// Borrows the option label.
    #[must_use]
    pub const fn label(&self) -> &NonEmptyText {
        &self.label
    }

    /// Borrows the optional option description.
    #[must_use]
    pub const fn description(&self) -> Option<&NonEmptyText> {
        self.description.as_ref()
    }
}

/// A validated single- or multiple-choice request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChoiceRequest {
    options: Vec<ChoiceOption>,
    multiple: bool,
}

impl ChoiceRequest {
    /// Creates a choice request with at least one uniquely identified option.
    pub fn new(options: Vec<ChoiceOption>, multiple: bool) -> Result<Self, DomainError> {
        if options.is_empty() {
            return Err(DomainError::EmptyCollection {
                field: "choice options",
            });
        }

        let mut identifiers = HashSet::with_capacity(options.len());
        for option in &options {
            if !identifiers.insert(option.id()) {
                return Err(DomainError::DuplicateId {
                    field: "choice options",
                    value: option.id().to_string(),
                });
            }
        }

        Ok(Self { options, multiple })
    }

    /// Borrows the options in display order.
    #[must_use]
    pub fn options(&self) -> &[ChoiceOption] {
        &self.options
    }

    /// Returns whether multiple options may be selected.
    #[must_use]
    pub const fn allows_multiple(&self) -> bool {
        self.multiple
    }

    fn contains(&self, option_id: ChoiceOptionId) -> bool {
        self.options.iter().any(|option| option.id() == option_id)
    }
}

/// Presentation hints for a non-sensitive text-input request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextInputRequest {
    placeholder: Option<NonEmptyText>,
    multiline: bool,
}

impl TextInputRequest {
    /// Creates a text-input request.
    #[must_use]
    pub const fn new(multiline: bool) -> Self {
        Self {
            placeholder: None,
            multiline,
        }
    }

    /// Adds a user-facing placeholder.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: NonEmptyText) -> Self {
        self.placeholder = Some(placeholder);
        self
    }

    /// Borrows the optional placeholder.
    #[must_use]
    pub const fn placeholder(&self) -> Option<&NonEmptyText> {
        self.placeholder.as_ref()
    }

    /// Returns whether the Channel should offer multiline input.
    #[must_use]
    pub const fn multiline(&self) -> bool {
        self.multiline
    }
}

/// The semantic payload of an interaction request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InteractionRequestPayload {
    /// Requests an approval decision.
    Approval(ApprovalRequest),
    /// Requests a single or multiple choice.
    Choice(ChoiceRequest),
    /// Requests non-sensitive text.
    Text(TextInputRequest),
}

impl InteractionRequestPayload {
    /// Returns the Provider capability needed to publish this request.
    #[must_use]
    pub const fn required_provider_request_capability(&self) -> ProviderCapabilities {
        match self {
            Self::Approval(_) => ProviderCapabilities::APPROVAL_REQUEST,
            Self::Choice(_) | Self::Text(_) => ProviderCapabilities::USER_INPUT_REQUEST,
        }
    }

    /// Returns the Provider capability needed to accept a response.
    #[must_use]
    pub const fn required_provider_response_capability(&self) -> ProviderCapabilities {
        match self {
            Self::Approval(_) => ProviderCapabilities::APPROVAL_RESPONSE,
            Self::Choice(_) | Self::Text(_) => ProviderCapabilities::USER_INPUT_RESPONSE,
        }
    }

    /// Returns the Channel capability needed to collect a response.
    #[must_use]
    pub const fn required_channel_response_capability(&self) -> ChannelCapabilities {
        match self {
            Self::Approval(_) => ChannelCapabilities::APPROVAL,
            Self::Choice(_) => ChannelCapabilities::CHOICE_INPUT,
            Self::Text(_) => ChannelCapabilities::TEXT_INPUT,
        }
    }

    /// Returns whether the request itself offers at least one valid response.
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        match self {
            Self::Approval(request) => request.is_actionable(),
            Self::Choice(_) | Self::Text(_) => true,
        }
    }
}

/// A Provider-originated request for one correlated user response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionRequest {
    id: InteractionId,
    session_id: SessionId,
    requested_at: Timestamp,
    expires_at: Option<Timestamp>,
    prompt: NonEmptyText,
    payload: InteractionRequestPayload,
}

impl InteractionRequest {
    /// Creates an interaction request without an expiration time.
    #[must_use]
    pub const fn new(
        id: InteractionId,
        session_id: SessionId,
        requested_at: Timestamp,
        prompt: NonEmptyText,
        payload: InteractionRequestPayload,
    ) -> Self {
        Self {
            id,
            session_id,
            requested_at,
            expires_at: None,
            prompt,
            payload,
        }
    }

    /// Sets an expiration time later than the request time.
    pub fn with_expiration(mut self, expires_at: Timestamp) -> Result<Self, DomainError> {
        if expires_at <= self.requested_at {
            return Err(DomainError::InvalidTimeOrder {
                earlier_field: "interaction requested_at",
                later_field: "interaction expires_at",
            });
        }
        self.expires_at = Some(expires_at);
        Ok(self)
    }

    /// Returns the interaction identifier.
    #[must_use]
    pub const fn id(&self) -> InteractionId {
        self.id
    }

    /// Returns the owning session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns when the request was created.
    #[must_use]
    pub const fn requested_at(&self) -> Timestamp {
        self.requested_at
    }

    /// Returns the optional expiration time.
    #[must_use]
    pub const fn expires_at(&self) -> Option<Timestamp> {
        self.expires_at
    }

    /// Borrows the user-facing prompt.
    #[must_use]
    pub const fn prompt(&self) -> &NonEmptyText {
        &self.prompt
    }

    /// Borrows the semantic request payload.
    #[must_use]
    pub const fn payload(&self) -> &InteractionRequestPayload {
        &self.payload
    }

    /// Returns the Provider capability needed to publish this request.
    #[must_use]
    pub const fn required_provider_request_capability(&self) -> ProviderCapabilities {
        self.payload.required_provider_request_capability()
    }

    /// Returns the Provider capability needed to accept a response.
    #[must_use]
    pub const fn required_provider_response_capability(&self) -> ProviderCapabilities {
        self.payload.required_provider_response_capability()
    }

    /// Returns the Channel capability needed to collect a response.
    #[must_use]
    pub const fn required_channel_response_capability(&self) -> ChannelCapabilities {
        self.payload.required_channel_response_capability()
    }

    /// Validates that a response is correlated, timely, and semantically compatible.
    pub fn validate_response(&self, response: &InteractionResponse) -> Result<(), DomainError> {
        if response.request_id != self.id {
            return Err(DomainError::CorrelationMismatch {
                field: "interaction request ID",
                expected: self.id.to_string(),
                actual: response.request_id.to_string(),
            });
        }
        if response.session_id != self.session_id {
            return Err(DomainError::CorrelationMismatch {
                field: "interaction session ID",
                expected: self.session_id.to_string(),
                actual: response.session_id.to_string(),
            });
        }
        if response.responded_at < self.requested_at {
            return Err(DomainError::InvalidTimeOrder {
                earlier_field: "interaction requested_at",
                later_field: "interaction responded_at",
            });
        }
        if self
            .expires_at
            .is_some_and(|expires_at| response.responded_at > expires_at)
        {
            return Err(DomainError::InteractionExpired);
        }

        match (&self.payload, &response.payload) {
            (
                InteractionRequestPayload::Approval(request),
                InteractionResponsePayload::Approval(_selection),
            ) if !request.is_actionable() => Err(DomainError::ApprovalUnavailable),
            (
                InteractionRequestPayload::Approval(request),
                InteractionResponsePayload::Approval(selection),
            ) if !request.contains(selection.option_id()) => {
                Err(DomainError::UnknownApprovalOption {
                    option: selection.option_id().to_string(),
                })
            }
            (InteractionRequestPayload::Approval(_), InteractionResponsePayload::Approval(_))
            | (InteractionRequestPayload::Text(_), InteractionResponsePayload::Text(_)) => Ok(()),
            (
                InteractionRequestPayload::Choice(request),
                InteractionResponsePayload::Choice(selection),
            ) => {
                if !request.allows_multiple() && selection.option_ids.len() != 1 {
                    return Err(DomainError::InvalidChoiceSelection {
                        reason: "single-choice requests require exactly one selected option",
                    });
                }
                for option_id in &selection.option_ids {
                    if !request.contains(*option_id) {
                        return Err(DomainError::UnknownChoiceOption {
                            option: option_id.to_string(),
                        });
                    }
                }
                Ok(())
            }
            _ => Err(DomainError::InteractionTypeMismatch),
        }
    }
}

/// Why a pending interaction disappeared without a Channel response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InteractionCloseReason {
    /// Codex or another attached client resolved the request.
    ResolvedElsewhere,
    /// The Provider cancelled the request because its owning operation ended.
    ProviderCancelled,
}

/// Provider-originated closure of one pending interaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionClosed {
    request_id: InteractionId,
    session_id: SessionId,
    reason: InteractionCloseReason,
}

impl InteractionClosed {
    /// Creates a correlated interaction closure.
    #[must_use]
    pub const fn new(
        request_id: InteractionId,
        session_id: SessionId,
        reason: InteractionCloseReason,
    ) -> Self {
        Self {
            request_id,
            session_id,
            reason,
        }
    }

    /// Returns the closed request identifier.
    #[must_use]
    pub const fn request_id(&self) -> InteractionId {
        self.request_id
    }

    /// Returns the owning session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns why the request was closed.
    #[must_use]
    pub const fn reason(&self) -> InteractionCloseReason {
        self.reason
    }
}

/// A selection of one opaque option from an approval request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalSelection {
    option_id: ApprovalOptionId,
}

impl ApprovalSelection {
    /// Selects one Provider-issued approval option.
    #[must_use]
    pub const fn new(option_id: ApprovalOptionId) -> Self {
        Self { option_id }
    }

    /// Returns the selected option identifier.
    #[must_use]
    pub const fn option_id(&self) -> ApprovalOptionId {
        self.option_id
    }
}

/// A non-empty set of uniquely selected choice option identifiers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChoiceSelection {
    option_ids: Vec<ChoiceOptionId>,
}

impl ChoiceSelection {
    /// Creates a selection and rejects empty or duplicate option identifiers.
    pub fn new(option_ids: Vec<ChoiceOptionId>) -> Result<Self, DomainError> {
        if option_ids.is_empty() {
            return Err(DomainError::EmptyCollection {
                field: "selected choice options",
            });
        }
        let mut unique = HashSet::with_capacity(option_ids.len());
        for option_id in &option_ids {
            if !unique.insert(*option_id) {
                return Err(DomainError::DuplicateId {
                    field: "selected choice options",
                    value: option_id.to_string(),
                });
            }
        }
        Ok(Self { option_ids })
    }

    /// Borrows selected option identifiers.
    #[must_use]
    pub fn option_ids(&self) -> &[ChoiceOptionId] {
        &self.option_ids
    }
}

/// The semantic payload of an interaction response.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InteractionResponsePayload {
    /// Answers an approval request.
    Approval(ApprovalSelection),
    /// Answers a choice request.
    Choice(ChoiceSelection),
    /// Answers a text-input request.
    Text(NonEmptyText),
}

impl InteractionResponsePayload {
    /// Returns the Provider capability needed to accept this response.
    #[must_use]
    pub const fn required_provider_capability(&self) -> ProviderCapabilities {
        match self {
            Self::Approval(_) => ProviderCapabilities::APPROVAL_RESPONSE,
            Self::Choice(_) | Self::Text(_) => ProviderCapabilities::USER_INPUT_RESPONSE,
        }
    }

    /// Returns the Channel capability needed to originate this response.
    #[must_use]
    pub const fn required_channel_capability(&self) -> ChannelCapabilities {
        match self {
            Self::Approval(_) => ChannelCapabilities::APPROVAL,
            Self::Choice(_) => ChannelCapabilities::CHOICE_INPUT,
            Self::Text(_) => ChannelCapabilities::TEXT_INPUT,
        }
    }
}

/// A Channel-originated response to one interaction request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionResponse {
    request_id: InteractionId,
    session_id: SessionId,
    channel_id: ChannelId,
    responded_at: Timestamp,
    payload: InteractionResponsePayload,
}

impl InteractionResponse {
    /// Creates an interaction response for later validation against its request.
    #[must_use]
    pub const fn new(
        request_id: InteractionId,
        session_id: SessionId,
        channel_id: ChannelId,
        responded_at: Timestamp,
        payload: InteractionResponsePayload,
    ) -> Self {
        Self {
            request_id,
            session_id,
            channel_id,
            responded_at,
            payload,
        }
    }

    /// Returns the correlated request identifier.
    #[must_use]
    pub const fn request_id(&self) -> InteractionId {
        self.request_id
    }

    /// Returns the owning session identifier.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the source Channel identifier.
    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// Returns when the response was created.
    #[must_use]
    pub const fn responded_at(&self) -> Timestamp {
        self.responded_at
    }

    /// Borrows the response payload.
    #[must_use]
    pub const fn payload(&self) -> &InteractionResponsePayload {
        &self.payload
    }

    /// Returns the Provider capability needed to accept this response.
    #[must_use]
    pub const fn required_provider_capability(&self) -> ProviderCapabilities {
        self.payload.required_provider_capability()
    }

    /// Returns the Channel capability needed to originate this response.
    #[must_use]
    pub const fn required_channel_capability(&self) -> ChannelCapabilities {
        self.payload.required_channel_capability()
    }
}
