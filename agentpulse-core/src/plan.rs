//! Complete plan snapshots.

use std::collections::HashSet;

use crate::{DomainError, NonEmptyText, PlanItemId, Revision};

/// The normalized state of one plan item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PlanItemStatus {
    /// Work has not started.
    Pending,
    /// Work is currently in progress.
    InProgress,
    /// Work completed successfully.
    Completed,
    /// Work cannot currently proceed.
    Blocked,
    /// Work was intentionally omitted.
    Skipped,
}

/// One stable item in a plan snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanItem {
    id: PlanItemId,
    content: NonEmptyText,
    status: PlanItemStatus,
}

impl PlanItem {
    /// Creates a plan item.
    #[must_use]
    pub const fn new(id: PlanItemId, content: NonEmptyText, status: PlanItemStatus) -> Self {
        Self {
            id,
            content,
            status,
        }
    }

    /// Returns the item identifier.
    #[must_use]
    pub const fn id(&self) -> PlanItemId {
        self.id
    }

    /// Borrows the item content.
    #[must_use]
    pub const fn content(&self) -> &NonEmptyText {
        &self.content
    }

    /// Returns the item status.
    #[must_use]
    pub const fn status(&self) -> PlanItemStatus {
        self.status
    }
}

/// A complete, replace-on-newer-revision plan snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanSnapshot {
    revision: Revision,
    explanation: Option<NonEmptyText>,
    items: Vec<PlanItem>,
}

impl PlanSnapshot {
    /// Creates a plan snapshot and rejects duplicate item identifiers.
    pub fn new(revision: Revision, items: Vec<PlanItem>) -> Result<Self, DomainError> {
        let mut identifiers = HashSet::with_capacity(items.len());
        for item in &items {
            if !identifiers.insert(item.id()) {
                return Err(DomainError::DuplicateId {
                    field: "plan items",
                    value: item.id().to_string(),
                });
            }
        }

        Ok(Self {
            revision,
            explanation: None,
            items,
        })
    }

    /// Adds an explanation for the complete snapshot.
    #[must_use]
    pub fn with_explanation(mut self, explanation: NonEmptyText) -> Self {
        self.explanation = Some(explanation);
        self
    }

    /// Returns the plan revision.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Borrows the optional explanation.
    #[must_use]
    pub const fn explanation(&self) -> Option<&NonEmptyText> {
        self.explanation.as_ref()
    }

    /// Borrows all plan items in display order.
    #[must_use]
    pub fn items(&self) -> &[PlanItem] {
        &self.items
    }
}
