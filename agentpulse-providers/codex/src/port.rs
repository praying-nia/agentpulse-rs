//! Read-only Codex Provider port.

use agentpulse_bridge::ProviderPort;
use agentpulse_core::{AgentCommand, InteractionResponse, ProviderDescriptor};

use crate::CodexProviderPortError;

/// Bridge-facing, read-only Codex Provider port.
pub struct CodexProviderPort {
    descriptor: ProviderDescriptor,
}

impl CodexProviderPort {
    pub(crate) const fn new(descriptor: ProviderDescriptor) -> Self {
        Self { descriptor }
    }
}

impl ProviderPort for CodexProviderPort {
    type Error = CodexProviderPortError;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        _response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        Err(CodexProviderPortError::ReadOnlyInteractionResponse)
    }

    fn accept_command(&mut self, _command: AgentCommand) -> Result<(), Self::Error> {
        Err(CodexProviderPortError::ReadOnlyCommand)
    }
}
