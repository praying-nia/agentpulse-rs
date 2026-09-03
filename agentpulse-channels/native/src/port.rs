//! Bridge-facing Native Channel Port.

use std::sync::MutexGuard;

use agentpulse_bridge::{ChannelPort, ChannelSessionBaseline, ChannelSessionSync};
use agentpulse_core::{AgentEvent, AgentSession, ChannelDescriptor, ChannelEventRoute, SessionId};
use agentpulse_protocol::ProtocolMessage;

use crate::{
    NativeChannelPortError, NativeDeliveryContext, NativeEventRoute, NativeServerMessage,
    encode_server_message,
    state::{DeliveryState, SharedDeliveryState},
    status::lock_status,
};

/// A Channel Port that queues normalized domain frames for one client.
pub struct NativeChannelPort {
    descriptor: ChannelDescriptor,
    state: SharedDeliveryState,
}

impl NativeChannelPort {
    pub(crate) fn new(descriptor: ChannelDescriptor, state: SharedDeliveryState) -> Self {
        Self { descriptor, state }
    }

    fn deliver(
        &self,
        session_id: SessionId,
        message: ProtocolMessage,
        event_route: Option<ChannelEventRoute>,
    ) -> Result<(), NativeChannelPortError> {
        let mut state = lock_delivery(&self.state);
        let client = state
            .client
            .as_ref()
            .ok_or(NativeChannelPortError::NoActiveClient)?;
        let context = if let Some(pending) = client.pending.as_ref()
            && pending.session_id == session_id
        {
            match event_route {
                Some(route) => NativeDeliveryContext::LiveEvent {
                    route: NativeEventRoute::from_core(route)
                        .map_err(|_| NativeChannelPortError::UnsupportedEventRoute)?,
                },
                None if pending.frames.is_empty() => NativeDeliveryContext::SubscriptionSession {
                    request_id: pending.request_id.clone(),
                },
                None => NativeDeliveryContext::LiveSession,
            }
        } else if client.subscriptions.contains(&session_id) {
            match event_route {
                Some(route) => NativeDeliveryContext::LiveEvent {
                    route: NativeEventRoute::from_core(route)
                        .map_err(|_| NativeChannelPortError::UnsupportedEventRoute)?,
                },
                None => NativeDeliveryContext::LiveSession,
            }
        } else {
            return Err(NativeChannelPortError::SessionNotSubscribed { session_id });
        };

        let frame = encoded_text(&NativeServerMessage::Domain {
            context,
            message: Box::new(message),
        })?;
        let queue_is_full = state.queued_len() >= state.capacity;
        let capacity = state.capacity;
        if let Some(pending) = state
            .client
            .as_mut()
            .and_then(|client| client.pending.as_mut())
            && pending.session_id == session_id
        {
            if queue_is_full {
                state.abort_reason = Some(format!(
                    "Native client output queue reached its {}-frame limit",
                    capacity
                ));
                return Err(NativeChannelPortError::QueueFull { capacity });
            }
            if pending.sync_delivered {
                pending.trailing_frames.push_back(frame);
            } else {
                pending.frames.push_back(frame);
            }
        } else {
            state
                .enqueue(frame)
                .map_err(|capacity| NativeChannelPortError::QueueFull { capacity })?;
        }
        lock_status(&state.status).domain_frames += 1;
        Ok(())
    }
}

impl ChannelPort for NativeChannelPort {
    type Error = NativeChannelPortError;

    fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), Self::Error> {
        self.deliver(
            event.session_id(),
            ProtocolMessage::AgentEvent(event),
            Some(route),
        )
    }

    fn deliver_session(&mut self, session: AgentSession) -> Result<(), Self::Error> {
        self.deliver(session.id(), ProtocolMessage::AgentSession(session), None)
    }

    fn deliver_session_baseline(
        &mut self,
        baseline: ChannelSessionBaseline,
    ) -> Result<(), Self::Error> {
        let session_id = baseline.session().id();
        let request_id = {
            let state = lock_delivery(&self.state);
            let pending = state
                .client
                .as_ref()
                .and_then(|client| client.pending.as_ref())
                .ok_or(NativeChannelPortError::NoActiveClient)?;
            if pending.session_id != session_id {
                return Err(NativeChannelPortError::SessionNotSubscribed { session_id });
            }
            pending.request_id.clone()
        };

        let mut frames = Vec::with_capacity(baseline.pending_interactions().len() + 1);
        frames.push(encoded_text(&NativeServerMessage::Domain {
            context: NativeDeliveryContext::SubscriptionSession {
                request_id: request_id.clone(),
            },
            message: Box::new(ProtocolMessage::AgentSession(baseline.session().clone())),
        })?);
        for interaction in baseline.pending_interactions() {
            frames.push(encoded_text(&NativeServerMessage::Domain {
                context: NativeDeliveryContext::SubscriptionInteraction {
                    request_id: request_id.clone(),
                    route: NativeEventRoute::from_core(ChannelEventRoute::Interaction(
                        interaction.route(),
                    ))
                    .map_err(|_| NativeChannelPortError::UnsupportedEventRoute)?,
                },
                message: Box::new(ProtocolMessage::InteractionRequest(
                    interaction.request().clone(),
                )),
            })?);
        }

        let mut state = lock_delivery(&self.state);
        let queued_len = state.queued_len();
        let capacity = state.capacity;
        if queued_len.saturating_add(frames.len()) > capacity {
            state.abort_reason = Some(format!(
                "Native client output queue reached its {}-frame limit",
                capacity
            ));
            return Err(NativeChannelPortError::QueueFull { capacity });
        }
        let pending = state
            .client
            .as_mut()
            .and_then(|client| client.pending.as_mut())
            .filter(|pending| pending.session_id == session_id && pending.request_id == request_id)
            .ok_or(NativeChannelPortError::NoActiveClient)?;
        pending.frames.extend(frames);
        pending.sync_delivered = true;
        lock_status(&state.status).domain_frames +=
            1 + baseline.pending_interactions().len() as u64;
        Ok(())
    }

    fn deliver_session_sync(&mut self, sync: ChannelSessionSync) -> Result<(), Self::Error> {
        let (session_id, request_id) = {
            let state = lock_delivery(&self.state);
            let pending = state
                .client
                .as_ref()
                .and_then(|client| client.pending.as_ref())
                .ok_or(NativeChannelPortError::NoActiveClient)?;
            (pending.session_id, pending.request_id.clone())
        };
        let mut frames = Vec::new();
        for routed in sync.events() {
            if routed.event().session_id() != session_id {
                return Err(NativeChannelPortError::SessionNotSubscribed { session_id });
            }
            frames.push(encoded_text(&NativeServerMessage::Domain {
                context: NativeDeliveryContext::LiveEvent {
                    route: NativeEventRoute::from_core(routed.route())
                        .map_err(|_| NativeChannelPortError::UnsupportedEventRoute)?,
                },
                message: Box::new(ProtocolMessage::AgentEvent(routed.event().clone())),
            })?);
        }
        if let Some(baseline) = sync.baseline() {
            frames.push(encoded_text(&NativeServerMessage::Domain {
                context: NativeDeliveryContext::SubscriptionSession {
                    request_id: request_id.clone(),
                },
                message: Box::new(ProtocolMessage::AgentSession(baseline.session().clone())),
            })?);
            for interaction in baseline.pending_interactions() {
                frames.push(encoded_text(&NativeServerMessage::Domain {
                    context: NativeDeliveryContext::SubscriptionInteraction {
                        request_id: request_id.clone(),
                        route: NativeEventRoute::from_core(ChannelEventRoute::Interaction(
                            interaction.route(),
                        ))
                        .map_err(|_| NativeChannelPortError::UnsupportedEventRoute)?,
                    },
                    message: Box::new(ProtocolMessage::InteractionRequest(
                        interaction.request().clone(),
                    )),
                })?);
            }
        }
        let mut state = lock_delivery(&self.state);
        let queued_len = state.queued_len();
        let capacity = state.capacity;
        if queued_len.saturating_add(frames.len()) > capacity {
            state.abort_reason = Some(format!(
                "Native client output queue reached its {}-frame limit",
                capacity
            ));
            return Err(NativeChannelPortError::QueueFull { capacity });
        }
        let pending = state
            .client
            .as_mut()
            .and_then(|client| client.pending.as_mut())
            .filter(|pending| pending.session_id == session_id && pending.request_id == request_id)
            .ok_or(NativeChannelPortError::NoActiveClient)?;
        pending.frames.extend(frames);
        pending.sync_delivered = true;
        lock_status(&state.status).domain_frames += sync.events().len() as u64
            + sync.baseline().map_or(0, |baseline| {
                1 + baseline.pending_interactions().len() as u64
            });
        Ok(())
    }
}

fn encoded_text(message: &NativeServerMessage) -> Result<String, NativeChannelPortError> {
    let bytes = encode_server_message(message)?;
    String::from_utf8(bytes).map_err(|error| {
        NativeChannelPortError::Protocol(crate::NativeProtocolError::InvalidField {
            field: "encoded server frame",
            reason: error.to_string(),
        })
    })
}

pub(crate) fn lock_delivery(state: &SharedDeliveryState) -> MutexGuard<'_, DeliveryState> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
