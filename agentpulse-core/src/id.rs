//! Strongly typed AgentPulse identifiers.

use std::{fmt, str::FromStr};

use uuid::Uuid;

use crate::DomainError;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $entity:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        #[allow(
            clippy::new_without_default,
            reason = "identity generation must remain an explicit operation"
        )]
        impl $name {
            /// Generates a new time-ordered UUIDv7 identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Validates and wraps an existing UUIDv7 value.
            pub fn try_from_uuid(value: Uuid) -> Result<Self, DomainError> {
                if value.get_version_num() == 7 {
                    Ok(Self(value))
                } else {
                    Err(DomainError::InvalidId {
                        entity: $entity,
                        value: value.to_string(),
                    })
                }
            }

            /// Returns the underlying UUID value.
            #[must_use]
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }

            /// Borrows the underlying UUID value.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
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
                let uuid = Uuid::parse_str(value).map_err(|_| DomainError::InvalidId {
                    entity: $entity,
                    value: value.to_owned(),
                })?;
                Self::try_from_uuid(uuid)
            }
        }

        impl TryFrom<Uuid> for $name {
            type Error = DomainError;

            fn try_from(value: Uuid) -> Result<Self, Self::Error> {
                Self::try_from_uuid(value)
            }
        }
    };
}

define_id!(
    /// Identifies one configured Provider instance.
    ProviderId,
    "provider ID"
);
define_id!(
    /// Identifies one configured Channel instance.
    ChannelId,
    "channel ID"
);
define_id!(
    /// Identifies an AgentPulse agent session.
    SessionId,
    "session ID"
);
define_id!(
    /// Identifies a normalized agent event.
    EventId,
    "event ID"
);
define_id!(
    /// Identifies an interaction request.
    InteractionId,
    "interaction ID"
);
define_id!(
    /// Identifies a command sent back to an agent.
    CommandId,
    "command ID"
);
define_id!(
    /// Identifies one item in a plan snapshot.
    PlanItemId,
    "plan item ID"
);
define_id!(
    /// Identifies one option in a choice request.
    ChoiceOptionId,
    "choice option ID"
);
define_id!(
    /// Identifies one provider tool call.
    ToolCallId,
    "tool call ID"
);
