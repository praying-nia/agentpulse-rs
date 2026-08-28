//! Shared domain value objects.

use std::{fmt, num::NonZeroU64, str::FromStr};

use time::{OffsetDateTime, UtcOffset};

use crate::DomainError;

/// A Unicode string that contains at least one non-whitespace character.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NonEmptyText(String);

impl NonEmptyText {
    /// Validates and creates non-empty text while preserving its original form.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            Err(DomainError::BlankText { field: "text" })
        } else {
            Ok(Self(value))
        }
    }

    /// Borrows the original text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the original text.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for NonEmptyText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for NonEmptyText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for NonEmptyText {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// An opaque, provider- or channel-owned external identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExternalId(NonEmptyText);

impl ExternalId {
    /// Validates and creates an opaque external identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            Err(DomainError::BlankText {
                field: "external ID",
            })
        } else {
            Ok(Self(NonEmptyText(value)))
        }
    }

    /// Borrows the external identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ExternalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

macro_rules! define_kind {
    ($(#[$meta:meta])* $name:ident, $field:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Validates a lowercase ASCII component slug.
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                let mut characters = value.chars();
                let valid_first = characters
                    .next()
                    .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit());
                let valid_rest = characters.all(|character| {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || matches!(character, '-' | '_' | '.')
                });

                if valid_first && valid_rest {
                    Ok(Self(value))
                } else {
                    Err(DomainError::InvalidKind {
                        field: $field,
                        value,
                    })
                }
            }

            /// Borrows the component slug.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

define_kind!(
    /// Identifies a Provider implementation family, such as `codex`.
    ProviderKind,
    "provider kind"
);
define_kind!(
    /// Identifies a Channel implementation family, such as `feishu`.
    ChannelKind,
    "channel kind"
);

/// A point in time normalized to UTC.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// Captures the current UTC time.
    #[must_use]
    pub fn now_utc() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// Normalizes an offset-aware date-time to UTC.
    #[must_use]
    pub fn from_offset_date_time(value: OffsetDateTime) -> Self {
        Self(value.to_offset(UtcOffset::UTC))
    }

    /// Creates a UTC timestamp from Unix nanoseconds.
    pub fn from_unix_timestamp_nanos(value: i128) -> Result<Self, DomainError> {
        OffsetDateTime::from_unix_timestamp_nanos(value)
            .map(Self::from_offset_date_time)
            .map_err(|_| DomainError::InvalidTimestamp { value })
    }

    /// Returns the normalized date-time.
    #[must_use]
    pub const fn as_offset_date_time(self) -> OffsetDateTime {
        self.0
    }

    /// Returns Unix nanoseconds for transport-independent comparison.
    #[must_use]
    pub fn unix_timestamp_nanos(self) -> i128 {
        self.0.unix_timestamp_nanos()
    }
}

macro_rules! define_positive_counter {
    ($(#[$meta:meta])* $name:ident, $field:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// The first valid value.
            pub const FIRST: Self = Self(NonZeroU64::MIN);

            /// Creates a non-zero counter.
            pub fn new(value: u64) -> Result<Self, DomainError> {
                NonZeroU64::new(value)
                    .map(Self)
                    .ok_or(DomainError::ZeroValue { field: $field })
            }

            /// Returns the counter value.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl TryFrom<u64> for $name {
            type Error = DomainError;

            fn try_from(value: u64) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

define_positive_counter!(
    /// Identifies a complete snapshot revision within its own stream.
    Revision,
    "revision"
);
define_positive_counter!(
    /// Orders events within one AgentPulse session.
    EventSequence,
    "event sequence"
);

/// An opaque workspace path and optional user-facing name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRef {
    path: NonEmptyText,
    display_name: Option<NonEmptyText>,
}

impl WorkspaceRef {
    /// Creates a workspace reference from an opaque path.
    #[must_use]
    pub const fn new(path: NonEmptyText) -> Self {
        Self {
            path,
            display_name: None,
        }
    }

    /// Adds a user-facing workspace name.
    #[must_use]
    pub fn with_display_name(mut self, display_name: NonEmptyText) -> Self {
        self.display_name = Some(display_name);
        self
    }

    /// Borrows the opaque path.
    #[must_use]
    pub const fn path(&self) -> &NonEmptyText {
        &self.path
    }

    /// Borrows the optional user-facing name.
    #[must_use]
    pub const fn display_name(&self) -> Option<&NonEmptyText> {
        self.display_name.as_ref()
    }
}
