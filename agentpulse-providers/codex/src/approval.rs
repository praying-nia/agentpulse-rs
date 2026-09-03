//! Runtime-only Codex approval presentation, correlation, and outbound queue.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use agentpulse_core::{
    ApprovalCommandKind, ApprovalDisposition, ApprovalFileChange, ApprovalFileChangeKind,
    ApprovalNetworkContext, ApprovalOption, ApprovalOptionId, ApprovalRequest, ApprovalSubject,
    InteractionId, InteractionRequest, InteractionRequestPayload, InteractionResponse,
    InteractionResponsePayload, NonEmptyText, SessionId, Timestamp,
};
use serde_json::{Value, json};

use crate::{CodexProviderPortError, CodexProviderSourceError, protocol::RequestId};

pub(crate) const MAX_PENDING_APPROVALS: usize = 64;
pub(crate) const MAX_APPROVAL_PRESENTATION_BYTES: usize = 256 * 1024;

pub(crate) type SharedApprovalState = Arc<Mutex<ApprovalRuntimeState>>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ApprovalRoute {
    Observer,
    Proxy(u64),
}

#[derive(Clone)]
pub(crate) struct OutboundApproval {
    pub(crate) route: ApprovalRoute,
    pub(crate) interaction_id: InteractionId,
    pub(crate) request_id: RequestId,
    pub(crate) method: String,
    pub(crate) decision: Value,
}

#[derive(Clone)]
enum PendingStatus {
    Awaiting,
    Queued(InteractionResponse),
    Sent(InteractionResponse),
}

struct PendingApproval {
    route: ApprovalRoute,
    request_id: RequestId,
    method: String,
    thread_id: String,
    turn_id: String,
    item_id: String,
    session_id: SessionId,
    decisions: BTreeMap<ApprovalOptionId, Value>,
    status: PendingStatus,
}

pub(crate) enum ResolvedApproval {
    Responded {
        thread_id: String,
        response: InteractionResponse,
    },
    Closed {
        thread_id: String,
        session_id: SessionId,
        interaction_id: InteractionId,
    },
}

pub(crate) struct ClosedApproval {
    pub(crate) thread_id: String,
    pub(crate) session_id: SessionId,
    pub(crate) interaction_id: InteractionId,
}

pub(crate) struct ApprovalRuntimeState {
    pending: BTreeMap<InteractionId, PendingApproval>,
    by_request: BTreeMap<(ApprovalRoute, RequestId), InteractionId>,
    outbound: VecDeque<OutboundApproval>,
}

impl ApprovalRuntimeState {
    pub(crate) fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
            by_request: BTreeMap::new(),
            outbound: VecDeque::with_capacity(MAX_PENDING_APPROVALS),
        }
    }

    pub(crate) fn register(
        &mut self,
        route: ApprovalRoute,
        request_id: RequestId,
        method: String,
        prepared: &PreparedApproval,
    ) -> Result<(), CodexProviderSourceError> {
        if self.pending.len() >= MAX_PENDING_APPROVALS {
            return Err(CodexProviderSourceError::protocol(format!(
                "pending approval limit of {MAX_PENDING_APPROVALS} reached"
            )));
        }
        let request_key = (route, request_id.clone());
        if self.by_request.contains_key(&request_key) {
            return Err(CodexProviderSourceError::protocol(
                "Codex reused a pending server request ID",
            ));
        }
        let interaction_id = prepared.request.id();
        if self.pending.contains_key(&interaction_id) {
            return Err(CodexProviderSourceError::protocol(
                "generated interaction ID is already pending",
            ));
        }
        self.by_request.insert(request_key, interaction_id);
        self.pending.insert(
            interaction_id,
            PendingApproval {
                route,
                request_id,
                method,
                thread_id: prepared.thread_id.clone(),
                turn_id: prepared.turn_id.clone(),
                item_id: prepared.item_id.clone(),
                session_id: prepared.request.session_id(),
                decisions: prepared.decisions.clone(),
                status: PendingStatus::Awaiting,
            },
        );
        Ok(())
    }

    pub(crate) fn remove(&mut self, interaction_id: InteractionId) {
        if let Some(pending) = self.pending.remove(&interaction_id) {
            self.by_request.remove(&(pending.route, pending.request_id));
        }
        self.outbound
            .retain(|outbound| outbound.interaction_id != interaction_id);
    }

    pub(crate) fn claim(
        &mut self,
        response: InteractionResponse,
    ) -> Result<(), CodexProviderPortError> {
        let interaction_id = response.request_id();
        let pending = self
            .pending
            .get_mut(&interaction_id)
            .ok_or(CodexProviderPortError::InteractionNotPending { interaction_id })?;
        if response.session_id() != pending.session_id {
            return Err(CodexProviderPortError::SessionMismatch {
                expected: pending.session_id,
                actual: response.session_id(),
            });
        }
        if !matches!(pending.status, PendingStatus::Awaiting) {
            return Err(CodexProviderPortError::InteractionAlreadyClaimed { interaction_id });
        }
        let option_id = match response.payload() {
            InteractionResponsePayload::Approval(selection) => selection.option_id(),
            _ => return Err(CodexProviderPortError::UnsupportedInteractionResponse),
        };
        let decision = pending.decisions.get(&option_id).cloned().ok_or(
            CodexProviderPortError::UnknownApprovalOption {
                interaction_id,
                option_id,
            },
        )?;
        if self.outbound.len() >= MAX_PENDING_APPROVALS {
            return Err(CodexProviderPortError::OutboundQueueFull {
                capacity: MAX_PENDING_APPROVALS,
            });
        }
        pending.status = PendingStatus::Queued(response);
        self.outbound.push_back(OutboundApproval {
            route: pending.route,
            interaction_id,
            request_id: pending.request_id.clone(),
            method: pending.method.clone(),
            decision,
        });
        Ok(())
    }

    pub(crate) fn pop_outbound_for(&mut self, route: ApprovalRoute) -> Option<OutboundApproval> {
        let index = self
            .outbound
            .iter()
            .position(|outbound| outbound.route == route)?;
        self.outbound.remove(index)
    }

    pub(crate) fn mark_sent(
        &mut self,
        interaction_id: InteractionId,
    ) -> Result<(), CodexProviderSourceError> {
        let pending = self.pending.get_mut(&interaction_id).ok_or_else(|| {
            CodexProviderSourceError::protocol("queued approval disappeared before write")
        })?;
        let PendingStatus::Queued(response) = &pending.status else {
            return Err(CodexProviderSourceError::protocol(
                "approval was not queued when its response was written",
            ));
        };
        pending.status = PendingStatus::Sent(response.clone());
        Ok(())
    }

    pub(crate) fn resolve(
        &mut self,
        route: ApprovalRoute,
        request_id: &RequestId,
        thread_id: &str,
    ) -> Result<Option<ResolvedApproval>, CodexProviderSourceError> {
        let request_key = (route, request_id.clone());
        let Some(interaction_id) = self.by_request.get(&request_key).copied() else {
            return Ok(None);
        };
        let pending = self.pending.get(&interaction_id).ok_or_else(|| {
            CodexProviderSourceError::protocol("approval request index was inconsistent")
        })?;
        if pending.thread_id != thread_id {
            return Err(CodexProviderSourceError::protocol(
                "serverRequest/resolved thread did not match the pending approval",
            ));
        }
        self.by_request.remove(&request_key);
        let pending = self.pending.remove(&interaction_id).ok_or_else(|| {
            CodexProviderSourceError::protocol("approval request disappeared during resolution")
        })?;
        self.outbound
            .retain(|outbound| outbound.interaction_id != interaction_id);
        Ok(Some(match pending.status {
            PendingStatus::Sent(response) => ResolvedApproval::Responded {
                thread_id: pending.thread_id,
                response,
            },
            PendingStatus::Awaiting | PendingStatus::Queued(_) => ResolvedApproval::Closed {
                thread_id: pending.thread_id,
                session_id: pending.session_id,
                interaction_id,
            },
        }))
    }

    pub(crate) fn close_item(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
    ) -> Vec<ClosedApproval> {
        self.close_matching(|pending| {
            pending.thread_id == thread_id
                && pending.turn_id == turn_id
                && pending.item_id == item_id
        })
    }

    pub(crate) fn close_turn(&mut self, thread_id: &str, turn_id: &str) -> Vec<ClosedApproval> {
        self.close_matching(|pending| pending.thread_id == thread_id && pending.turn_id == turn_id)
    }

    pub(crate) fn close_thread(&mut self, thread_id: &str) -> Vec<ClosedApproval> {
        self.close_matching(|pending| pending.thread_id == thread_id)
    }

    pub(crate) fn close_all(&mut self) -> Vec<ClosedApproval> {
        self.close_matching(|_| true)
    }

    pub(crate) fn close_route(&mut self, route: ApprovalRoute) -> Vec<ClosedApproval> {
        self.close_matching(|pending| pending.route == route)
    }

    fn close_matching(
        &mut self,
        matches: impl Fn(&PendingApproval) -> bool,
    ) -> Vec<ClosedApproval> {
        let identifiers = self
            .pending
            .iter()
            .filter_map(|(interaction_id, pending)| matches(pending).then_some(*interaction_id))
            .collect::<Vec<_>>();
        let mut closed = Vec::with_capacity(identifiers.len());
        for interaction_id in identifiers {
            if let Some(pending) = self.pending.remove(&interaction_id) {
                self.by_request.remove(&(pending.route, pending.request_id));
                closed.push(ClosedApproval {
                    thread_id: pending.thread_id,
                    session_id: pending.session_id,
                    interaction_id,
                });
            }
        }
        self.outbound.retain(|outbound| {
            !closed
                .iter()
                .any(|entry| entry.interaction_id == outbound.interaction_id)
        });
        closed
    }
}

pub(crate) struct PreparedApproval {
    pub(crate) request: InteractionRequest,
    pub(crate) decisions: BTreeMap<ApprovalOptionId, Value>,
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) item_id: String,
}

pub(crate) fn prepare_approval(
    method: &str,
    params: &Value,
    item: Option<&Value>,
    session_id: SessionId,
) -> Result<PreparedApproval, CodexProviderSourceError> {
    let thread_id = required_string(params, "threadId")?.to_owned();
    let turn_id = required_string(params, "turnId")?.to_owned();
    let item_id = required_string(params, "itemId")?.to_owned();
    let requested_at = timestamp_milliseconds(required_i64(params, "startedAtMs")?)?;
    let (subject, mut unavailable_reason) = match method {
        "item/commandExecution/requestApproval" => command_subject(params, item)?,
        "item/fileChange/requestApproval" => file_subject(params, item)?,
        _ => {
            return Err(CodexProviderSourceError::protocol(format!(
                "unsupported approval method {method}"
            )));
        }
    };
    if presentation_bytes(&subject) > MAX_APPROVAL_PRESENTATION_BYTES {
        unavailable_reason = Some(oversized_reason()?);
    }

    let (options, decisions) = if unavailable_reason.is_none() {
        let (options, decisions) = approval_options(method, params, &subject)?;
        if approval_presentation_bytes(&subject, &options) > MAX_APPROVAL_PRESENTATION_BYTES {
            unavailable_reason = Some(oversized_reason()?);
            (Vec::new(), BTreeMap::new())
        } else {
            (options, decisions)
        }
    } else {
        (Vec::new(), BTreeMap::new())
    };
    let approval = if let Some(reason) = unavailable_reason {
        ApprovalRequest::unavailable(redacted_subject(subject), reason)
    } else {
        ApprovalRequest::actionable(subject, options)?
    };
    let prompt = match method {
        "item/commandExecution/requestApproval" => "Command approval required",
        "item/fileChange/requestApproval" => "File change approval required",
        _ => {
            return Err(CodexProviderSourceError::protocol(format!(
                "unsupported approval method {method}"
            )));
        }
    };
    let request = InteractionRequest::new(
        InteractionId::new(),
        session_id,
        requested_at,
        NonEmptyText::new(prompt)?,
        InteractionRequestPayload::Approval(approval),
    );
    Ok(PreparedApproval {
        request,
        decisions,
        thread_id,
        turn_id,
        item_id,
    })
}

fn command_subject(
    params: &Value,
    item: Option<&Value>,
) -> Result<(ApprovalSubject, Option<NonEmptyText>), CodexProviderSourceError> {
    let item =
        item.filter(|item| item.get("type").and_then(Value::as_str) == Some("commandExecution"));
    let kind = match params
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("command")
    {
        "command" => ApprovalCommandKind::Command,
        "writeStdin" => ApprovalCommandKind::WriteStdin,
        other => {
            return Err(CodexProviderSourceError::protocol(format!(
                "unsupported command approval kind {other}"
            )));
        }
    };
    let command = optional_text(params, "command")?
        .or_else(|| item.and_then(|value| optional_text(value, "command").ok().flatten()));
    let cwd = optional_text(params, "cwd")?
        .or_else(|| item.and_then(|value| optional_text(value, "cwd").ok().flatten()));
    let reason = optional_text(params, "reason")?;
    let network = match params.get("networkApprovalContext") {
        None | Some(Value::Null) => None,
        Some(value) => Some(ApprovalNetworkContext::new(
            NonEmptyText::new(required_string(value, "host")?.to_owned())?,
            NonEmptyText::new(required_string(value, "protocol")?.to_owned())?,
        )),
    };
    let unavailable = if command.is_none() && network.is_none() {
        Some(NonEmptyText::new(
            "Codex did not provide command or network details; use the desktop Codex client",
        )?)
    } else {
        None
    };
    Ok((
        ApprovalSubject::Command {
            kind,
            command,
            cwd,
            reason,
            network,
        },
        unavailable,
    ))
}

fn file_subject(
    params: &Value,
    item: Option<&Value>,
) -> Result<(ApprovalSubject, Option<NonEmptyText>), CodexProviderSourceError> {
    let reason = optional_text(params, "reason")?;
    let grant_root = optional_text(params, "grantRoot")?;
    let mut changes = Vec::new();
    let valid_item =
        item.filter(|item| item.get("type").and_then(Value::as_str) == Some("fileChange"));
    if let Some(values) = valid_item
        .and_then(|item| item.get("changes"))
        .and_then(Value::as_array)
    {
        for value in values {
            let kind = match required_string(value, "kind")? {
                "add" => ApprovalFileChangeKind::Add,
                "delete" => ApprovalFileChangeKind::Delete,
                "update" => ApprovalFileChangeKind::Update,
                other => {
                    return Err(CodexProviderSourceError::protocol(format!(
                        "unsupported file change kind {other}"
                    )));
                }
            };
            changes.push(ApprovalFileChange::new(
                NonEmptyText::new(required_string(value, "path")?.to_owned())?,
                kind,
                required_string(value, "diff")?.to_owned(),
            ));
        }
    }
    let unavailable = if changes.is_empty() {
        Some(NonEmptyText::new(
            "Codex file-change details were not available; use the desktop Codex client",
        )?)
    } else {
        None
    };
    Ok((
        ApprovalSubject::FileChange {
            changes,
            grant_root,
            reason,
        },
        unavailable,
    ))
}

fn approval_options(
    method: &str,
    params: &Value,
    subject: &ApprovalSubject,
) -> Result<(Vec<ApprovalOption>, BTreeMap<ApprovalOptionId, Value>), CodexProviderSourceError> {
    let mut options = Vec::new();
    let mut decisions = BTreeMap::new();
    add_option(
        &mut options,
        &mut decisions,
        ApprovalDisposition::Approve,
        "Approve once",
        "Allow only this operation",
        json!("accept"),
    )?;
    add_option(
        &mut options,
        &mut decisions,
        ApprovalDisposition::Approve,
        "Approve for session",
        match subject {
            ApprovalSubject::FileChange {
                grant_root: Some(root),
                ..
            } => {
                format!("Allow this change and future writes under {root} for this Codex session")
            }
            ApprovalSubject::FileChange { .. } => {
                "Allow this change and future changes to the same files for this Codex session"
                    .to_owned()
            }
            _ => {
                "Allow this operation and matching future prompts for this Codex session".to_owned()
            }
        },
        json!("acceptForSession"),
    )?;

    if method == "item/commandExecution/requestApproval" {
        if let Some(amendment) = params
            .get("proposedExecpolicyAmendment")
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty())
        {
            let exact = amendment
                .iter()
                .map(|value| value.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(" ");
            add_option(
                &mut options,
                &mut decisions,
                ApprovalDisposition::Approve,
                "Approve and remember command rule",
                format!("Apply Codex exec policy amendment: {exact}"),
                json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": amendment}}),
            )?;
        }
        if let Some(amendments) = params
            .get("proposedNetworkPolicyAmendments")
            .and_then(Value::as_array)
        {
            for amendment in amendments {
                let action = required_string(amendment, "action")?;
                let host = required_string(amendment, "host")?;
                let (disposition, label) = match action {
                    "allow" => (ApprovalDisposition::Approve, format!("Always allow {host}")),
                    "deny" => (ApprovalDisposition::Reject, format!("Always deny {host}")),
                    other => {
                        return Err(CodexProviderSourceError::protocol(format!(
                            "unsupported network policy action {other}"
                        )));
                    }
                };
                add_option(
                    &mut options,
                    &mut decisions,
                    disposition,
                    label,
                    format!("Apply persistent Codex network policy for {host}"),
                    json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": amendment}}),
                )?;
            }
        }
    }

    add_option(
        &mut options,
        &mut decisions,
        ApprovalDisposition::Reject,
        "Reject",
        "Deny this operation and continue the turn",
        json!("decline"),
    )?;
    add_option(
        &mut options,
        &mut decisions,
        ApprovalDisposition::Cancel,
        "Reject and stop",
        "Deny this operation and immediately interrupt the turn",
        json!("cancel"),
    )?;
    Ok((options, decisions))
}

fn add_option(
    options: &mut Vec<ApprovalOption>,
    decisions: &mut BTreeMap<ApprovalOptionId, Value>,
    disposition: ApprovalDisposition,
    label: impl Into<String>,
    description: impl Into<String>,
    decision: Value,
) -> Result<(), CodexProviderSourceError> {
    let id = ApprovalOptionId::new();
    options.push(
        ApprovalOption::new(id, disposition, NonEmptyText::new(label.into())?)
            .with_description(NonEmptyText::new(description.into())?),
    );
    decisions.insert(id, decision);
    Ok(())
}

fn presentation_bytes(subject: &ApprovalSubject) -> usize {
    match subject {
        ApprovalSubject::Command {
            command,
            cwd,
            reason,
            network,
            ..
        } => {
            command.as_ref().map_or(0, |value| value.as_str().len())
                + cwd.as_ref().map_or(0, |value| value.as_str().len())
                + reason.as_ref().map_or(0, |value| value.as_str().len())
                + network.as_ref().map_or(0, |value| {
                    value.host().as_str().len() + value.protocol().as_str().len()
                })
        }
        ApprovalSubject::FileChange {
            changes,
            grant_root,
            reason,
        } => {
            changes.iter().fold(0usize, |total, change| {
                total
                    .saturating_add(change.path().as_str().len())
                    .saturating_add(change.diff().len())
            }) + grant_root.as_ref().map_or(0, |value| value.as_str().len())
                + reason.as_ref().map_or(0, |value| value.as_str().len())
        }
        _ => usize::MAX,
    }
}

fn approval_presentation_bytes(subject: &ApprovalSubject, options: &[ApprovalOption]) -> usize {
    options
        .iter()
        .fold(presentation_bytes(subject), |total, option| {
            total
                .saturating_add(option.label().as_str().len())
                .saturating_add(
                    option
                        .description()
                        .map_or(0, |description| description.as_str().len()),
                )
        })
}

fn oversized_reason() -> Result<NonEmptyText, CodexProviderSourceError> {
    NonEmptyText::new(format!(
        "Approval details exceed the {MAX_APPROVAL_PRESENTATION_BYTES}-byte display limit; use the desktop Codex client"
    ))
    .map_err(Into::into)
}

fn redacted_subject(subject: ApprovalSubject) -> ApprovalSubject {
    match subject {
        ApprovalSubject::Command { kind, .. } => ApprovalSubject::Command {
            kind,
            command: None,
            cwd: None,
            reason: None,
            network: None,
        },
        ApprovalSubject::FileChange { .. } => ApprovalSubject::FileChange {
            changes: Vec::new(),
            grant_root: None,
            reason: None,
        },
        other => other,
    }
}

fn optional_text(
    value: &Value,
    field: &'static str,
) -> Result<Option<NonEmptyText>, CodexProviderSourceError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(NonEmptyText::new(text.clone())?)),
        Some(_) => Err(CodexProviderSourceError::protocol(format!(
            "{field} must be a string or null"
        ))),
    }
}

fn required_string<'a>(
    value: &'a Value,
    field: &'static str,
) -> Result<&'a str, CodexProviderSourceError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| CodexProviderSourceError::protocol(format!("{field} must be a string")))
}

fn required_i64(value: &Value, field: &'static str) -> Result<i64, CodexProviderSourceError> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| CodexProviderSourceError::protocol(format!("{field} must be an int64")))
}

fn timestamp_milliseconds(value: i64) -> Result<Timestamp, CodexProviderSourceError> {
    let nanos = i128::from(value)
        .checked_mul(1_000_000)
        .ok_or_else(|| CodexProviderSourceError::protocol("approval timestamp overflowed"))?;
    Timestamp::from_unix_timestamp_nanos(nanos).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use agentpulse_core::{ApprovalSelection, ChannelId, InteractionResponsePayload};

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    fn command_params() -> Value {
        json!({
            "threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6",
            "turnId": "019976a4-00f1-76c0-b845-e1509dc4e3de",
            "itemId": "019976a4-00f2-741b-870f-21b4fb983746",
            "startedAtMs": 1_000,
            "command": "cargo test --workspace",
            "cwd": "/workspace",
            "reason": "Run tests",
            "proposedExecpolicyAmendment": ["cargo", "test"],
            "proposedNetworkPolicyAmendments": [
                {"action": "allow", "host": "crates.io"},
                {"action": "deny", "host": "example.invalid"}
            ]
        })
    }

    #[test]
    fn command_approval_exposes_every_exact_codex_decision() -> TestResult {
        let prepared = prepare_approval(
            "item/commandExecution/requestApproval",
            &command_params(),
            None,
            SessionId::new(),
        )?;
        assert!(prepared.request.expires_at().is_none());
        let InteractionRequestPayload::Approval(approval) = prepared.request.payload() else {
            return Err("expected approval request".into());
        };
        assert!(approval.is_actionable());
        assert_eq!(approval.options().len(), 7);
        assert_eq!(prepared.decisions.len(), approval.options().len());
        let labels = approval
            .options()
            .iter()
            .map(|option| option.label().as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"Approve and remember command rule"));
        assert!(labels.contains(&"Always allow crates.io"));
        assert!(labels.contains(&"Always deny example.invalid"));
        assert!(approval.options().iter().any(|option| {
            option.label().as_str() == "Always deny example.invalid"
                && option.disposition() == ApprovalDisposition::Reject
        }));
        for decision in [
            json!("accept"),
            json!("acceptForSession"),
            json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["cargo", "test"]}}),
            json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": {"action": "allow", "host": "crates.io"}}}),
            json!({"applyNetworkPolicyAmendment": {"network_policy_amendment": {"action": "deny", "host": "example.invalid"}}}),
            json!("decline"),
            json!("cancel"),
        ] {
            assert!(
                prepared
                    .decisions
                    .values()
                    .any(|actual| actual == &decision)
            );
        }
        Ok(())
    }

    #[test]
    fn file_approval_preserves_exact_paths_and_diffs() -> TestResult {
        let params = json!({
            "threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6",
            "turnId": "019976a4-00f1-76c0-b845-e1509dc4e3de",
            "itemId": "019976a4-00f2-741b-870f-21b4fb983746",
            "startedAtMs": 1_000,
            "grantRoot": "/workspace",
            "reason": "Apply fix"
        });
        let item = json!({
            "id": "019976a4-00f2-741b-870f-21b4fb983746",
            "type": "fileChange",
            "changes": [{
                "path": "src/main.rs",
                "kind": "update",
                "diff": "@@ -1 +1 @@\n-old\n+new\n"
            }]
        });
        let prepared = prepare_approval(
            "item/fileChange/requestApproval",
            &params,
            Some(&item),
            SessionId::new(),
        )?;
        let InteractionRequestPayload::Approval(approval) = prepared.request.payload() else {
            return Err("expected approval request".into());
        };
        let ApprovalSubject::FileChange { changes, .. } = approval.subject() else {
            return Err("expected file-change subject".into());
        };
        assert_eq!(changes[0].path().as_str(), "src/main.rs");
        assert_eq!(changes[0].diff(), "@@ -1 +1 @@\n-old\n+new\n");
        Ok(())
    }

    #[test]
    fn response_is_claimed_once_and_only_resolves_after_codex_confirmation() -> TestResult {
        let prepared = prepare_approval(
            "item/commandExecution/requestApproval",
            &command_params(),
            None,
            SessionId::new(),
        )?;
        let interaction_id = prepared.request.id();
        let session_id = prepared.request.session_id();
        let option_id = *prepared
            .decisions
            .keys()
            .next()
            .ok_or("approval had no options")?;
        let request_id = RequestId::String("approval-1".to_owned());
        let route = ApprovalRoute::Proxy(7);
        let mut state = ApprovalRuntimeState::new();
        state.register(
            route,
            request_id.clone(),
            "item/commandExecution/requestApproval".to_owned(),
            &prepared,
        )?;
        let response = InteractionResponse::new(
            interaction_id,
            session_id,
            ChannelId::new(),
            Timestamp::now_utc(),
            InteractionResponsePayload::Approval(ApprovalSelection::new(option_id)),
        );
        state.claim(response.clone())?;
        assert!(matches!(
            state.claim(response.clone()),
            Err(CodexProviderPortError::InteractionAlreadyClaimed { .. })
        ));
        assert!(state.pop_outbound_for(ApprovalRoute::Observer).is_none());
        let outbound = state
            .pop_outbound_for(route)
            .ok_or("response was not queued")?;
        assert_eq!(outbound.interaction_id, interaction_id);
        assert!(state.pending.contains_key(&interaction_id));
        state.mark_sent(interaction_id)?;
        assert!(
            state
                .resolve(
                    ApprovalRoute::Observer,
                    &request_id,
                    "019976a4-00f0-7312-b36c-d01f9c5c06f6"
                )?
                .is_none()
        );
        let resolved = state.resolve(route, &request_id, "019976a4-00f0-7312-b36c-d01f9c5c06f6")?;
        assert!(matches!(
            resolved,
            Some(ResolvedApproval::Responded { response: actual, .. }) if actual == response
        ));
        Ok(())
    }

    #[test]
    fn oversized_approval_is_explicitly_read_only_and_redacted() -> TestResult {
        let mut oversized_subject = command_params();
        oversized_subject["command"] =
            Value::String("x".repeat(MAX_APPROVAL_PRESENTATION_BYTES + 1));
        let mut oversized_options = command_params();
        oversized_options["proposedNetworkPolicyAmendments"] = json!([{
            "action": "allow",
            "host": "x".repeat(MAX_APPROVAL_PRESENTATION_BYTES + 1)
        }]);

        for params in [oversized_subject, oversized_options] {
            let prepared = prepare_approval(
                "item/commandExecution/requestApproval",
                &params,
                None,
                SessionId::new(),
            )?;
            let InteractionRequestPayload::Approval(approval) = prepared.request.payload() else {
                return Err("expected approval request".into());
            };
            assert!(!approval.is_actionable());
            assert!(approval.unavailable_reason().is_some());
            assert!(prepared.decisions.is_empty());
            let ApprovalSubject::Command {
                command, network, ..
            } = approval.subject()
            else {
                return Err("expected command subject".into());
            };
            assert!(command.is_none());
            assert!(network.is_none());
        }
        Ok(())
    }
}
