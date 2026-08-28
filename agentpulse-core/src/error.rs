//! Domain validation errors.

use thiserror::Error;

/// An error raised when a domain value would violate a local invariant.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum DomainError {
    /// A required text value contains no non-whitespace characters.
    #[error("{field} must not be blank")]
    BlankText {
        /// The semantic field that was rejected.
        field: &'static str,
    },

    /// A component kind is not a valid lowercase slug.
    #[error("{field} must be a lowercase ASCII slug, got `{value}`")]
    InvalidKind {
        /// The semantic field that was rejected.
        field: &'static str,
        /// The rejected value.
        value: String,
    },

    /// An AgentPulse identifier is malformed or is not UUID version 7.
    #[error("invalid {entity} UUIDv7: `{value}`")]
    InvalidId {
        /// The identifier type.
        entity: &'static str,
        /// The rejected value.
        value: String,
    },

    /// A timestamp is outside the supported range.
    #[error("invalid Unix timestamp in nanoseconds: {value}")]
    InvalidTimestamp {
        /// The rejected Unix timestamp.
        value: i128,
    },

    /// A value that starts at one was set to zero.
    #[error("{field} must be greater than zero")]
    ZeroValue {
        /// The semantic field that was rejected.
        field: &'static str,
    },

    /// Two related timestamps are in an invalid order.
    #[error("{later_field} must not be earlier than {earlier_field}")]
    InvalidTimeOrder {
        /// The field that must occur first.
        earlier_field: &'static str,
        /// The field that must occur second.
        later_field: &'static str,
    },

    /// A collection that must contain an item is empty.
    #[error("{field} must contain at least one item")]
    EmptyCollection {
        /// The semantic collection that was rejected.
        field: &'static str,
    },

    /// A collection contains a duplicate typed identifier.
    #[error("duplicate identifier in {field}: {value}")]
    DuplicateId {
        /// The semantic collection that was rejected.
        field: &'static str,
        /// The duplicated identifier.
        value: String,
    },

    /// A collection contains a duplicate semantic value.
    #[error("duplicate value in {field}: {value}")]
    DuplicateValue {
        /// The semantic collection that was rejected.
        field: &'static str,
        /// The duplicated value.
        value: &'static str,
    },

    /// A determinate progress value exceeds its total.
    #[error("completed progress {completed} exceeds total {total}")]
    InvalidProgress {
        /// Completed units.
        completed: u64,
        /// Total units.
        total: u64,
    },

    /// A choice response violates the request's selection mode.
    #[error("invalid choice selection: {reason}")]
    InvalidChoiceSelection {
        /// A stable explanation of the violation.
        reason: &'static str,
    },

    /// Correlated domain objects refer to different identifiers.
    #[error("{field} mismatch: expected {expected}, got {actual}")]
    CorrelationMismatch {
        /// The correlated field.
        field: &'static str,
        /// The expected identifier.
        expected: String,
        /// The actual identifier.
        actual: String,
    },

    /// A response payload does not match the request payload.
    #[error("interaction response type does not match its request")]
    InteractionTypeMismatch,

    /// A response was produced after its request expired.
    #[error("interaction response was produced after the request expired")]
    InteractionExpired,

    /// An approval used a scope that the request did not offer.
    #[error("approval scope is not allowed by the request")]
    ApprovalScopeNotAllowed,

    /// A choice response selected an option absent from the request.
    #[error("choice option is not present in the request: {option}")]
    UnknownChoiceOption {
        /// The unrecognized option identifier.
        option: String,
    },
}
