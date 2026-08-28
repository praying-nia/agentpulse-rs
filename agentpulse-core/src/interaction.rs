//! Correlated interaction requests and responses.

use std::collections::HashSet;

use crate::{
    ChannelCapabilities, ChannelId, ChoiceOptionId, DomainError, InteractionId, NonEmptyText,
    ProviderCapabilities, SessionId, Timestamp,
};

/// How long an approval decision applies.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ApprovalScope {
    /// Approve only the requested operation.
    Once,
    /// Approve matching operations for the current session.
    Session,
}

/// A validated set of approval scopes offered to a user.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalRequest {
    allowed_scopes: Vec<ApprovalScope>,
}

impl ApprovalRequest {
    /// Creates an approval request with at least one unique allowed scope.
    pub fn new(allowed_scopes: Vec<ApprovalScope>) -> Result<Self, DomainError> {
        if allowed_scopes.is_empty() {
            return Err(DomainError::EmptyCollection {
                field: "approval scopes",
            });
        }

        let mut unique = HashSet::with_capacity(allowed_scopes.len());
        for scope in &allowed_scopes {
            if !unique.insert(*scope) {
                let value = match scope {
                    ApprovalScope::Once => "once",
                    ApprovalScope::Session => "session",
                };
                return Err(DomainError::DuplicateValue {
                    field: "approval scopes",
                    value,
                });
            }
        }

        Ok(Self { allowed_scopes })
    }

    /// Borrows the offered approval scopes.
    #[must_use]
    pub fn allowed_scopes(&self) -> &[ApprovalScope] {
        &self.allowed_scopes
    }

    /// Returns whether a scope was offered.
    #[must_use]
    pub fn allows(&self, scope: ApprovalScope) -> bool {
        self.allowed_scopes.contains(&scope)
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
                InteractionResponsePayload::Approval(ApprovalDecision::Approved(scope)),
            ) if !request.allows(*scope) => Err(DomainError::ApprovalScopeNotAllowed),
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

/// A user decision for an approval request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ApprovalDecision {
    /// Approve using one of the scopes offered by the request.
    Approved(ApprovalScope),
    /// Reject the operation with an optional reason.
    Rejected {
        /// A user-facing rejection reason.
        reason: Option<NonEmptyText>,
    },
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
    Approval(ApprovalDecision),
    /// Answers a choice request.
    Choice(ChoiceSelection),
    /// Answers a text-input request.
    Text(NonEmptyText),
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
}
