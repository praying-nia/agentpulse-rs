//! Private heterogeneous endpoint registry support.

use std::error::Error;

use agentpulse_core::{
    AgentCommand, AgentEvent, AgentSession, ChannelDescriptor, ChannelEventRoute,
    InteractionResponse, ProviderDescriptor,
};

use crate::{ChannelPort, ChannelSessionBaseline, ProviderPort};

pub(crate) type BoxAdapterError = Box<dyn Error + Send + Sync + 'static>;

trait ErasedProviderPort: Send {
    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), BoxAdapterError>;

    fn accept_command(&mut self, command: AgentCommand) -> Result<(), BoxAdapterError>;
}

impl<P> ErasedProviderPort for P
where
    P: ProviderPort + 'static,
{
    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), BoxAdapterError> {
        ProviderPort::accept_interaction_response(self, response)
            .map_err(|source| Box::new(source) as BoxAdapterError)
    }

    fn accept_command(&mut self, command: AgentCommand) -> Result<(), BoxAdapterError> {
        ProviderPort::accept_command(self, command)
            .map_err(|source| Box::new(source) as BoxAdapterError)
    }
}

pub(crate) struct RegisteredProvider {
    descriptor: ProviderDescriptor,
    port: Box<dyn ErasedProviderPort>,
}

impl RegisteredProvider {
    pub(crate) fn new<P>(port: P, descriptor: ProviderDescriptor) -> Self
    where
        P: ProviderPort + 'static,
    {
        Self {
            descriptor,
            port: Box::new(port),
        }
    }

    pub(crate) const fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    pub(crate) fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), BoxAdapterError> {
        self.port.accept_interaction_response(response)
    }

    pub(crate) fn accept_command(&mut self, command: AgentCommand) -> Result<(), BoxAdapterError> {
        self.port.accept_command(command)
    }
}

trait ErasedChannelPort: Send {
    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), BoxAdapterError>;

    fn deliver_session(&mut self, session: AgentSession) -> Result<(), BoxAdapterError>;

    fn deliver_session_baseline(
        &mut self,
        baseline: ChannelSessionBaseline,
    ) -> Result<(), BoxAdapterError>;
}

impl<C> ErasedChannelPort for C
where
    C: ChannelPort + 'static,
{
    fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), BoxAdapterError> {
        ChannelPort::deliver_event(self, event, route)
            .map_err(|source| Box::new(source) as BoxAdapterError)
    }

    fn deliver_session(&mut self, session: AgentSession) -> Result<(), BoxAdapterError> {
        ChannelPort::deliver_session(self, session)
            .map_err(|source| Box::new(source) as BoxAdapterError)
    }

    fn deliver_session_baseline(
        &mut self,
        baseline: ChannelSessionBaseline,
    ) -> Result<(), BoxAdapterError> {
        ChannelPort::deliver_session_baseline(self, baseline)
            .map_err(|source| Box::new(source) as BoxAdapterError)
    }
}

pub(crate) struct RegisteredChannel {
    descriptor: ChannelDescriptor,
    port: Box<dyn ErasedChannelPort>,
}

impl RegisteredChannel {
    pub(crate) fn new<C>(port: C, descriptor: ChannelDescriptor) -> Self
    where
        C: ChannelPort + 'static,
    {
        Self {
            descriptor,
            port: Box::new(port),
        }
    }

    pub(crate) const fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    pub(crate) fn deliver_event(
        &mut self,
        event: AgentEvent,
        route: ChannelEventRoute,
    ) -> Result<(), BoxAdapterError> {
        self.port.deliver_event(event, route)
    }

    pub(crate) fn deliver_session(&mut self, session: AgentSession) -> Result<(), BoxAdapterError> {
        self.port.deliver_session(session)
    }

    pub(crate) fn deliver_session_baseline(
        &mut self,
        baseline: ChannelSessionBaseline,
    ) -> Result<(), BoxAdapterError> {
        self.port.deliver_session_baseline(baseline)
    }
}
