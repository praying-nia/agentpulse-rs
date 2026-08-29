//! Bridge-facing Native Channel Port.

use std::sync::MutexGuard;

use agentpulse_bridge::ChannelPort;
use agentpulse_core::{AgentEvent, AgentSession, ChannelDescriptor, ChannelEventRoute, SessionId};
use agentpulse_protocol::ProtocolMessage;

use crate::{
    NativeChannelPortError, NativeDeliveryContext, NativeEventRoute, NativeServerMessage,
    encode_server_message,
    state::{DeliveryState, SharedDeliveryState},
    status::lock_status,
};

/// A read-only Channel Port that queues normalized domain frames for one client.
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
                        .map_err(|_| NativeChannelPortError::InteractiveRoute)?,
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
                        .map_err(|_| NativeChannelPortError::InteractiveRoute)?,
                },
                None => NativeDeliveryContext::LiveSession,
            }
        } else {
            return Err(NativeChannelPortError::SessionNotSubscribed { session_id });
        };

        let frame = encoded_text(&NativeServerMessage::Domain { context, message })?;
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
            pending.frames.push_back(frame);
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
