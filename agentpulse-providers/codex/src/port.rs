//! Codex Provider command and approval-response port.

use std::sync::{Arc, Mutex};

use agentpulse_bridge::ProviderPort;
use agentpulse_core::{AgentCommand, InteractionResponse, ProviderDescriptor};

use crate::{
    CodexProviderPortError,
    approval::ApprovalRuntimeState,
    control::{ControlRuntimeState, SharedControlState},
};

/// Bridge-facing Codex Provider port.
pub struct CodexProviderPort {
    descriptor: ProviderDescriptor,
    approvals: Arc<Mutex<ApprovalRuntimeState>>,
    controls: SharedControlState,
}

impl CodexProviderPort {
    pub(crate) fn new(
        descriptor: ProviderDescriptor,
        approvals: Arc<Mutex<ApprovalRuntimeState>>,
        controls: Arc<Mutex<ControlRuntimeState>>,
    ) -> Self {
        Self {
            descriptor,
            approvals,
            controls,
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

    fn accept_command(&mut self, command: AgentCommand) -> Result<(), Self::Error> {
        self.controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accept(command)
    }
}
