//! Drives Host-owned forms on the observer worker, outside Bridge callbacks.
use super::*;
use crate::local_interaction::{LocalAction, LocalChoice};

pub(super) fn dismiss_plan(
    thread: &str,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    let mut mapper = lock_mapper(mapper);
    let waiting = mapper.has_plan(thread);
    mapper.dismiss_plan(thread);
    let ids = mapper
        .local_choices
        .iter()
        .filter(|(_, (owner, plan))| owner == thread && *plan)
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    for id in ids {
        mapper.close_local_choice(id, events, status)?;
    }
    if waiting {
        mapper.publish_payload(
            thread,
            Timestamp::now_utc(),
            AgentEventPayload::StateChanged(AgentState::Idle),
            events,
            status,
        )?;
    }
    Ok(())
}

pub(super) fn report(
    session_id: SessionId,
    text: impl Into<String>,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    let context = lock_mapper(mapper)
        .command_context(session_id)
        .map(|(thread, turn, state)| (thread.to_owned(), turn.map(str::to_owned), state));
    publish_system_for_session(context.as_ref(), text, mapper, events, status)
}

fn show(
    choice: LocalChoice,
    controls: &SharedControlState,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    let old = {
        lock_controls(controls)
            .local
            .pending
            .iter()
            .filter(|(_, old)| {
                old.request.session_id() == choice.request.session_id() && old.plan == choice.plan
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
    };
    for id in old {
        lock_mapper(mapper).close_local_choice(id, events, status)?;
        lock_controls(controls).local.pending.remove(&id);
    }
    let (id, thread, request, plan) = (
        choice.request.id(),
        choice.thread_id.clone(),
        choice.request.clone(),
        choice.plan,
    );
    // Register before publication so an immediate phone response can be claimed.
    if lock_controls(controls).local.pending.len() >= 64 {
        return report(
            request.session_id(),
            "待处理选择过多，请先完成或取消已有选择。",
            mapper,
            events,
            status,
        );
    }
    lock_controls(controls).local.pending.insert(id, choice);
    if let Err(error) =
        lock_mapper(mapper).publish_local_choice(&thread, request, plan, events, status)
    {
        lock_controls(controls).local.pending.remove(&id);
        return Err(error);
    }
    Ok(())
}

pub(super) fn show_models(
    session: SessionId,
    catalog: &serde_json::Value,
    controls: &SharedControlState,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    events: &ProviderEventHandle,
    status: &SharedStatus,
) -> Result<(), CodexProviderSourceError> {
    let Some(thread) = lock_mapper(mapper)
        .command_context(session)
        .map(|v| v.0.to_owned())
    else {
        return Ok(());
    };
    let explicit = { lock_controls(controls).defaults(session).model };
    let current = explicit.or_else(|| {
        lock_mapper(mapper)
            .model_settings_for_session(session)
            .map(|v| v.0.to_owned())
    });
    let mut options = Vec::new();
    for model in catalog["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|model| model["hidden"] != true)
    {
        let Some(id) = model["id"].as_str() else {
            continue;
        };
        let name = model["displayName"].as_str().unwrap_or(id);
        let selected = current
            .as_deref()
            .is_some_and(|value| value == id || Some(value) == model["model"].as_str());
        options.push((
            format!("{name}{}", if selected { "（当前）" } else { "" }),
            model["description"].as_str().map(str::to_owned),
            LocalAction::Model(model.clone()),
        ));
    }
    if options.is_empty() {
        return report(
            session,
            "暂无可用模型，请稍后重试 /model",
            mapper,
            events,
            status,
        );
    }
    options.push(("取消".to_owned(), None, LocalAction::Cancel));
    show(
        LocalChoice::new(
            session,
            thread,
            "选择模型",
            "第 1 步：选择模型，下一步选择推理强度。",
            false,
            options,
        )?,
        controls,
        mapper,
        events,
        status,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn flush_local_choices(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    mapper: &Arc<Mutex<CodexEventMapper>>,
    status: &SharedStatus,
    controls: &SharedControlState,
    events: &ProviderEventHandle,
    pending: &mut BTreeMap<crate::protocol::RequestId, PendingControl>,
) -> Result<(), CodexProviderSourceError> {
    let live = { lock_mapper(mapper).local_choices.clone() };
    let revisions = { std::mem::take(&mut lock_controls(controls).revise_requested) };
    for session in revisions {
        let thread = lock_mapper(mapper)
            .command_context(session)
            .map(|v| v.0.to_owned());
        if let Some(thread) = thread {
            if !lock_mapper(mapper).has_plan(&thread) {
                continue;
            }
            lock_controls(controls).defaults_mut(session).plan_mode = true;
            lock_mapper(mapper).dismiss_plan(&thread);
            for (id, (owner, plan)) in &live {
                if owner == &thread && *plan {
                    lock_mapper(mapper).close_local_choice(*id, events, status)?;
                }
            }
        }
    }
    loop {
        let next = { lock_controls(controls).local.responses.pop_front() };
        let Some((response, action)) = next else {
            break;
        };
        let id = response.request_id();
        let choice = { lock_controls(controls).local.pending.remove(&id) };
        let Some(choice) = choice else {
            continue;
        };
        let session = response.session_id();
        let thread = choice.thread_id;
        {
            let mut mapper = lock_mapper(mapper);
            // Check and publish under the same lock: a desktop lifecycle event
            // must not close the form between validation and response reduction.
            if !mapper.local_choices.contains_key(&id) {
                continue;
            }
            mapper.publish_payload(
                &thread,
                Timestamp::now_utc(),
                AgentEventPayload::InteractionResponded(response),
                events,
                status,
            )?;
            mapper.local_choices.remove(&id);
        }
        match action {
            LocalAction::Implement => {
                if !lock_mapper(mapper).has_plan(&thread) {
                    continue;
                }
                let mut defaults = lock_controls(controls).defaults(session);
                defaults.plan_mode = false;
                let settings = lock_mapper(mapper)
                    .model_settings_for_session(session)
                    .map(|(model, effort)| (model.to_owned(), effort.map(str::to_owned)));
                if settings.is_none() && defaults.model.is_none() {
                    report(
                        session,
                        "尚未获取当前模型，请稍后重试实施。",
                        mapper,
                        events,
                        status,
                    )?;
                    continue;
                }
                let mut params =
                    json!({"threadId": thread, "input": turn_input("Implement the plan.")});
                apply_turn_defaults(&mut params, &defaults, settings.as_ref())?;
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::TurnStart,
                    params,
                    PendingControl::ImplementPlan {
                        session_id: session,
                        thread_id: thread,
                    },
                    pending,
                )?;
                lock_controls(controls).mark_turn_inflight(session);
            }
            LocalAction::Revise => {
                lock_controls(controls).defaults_mut(session).plan_mode = true;
                lock_mapper(mapper).dismiss_plan(&thread);
                lock_mapper(mapper).publish_payload(
                    &thread,
                    Timestamp::now_utc(),
                    AgentEventPayload::StateChanged(AgentState::Idle),
                    events,
                    status,
                )?;
                report(
                    session,
                    "继续修改计划：请输入修改意见。",
                    mapper,
                    events,
                    status,
                )?;
            }
            LocalAction::Model(model) => {
                let id = model["id"].as_str().unwrap_or_default().to_owned();
                let default = model["defaultReasoningEffort"].as_str();
                let mut options = supported_reasoning_efforts(&model)
                    .into_iter()
                    .map(|effort| {
                        (
                            format!(
                                "使用 {effort}{}",
                                if Some(effort) == default {
                                    "（默认）"
                                } else {
                                    ""
                                }
                            ),
                            None,
                            LocalAction::SelectModel {
                                model: id.clone(),
                                effort: Some(effort.to_owned()),
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                if options.is_empty() {
                    options.push((
                        "使用模型默认值".to_owned(),
                        None,
                        LocalAction::SelectModel {
                            model: id.clone(),
                            effort: None,
                        },
                    ));
                }
                options.push(("返回模型列表".to_owned(), None, LocalAction::Models));
                options.push(("取消".to_owned(), None, LocalAction::Cancel));
                show(
                    LocalChoice::new(
                        session,
                        thread,
                        "选择推理强度",
                        &format!("第 2 步：{id}；点击强度确认切换。"),
                        false,
                        options,
                    )?,
                    controls,
                    mapper,
                    events,
                    status,
                )?;
            }
            LocalAction::SelectModel { model, effort } => {
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::ModelList,
                    json!({"limit": 50}),
                    PendingControl::SelectModel {
                        session_id: session,
                        model,
                        effort,
                        models: Vec::new(),
                        cursors: Vec::new(),
                    },
                    pending,
                )?;
            }
            LocalAction::Models => {
                send_control_request(
                    io,
                    protocol,
                    ExpectedResponse::ModelList,
                    json!({"limit": 50}),
                    PendingControl::Models {
                        session_id: session,
                        models: Vec::new(),
                        cursors: Vec::new(),
                    },
                    pending,
                )?;
            }
            LocalAction::Cancel => {
                report(
                    session,
                    "已取消模型选择，当前模型未改变。",
                    mapper,
                    events,
                    status,
                )?;
            }
        }
    }
    let live = { lock_mapper(mapper).local_choices.clone() };
    lock_controls(controls)
        .local
        .pending
        .retain(|id, _| live.contains_key(id));
    let plans = { lock_mapper(mapper).ready_plans() };
    for (session, thread) in plans {
        let exists = {
            lock_controls(controls)
                .local
                .pending
                .values()
                .any(|choice| choice.plan && choice.request.session_id() == session)
        };
        if exists
            || lock_controls(controls)
                .inflight_sessions()
                .contains(&session)
        {
            continue;
        }
        show(
            LocalChoice::new(
                session,
                thread,
                "计划已准备好",
                "请选择实施计划，或继续修改。",
                true,
                vec![
                    (
                        "实施计划".to_owned(),
                        Some("切换到执行模式并立即开始".to_owned()),
                        LocalAction::Implement,
                    ),
                    (
                        "继续修改".to_owned(),
                        Some("保留计划模式，输入修改意见".to_owned()),
                        LocalAction::Revise,
                    ),
                ],
            )?,
            controls,
            mapper,
            events,
            status,
        )?;
    }
    Ok(())
}
