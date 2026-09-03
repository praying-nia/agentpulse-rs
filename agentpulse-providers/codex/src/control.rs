//! Bounded in-memory state for the public AgentPulse command surface.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use agentpulse_core::{
    AgentCommand, AgentCommandPayload, NonEmptyText, PromptDelivery, QueueAction, SessionId,
};

use crate::CodexProviderPortError;

pub(crate) const MAX_COMMANDS: usize = 64;
pub(crate) const MAX_PROMPTS_PER_SESSION: usize = 32;
pub(crate) const MAX_PROMPT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_QUEUED_PROMPT_BYTES: usize = 1024 * 1024;

pub(crate) type SharedControlState = Arc<Mutex<ControlRuntimeState>>;

#[derive(Clone, Debug, Default)]
pub(crate) struct TurnDefaults {
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) plan_mode: bool,
    pub(crate) permission_profile: Option<String>,
}

struct PromptQueue {
    paused: bool,
    bytes: usize,
    prompts: VecDeque<NonEmptyText>,
}

impl PromptQueue {
    fn new() -> Self {
        Self {
            paused: false,
            bytes: 0,
            prompts: VecDeque::new(),
        }
    }
}

pub(crate) struct ControlRuntimeState {
    commands: VecDeque<AgentCommand>,
    prompts: BTreeMap<SessionId, PromptQueue>,
    defaults: BTreeMap<SessionId, TurnDefaults>,
    inflight_turns: BTreeMap<SessionId, bool>,
}

impl ControlRuntimeState {
    pub(crate) fn new() -> Self {
        Self {
            commands: VecDeque::new(),
            prompts: BTreeMap::new(),
            defaults: BTreeMap::new(),
            inflight_turns: BTreeMap::new(),
        }
    }

    pub(crate) fn accept(&mut self, command: AgentCommand) -> Result<(), CodexProviderPortError> {
        match command.payload() {
            AgentCommandPayload::SubmitPrompt {
                text,
                delivery: PromptDelivery::Queue,
            } => {
                let bytes = text.as_str().len();
                if bytes > MAX_PROMPT_BYTES {
                    return Err(CodexProviderPortError::PromptTooLarge {
                        bytes,
                        maximum: MAX_PROMPT_BYTES,
                    });
                }
                let total_bytes = self
                    .prompts
                    .values()
                    .map(|queue| queue.bytes)
                    .sum::<usize>();
                let queue = self
                    .prompts
                    .entry(command.session_id())
                    .or_insert_with(PromptQueue::new);
                if queue.prompts.len() >= MAX_PROMPTS_PER_SESSION
                    || total_bytes.saturating_add(bytes) > MAX_QUEUED_PROMPT_BYTES
                {
                    return Err(CodexProviderPortError::PromptQueueFull {
                        capacity: MAX_PROMPTS_PER_SESSION,
                    });
                }
                queue.bytes += bytes;
                queue.prompts.push_back(text.clone());
                Ok(())
            }
            AgentCommandPayload::Queue { action } => {
                let queue = self
                    .prompts
                    .entry(command.session_id())
                    .or_insert_with(PromptQueue::new);
                match action {
                    QueueAction::Pause => queue.paused = true,
                    QueueAction::Resume => queue.paused = false,
                    QueueAction::Clear => {
                        queue.bytes = 0;
                        queue.prompts.clear();
                    }
                }
                self.push(command)
            }
            AgentCommandPayload::CancelSession { .. } => {
                self.prompts
                    .entry(command.session_id())
                    .or_insert_with(PromptQueue::new)
                    .paused = true;
                self.push(command)
            }
            _ => self.push(command),
        }
    }

    fn push(&mut self, command: AgentCommand) -> Result<(), CodexProviderPortError> {
        if self.commands.len() >= MAX_COMMANDS {
            return Err(CodexProviderPortError::CommandQueueFull {
                capacity: MAX_COMMANDS,
            });
        }
        self.commands.push_back(command);
        Ok(())
    }

    pub(crate) fn pop_command(&mut self) -> Option<AgentCommand> {
        self.commands.pop_front()
    }

    pub(crate) fn queued_sessions(&self) -> Vec<SessionId> {
        self.prompts
            .iter()
            .filter_map(|(session_id, queue)| {
                (!queue.paused
                    && !queue.prompts.is_empty()
                    && !self.inflight_turns.contains_key(session_id))
                .then_some(*session_id)
            })
            .collect()
    }

    pub(crate) fn front_prompt(&self, session_id: SessionId) -> Option<NonEmptyText> {
        self.prompts.get(&session_id)?.prompts.front().cloned()
    }

    pub(crate) fn pop_prompt(&mut self, session_id: SessionId) -> Option<NonEmptyText> {
        let queue = self.prompts.get_mut(&session_id)?;
        let prompt = queue.prompts.pop_front()?;
        queue.bytes = queue.bytes.saturating_sub(prompt.as_str().len());
        Some(prompt)
    }

    pub(crate) fn mark_turn_inflight(&mut self, session_id: SessionId) {
        self.inflight_turns.insert(session_id, false);
    }

    pub(crate) fn inflight_sessions(&self) -> Vec<SessionId> {
        self.inflight_turns.keys().copied().collect()
    }

    pub(crate) fn observe_turn(&mut self, session_id: SessionId, active: bool) {
        let Some(observed_active) = self.inflight_turns.get_mut(&session_id) else {
            return;
        };
        if active {
            *observed_active = true;
        } else if *observed_active {
            self.inflight_turns.remove(&session_id);
        }
    }

    pub(crate) fn clear_turn_inflight(&mut self, session_id: SessionId) {
        self.inflight_turns.remove(&session_id);
    }

    pub(crate) fn pause_prompts(&mut self, session_id: SessionId) {
        self.prompts
            .entry(session_id)
            .or_insert_with(PromptQueue::new)
            .paused = true;
    }

    pub(crate) fn clear_all_inflight(&mut self) {
        self.inflight_turns.clear();
    }

    pub(crate) fn defaults(&self, session_id: SessionId) -> TurnDefaults {
        self.defaults.get(&session_id).cloned().unwrap_or_default()
    }

    pub(crate) fn defaults_mut(&mut self, session_id: SessionId) -> &mut TurnDefaults {
        self.defaults.entry(session_id).or_default()
    }

    pub(crate) fn queue_summary(&self, session_id: SessionId) -> (usize, usize, bool) {
        self.prompts
            .get(&session_id)
            .map(|queue| (queue.prompts.len(), queue.bytes, queue.paused))
            .unwrap_or((0, 0, false))
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use agentpulse_core::{ChannelId, CommandId, Timestamp};

    fn command(session_id: SessionId, payload: AgentCommandPayload) -> AgentCommand {
        AgentCommand::new(
            CommandId::new(),
            session_id,
            ChannelId::new(),
            Timestamp::now_utc(),
            payload,
        )
    }

    #[test]
    fn prompt_queue_is_fifo_bounded_and_stop_preserves_it_paused() -> Result<(), Box<dyn Error>> {
        let session_id = SessionId::new();
        let mut state = ControlRuntimeState::new();
        for index in 0..MAX_PROMPTS_PER_SESSION {
            state.accept(command(
                session_id,
                AgentCommandPayload::SubmitPrompt {
                    text: NonEmptyText::new(format!("prompt {index}"))?,
                    delivery: PromptDelivery::Queue,
                },
            ))?;
        }
        assert!(matches!(
            state.accept(command(
                session_id,
                AgentCommandPayload::SubmitPrompt {
                    text: NonEmptyText::new("overflow")?,
                    delivery: PromptDelivery::Queue,
                },
            )),
            Err(CodexProviderPortError::PromptQueueFull { .. })
        ));
        state.accept(command(
            session_id,
            AgentCommandPayload::CancelSession { reason: None },
        ))?;
        assert!(state.queued_sessions().is_empty());
        assert_eq!(state.queue_summary(session_id).0, MAX_PROMPTS_PER_SESSION);
        state.accept(command(
            session_id,
            AgentCommandPayload::Queue {
                action: QueueAction::Resume,
            },
        ))?;
        assert_eq!(state.queued_sessions(), vec![session_id]);
        assert_eq!(
            state
                .pop_prompt(session_id)
                .ok_or("first prompt missing")?
                .as_str(),
            "prompt 0"
        );

        let mut aggregate = ControlRuntimeState::new();
        for _ in 0..(MAX_QUEUED_PROMPT_BYTES / MAX_PROMPT_BYTES) {
            aggregate.accept(command(
                SessionId::new(),
                AgentCommandPayload::SubmitPrompt {
                    text: NonEmptyText::new("x".repeat(MAX_PROMPT_BYTES))?,
                    delivery: PromptDelivery::Queue,
                },
            ))?;
        }
        assert!(matches!(
            aggregate.accept(command(
                SessionId::new(),
                AgentCommandPayload::SubmitPrompt {
                    text: NonEmptyText::new("x")?,
                    delivery: PromptDelivery::Queue,
                },
            )),
            Err(CodexProviderPortError::PromptQueueFull { .. })
        ));
        Ok(())
    }
}
