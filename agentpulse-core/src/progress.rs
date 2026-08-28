//! Complete progress snapshots.

use std::num::NonZeroU64;

use crate::{DomainError, NonEmptyText, Revision};

/// A validated determinate progress value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeterminateProgress {
    completed: u64,
    total: NonZeroU64,
}

impl DeterminateProgress {
    /// Creates determinate progress where completed units do not exceed total units.
    pub fn new(completed: u64, total: u64) -> Result<Self, DomainError> {
        let total = NonZeroU64::new(total).ok_or(DomainError::ZeroValue {
            field: "progress total",
        })?;
        if completed > total.get() {
            return Err(DomainError::InvalidProgress {
                completed,
                total: total.get(),
            });
        }
        Ok(Self { completed, total })
    }

    /// Returns completed units.
    #[must_use]
    pub const fn completed(self) -> u64 {
        self.completed
    }

    /// Returns total units.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.total.get()
    }
}

/// Whether progress is measurable or currently indeterminate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProgressValue {
    /// Progress is active but cannot be measured.
    Indeterminate,
    /// Progress has a validated completed/total pair.
    Determinate(DeterminateProgress),
}

/// A complete, replace-on-newer-revision progress snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgressSnapshot {
    revision: Revision,
    value: ProgressValue,
    message: Option<NonEmptyText>,
}

impl ProgressSnapshot {
    /// Creates a progress snapshot.
    #[must_use]
    pub const fn new(revision: Revision, value: ProgressValue) -> Self {
        Self {
            revision,
            value,
            message: None,
        }
    }

    /// Adds a user-facing progress message.
    #[must_use]
    pub fn with_message(mut self, message: NonEmptyText) -> Self {
        self.message = Some(message);
        self
    }

    /// Returns the progress revision.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Returns the progress value.
    #[must_use]
    pub const fn value(&self) -> ProgressValue {
        self.value
    }

    /// Borrows the optional progress message.
    #[must_use]
    pub const fn message(&self) -> Option<&NonEmptyText> {
        self.message.as_ref()
    }
}
