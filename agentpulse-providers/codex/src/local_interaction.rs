//! Host-owned choices. These are not Codex server requests or permission approvals.
use std::collections::{BTreeMap, VecDeque};

use agentpulse_core::{
    ChoiceOption, ChoiceOptionId, FormAnswerValue, FormField, FormFieldId, FormRequest,
    InteractionId, InteractionRequest, InteractionRequestPayload, InteractionResponse,
    InteractionResponsePayload, NonEmptyText, SessionId, Timestamp,
};
use serde_json::Value;

use crate::{CodexProviderPortError, CodexProviderSourceError};

#[derive(Clone)]
pub(crate) enum LocalAction {
    Implement,
    Revise,
    Model(Value),
    SelectModel {
        model: String,
        effort: Option<String>,
    },
    Models,
    Cancel,
}

pub(crate) struct LocalChoice {
    pub(crate) request: InteractionRequest,
    pub(crate) thread_id: String,
    pub(crate) plan: bool,
    actions: BTreeMap<ChoiceOptionId, LocalAction>,
    claimed: bool,
}

impl LocalChoice {
    pub(crate) fn new(
        session_id: SessionId,
        thread_id: String,
        title: &str,
        prompt: &str,
        plan: bool,
        options: Vec<(String, Option<String>, LocalAction)>,
    ) -> Result<Self, CodexProviderSourceError> {
        let mut actions = BTreeMap::new();
        let mut choices = Vec::new();
        for (label, description, action) in options {
            let id = ChoiceOptionId::new();
            let mut choice = ChoiceOption::new(id, NonEmptyText::new(label)?);
            if let Some(description) = description.filter(|text| !text.trim().is_empty()) {
                choice = choice.with_description(NonEmptyText::new(description)?);
            }
            choices.push(choice);
            actions.insert(id, action);
        }
        let field = FormField::new(
            FormFieldId::new(),
            NonEmptyText::new(title)?,
            NonEmptyText::new(prompt)?,
            choices,
            false,
            false,
        )?;
        Ok(Self {
            request: InteractionRequest::new(
                InteractionId::new(),
                session_id,
                Timestamp::now_utc(),
                NonEmptyText::new(title)?,
                InteractionRequestPayload::Form(FormRequest::new(vec![field], plan)?),
            ),
            thread_id,
            plan,
            actions,
            claimed: false,
        })
    }
}

#[derive(Default)]
pub(crate) struct LocalInteractions {
    pub(crate) pending: BTreeMap<InteractionId, LocalChoice>,
    pub(crate) responses: VecDeque<(InteractionResponse, LocalAction)>,
}

impl LocalInteractions {
    pub(crate) fn claim(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), CodexProviderPortError> {
        let id = response.request_id();
        let pending = self
            .pending
            .get_mut(&id)
            .ok_or(CodexProviderPortError::InteractionNotPending { interaction_id: id })?;
        if pending.claimed {
            return Err(CodexProviderPortError::InteractionAlreadyClaimed { interaction_id: id });
        }
        pending
            .request
            .validate_response(&response)
            .map_err(|_| CodexProviderPortError::UnsupportedInteractionResponse)?;
        let InteractionResponsePayload::Form(form) = response.payload() else {
            return Err(CodexProviderPortError::UnsupportedInteractionResponse);
        };
        let Some(answer) = form.answers().first() else {
            return Err(CodexProviderPortError::UnsupportedInteractionResponse);
        };
        let FormAnswerValue::Choice(option) = answer.value() else {
            return Err(CodexProviderPortError::UnsupportedInteractionResponse);
        };
        let action = pending
            .actions
            .get(option)
            .cloned()
            .ok_or(CodexProviderPortError::UnsupportedInteractionResponse)?;
        pending.claimed = true;
        self.responses.push_back((response, action));
        Ok(())
    }
}
