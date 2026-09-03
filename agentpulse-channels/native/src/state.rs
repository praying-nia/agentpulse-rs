//! Shared bounded delivery state between the Native Port and Source worker.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use agentpulse_core::SessionId;

use crate::status::SharedStatus;

pub(crate) struct PendingSubscription {
    pub(crate) request_id: String,
    pub(crate) session_id: SessionId,
    pub(crate) frames: VecDeque<String>,
    pub(crate) trailing_frames: VecDeque<String>,
    pub(crate) sync_delivered: bool,
}

pub(crate) struct ActiveClient {
    pub(crate) discovered: BTreeSet<SessionId>,
    pub(crate) subscriptions: BTreeSet<SessionId>,
    pub(crate) pending: Option<PendingSubscription>,
    pub(crate) session_cursors: BTreeMap<SessionId, u64>,
}

pub(crate) struct DeliveryState {
    pub(crate) client: Option<ActiveClient>,
    pub(crate) outgoing: VecDeque<String>,
    pub(crate) abort_reason: Option<String>,
    pub(crate) capacity: usize,
    pub(crate) status: SharedStatus,
}

impl DeliveryState {
    pub(crate) fn new(capacity: usize, status: SharedStatus) -> Self {
        Self {
            client: None,
            outgoing: VecDeque::with_capacity(capacity),
            abort_reason: None,
            capacity,
            status,
        }
    }

    pub(crate) fn queued_len(&self) -> usize {
        self.outgoing.len()
            + self
                .client
                .as_ref()
                .and_then(|client| client.pending.as_ref())
                .map_or(0, |pending| {
                    pending.frames.len() + pending.trailing_frames.len()
                })
    }

    pub(crate) fn enqueue(&mut self, frame: String) -> Result<(), usize> {
        if self.queued_len() >= self.capacity {
            self.abort_reason = Some(format!(
                "Native client output queue reached its {}-frame limit",
                self.capacity
            ));
            return Err(self.capacity);
        }
        self.outgoing.push_back(frame);
        Ok(())
    }

    pub(crate) fn enqueue_batch(&mut self, frames: Vec<String>) -> Result<(), usize> {
        if self.queued_len().saturating_add(frames.len()) > self.capacity {
            self.abort_reason = Some(format!(
                "Native client output queue reached its {}-frame limit",
                self.capacity
            ));
            return Err(self.capacity);
        }
        self.outgoing.extend(frames);
        Ok(())
    }

    pub(crate) fn clear_connection(&mut self) {
        self.client = None;
        self.outgoing.clear();
        self.abort_reason = None;
    }
}

pub(crate) type SharedDeliveryState = std::sync::Arc<std::sync::Mutex<DeliveryState>>;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::NativeChannelSnapshot;

    use super::*;

    #[test]
    fn output_queue_is_bounded_and_connection_cleanup_is_complete() {
        let status = Arc::new(Mutex::new(NativeChannelSnapshot::default()));
        let mut state = DeliveryState::new(1, status);
        assert_eq!(state.enqueue("first".to_owned()), Ok(()));
        assert_eq!(state.enqueue("second".to_owned()), Err(1));
        assert!(state.abort_reason.is_some());

        state.clear_connection();
        assert!(state.client.is_none());
        assert!(state.outgoing.is_empty());
        assert!(state.abort_reason.is_none());
    }
}
