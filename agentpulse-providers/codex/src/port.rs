//! Codex Provider command and approval-response port.

use std::sync::{Arc, Mutex};

use agentpulse_bridge::ProviderPort;
use agentpulse_core::{AgentCommand, InteractionResponse, ProviderDescriptor};

use crate::{CodexProviderPortError, approval::ApprovalRuntimeState};

/// Bridge-facing Codex Provider port.
pub struct CodexProviderPort {
    descriptor: ProviderDescriptor,
    approvals: Arc<Mutex<ApprovalRuntimeState>>,
}

impl CodexProviderPort {
    pub(crate) fn with_approvals(
        descriptor: ProviderDescriptor,
        approvals: Arc<Mutex<ApprovalRuntimeState>>,
    ) -> Self {
        Self {
            descriptor,
            approvals,
        }
    }
}

impl ProviderPort for CodexProviderPort {
    type Error = CodexProviderPortError;

    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn accept_interaction_response(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), Self::Error> {
        self.approvals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .claim(response)
    }

    fn accept_command(&mut self, _command: AgentCommand) -> Result<(), Self::Error> {
        Err(CodexProviderPortError::UnsupportedCommand)
    }
}
