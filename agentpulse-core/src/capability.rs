//! Provider and Channel capability declarations.

use std::ops::{BitOr, BitOrAssign};

use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
    struct ProviderCapabilityBits: u16 {
        const SESSION_STATE = 1 << 0;
        const TOOL_EVENTS = 1 << 1;
        const PLAN = 1 << 2;
        const PROGRESS = 1 << 3;
        const APPROVAL_REQUEST = 1 << 4;
        const APPROVAL_RESPONSE = 1 << 5;
        const USER_INPUT_REQUEST = 1 << 6;
        const USER_INPUT_RESPONSE = 1 << 7;
        const PROMPT_SUBMIT = 1 << 8;
        const CANCEL = 1 << 9;
    }
}

/// Agent-side events and write-back operations supported by a Provider.
///
/// The underlying bit representation is deliberately private and is not a
/// protocol encoding.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ProviderCapabilities(ProviderCapabilityBits);

impl ProviderCapabilities {
    /// No declared Provider capabilities.
    pub const NONE: Self = Self(ProviderCapabilityBits::empty());
    /// Publishes current session state.
    pub const SESSION_STATE: Self = Self(ProviderCapabilityBits::SESSION_STATE);
    /// Publishes normalized tool activity.
    pub const TOOL_EVENTS: Self = Self(ProviderCapabilityBits::TOOL_EVENTS);
    /// Publishes complete plan snapshots.
    pub const PLAN: Self = Self(ProviderCapabilityBits::PLAN);
    /// Publishes progress snapshots.
    pub const PROGRESS: Self = Self(ProviderCapabilityBits::PROGRESS);
    /// Publishes approval requests.
    pub const APPROVAL_REQUEST: Self = Self(ProviderCapabilityBits::APPROVAL_REQUEST);
    /// Accepts approval responses.
    pub const APPROVAL_RESPONSE: Self = Self(ProviderCapabilityBits::APPROVAL_RESPONSE);
    /// Publishes user-input requests.
    pub const USER_INPUT_REQUEST: Self = Self(ProviderCapabilityBits::USER_INPUT_REQUEST);
    /// Accepts user-input responses.
    pub const USER_INPUT_RESPONSE: Self = Self(ProviderCapabilityBits::USER_INPUT_RESPONSE);
    /// Accepts a prompt submitted from a Channel.
    pub const PROMPT_SUBMIT: Self = Self(ProviderCapabilityBits::PROMPT_SUBMIT);
    /// Accepts session cancellation.
    pub const CANCEL: Self = Self(ProviderCapabilityBits::CANCEL);

    /// Returns whether all requested capabilities are present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0.contains(other.0)
    }

    /// Returns whether any requested capability is present.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0.intersects(other.0)
    }

    /// Returns whether no capability is declared.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Returns the union of two capability sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0.union(other.0))
    }
}

impl BitOr for ProviderCapabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl BitOrAssign for ProviderCapabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
    struct ChannelCapabilityBits: u16 {
        const NOTIFICATION = 1 << 0;
        const SESSION_VIEW = 1 << 1;
        const TOOL_VIEW = 1 << 2;
        const PLAN_VIEW = 1 << 3;
        const PROGRESS_VIEW = 1 << 4;
        const RICH_MESSAGE = 1 << 5;
        const APPROVAL = 1 << 6;
        const CHOICE_INPUT = 1 << 7;
        const TEXT_INPUT = 1 << 8;
        const FORM_INPUT = 1 << 9;
        const REALTIME_SYNC = 1 << 10;
        const REMOTE_COMMAND = 1 << 11;
    }
}

/// Presentation and input surfaces supported by a Channel.
///
/// The underlying bit representation is deliberately private and is not a
/// protocol encoding.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ChannelCapabilities(ChannelCapabilityBits);

impl ChannelCapabilities {
    /// No declared Channel capabilities.
    pub const NONE: Self = Self(ChannelCapabilityBits::empty());
    /// Delivers notifications.
    pub const NOTIFICATION: Self = Self(ChannelCapabilityBits::NOTIFICATION);
    /// Displays session state.
    pub const SESSION_VIEW: Self = Self(ChannelCapabilityBits::SESSION_VIEW);
    /// Displays structured tool activity.
    pub const TOOL_VIEW: Self = Self(ChannelCapabilityBits::TOOL_VIEW);
    /// Displays structured plans.
    pub const PLAN_VIEW: Self = Self(ChannelCapabilityBits::PLAN_VIEW);
    /// Displays structured progress.
    pub const PROGRESS_VIEW: Self = Self(ChannelCapabilityBits::PROGRESS_VIEW);
    /// Displays platform-specific rich messages.
    pub const RICH_MESSAGE: Self = Self(ChannelCapabilityBits::RICH_MESSAGE);
    /// Collects approval decisions.
    pub const APPROVAL: Self = Self(ChannelCapabilityBits::APPROVAL);
    /// Collects a choice selection.
    pub const CHOICE_INPUT: Self = Self(ChannelCapabilityBits::CHOICE_INPUT);
    /// Collects text input.
    pub const TEXT_INPUT: Self = Self(ChannelCapabilityBits::TEXT_INPUT);
    /// Collects structured form input.
    pub const FORM_INPUT: Self = Self(ChannelCapabilityBits::FORM_INPUT);
    /// Keeps a real-time synchronized view.
    pub const REALTIME_SYNC: Self = Self(ChannelCapabilityBits::REALTIME_SYNC);
    /// Sends remote commands toward an agent.
    pub const REMOTE_COMMAND: Self = Self(ChannelCapabilityBits::REMOTE_COMMAND);

    /// Returns whether all requested capabilities are present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0.contains(other.0)
    }

    /// Returns whether any requested capability is present.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0.intersects(other.0)
    }

    /// Returns whether no capability is declared.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Returns the union of two capability sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0.union(other.0))
    }
}

impl BitOr for ChannelCapabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl BitOrAssign for ChannelCapabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}
