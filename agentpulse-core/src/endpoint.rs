//! Provider and Channel descriptors.

use crate::{
    ChannelCapabilities, ChannelId, ChannelKind, NonEmptyText, ProviderCapabilities, ProviderId,
    ProviderKind,
};

/// Describes one configured Provider instance and its capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDescriptor {
    id: ProviderId,
    kind: ProviderKind,
    display_name: NonEmptyText,
    version: Option<NonEmptyText>,
    capabilities: ProviderCapabilities,
}

impl ProviderDescriptor {
    /// Creates a Provider descriptor.
    #[must_use]
    pub const fn new(
        id: ProviderId,
        kind: ProviderKind,
        display_name: NonEmptyText,
        capabilities: ProviderCapabilities,
    ) -> Self {
        Self {
            id,
            kind,
            display_name,
            version: None,
            capabilities,
        }
    }

    /// Adds the Provider implementation version.
    #[must_use]
    pub fn with_version(mut self, version: NonEmptyText) -> Self {
        self.version = Some(version);
        self
    }

    /// Returns the Provider instance identifier.
    #[must_use]
    pub const fn id(&self) -> ProviderId {
        self.id
    }

    /// Borrows the Provider implementation kind.
    #[must_use]
    pub const fn kind(&self) -> &ProviderKind {
        &self.kind
    }

    /// Borrows the user-facing name.
    #[must_use]
    pub const fn display_name(&self) -> &NonEmptyText {
        &self.display_name
    }

    /// Borrows the optional implementation version.
    #[must_use]
    pub const fn version(&self) -> Option<&NonEmptyText> {
        self.version.as_ref()
    }

    /// Returns the declared Provider capabilities.
    #[must_use]
    pub const fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities
    }
}

/// Describes one configured Channel instance and its capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelDescriptor {
    id: ChannelId,
    kind: ChannelKind,
    display_name: NonEmptyText,
    version: Option<NonEmptyText>,
    capabilities: ChannelCapabilities,
}

impl ChannelDescriptor {
    /// Creates a Channel descriptor.
    #[must_use]
    pub const fn new(
        id: ChannelId,
        kind: ChannelKind,
        display_name: NonEmptyText,
        capabilities: ChannelCapabilities,
    ) -> Self {
        Self {
            id,
            kind,
            display_name,
            version: None,
            capabilities,
        }
    }

    /// Adds the Channel implementation version.
    #[must_use]
    pub fn with_version(mut self, version: NonEmptyText) -> Self {
        self.version = Some(version);
        self
    }

    /// Returns the Channel instance identifier.
    #[must_use]
    pub const fn id(&self) -> ChannelId {
        self.id
    }

    /// Borrows the Channel implementation kind.
    #[must_use]
    pub const fn kind(&self) -> &ChannelKind {
        &self.kind
    }

    /// Borrows the user-facing name.
    #[must_use]
    pub const fn display_name(&self) -> &NonEmptyText {
        &self.display_name
    }

    /// Borrows the optional implementation version.
    #[must_use]
    pub const fn version(&self) -> Option<&NonEmptyText> {
        self.version.as_ref()
    }

    /// Returns the declared Channel capabilities.
    #[must_use]
    pub const fn capabilities(&self) -> ChannelCapabilities {
        self.capabilities
    }
}
