use super::*;
use agentpulse_core::{
    AgentCommand, CommandId, FormAnswer, FormAnswerValue, FormResponse, InteractionRequest,
};

struct Harness {
    host: RuntimeHost,
    control: Arc<FakeControl>,
    handle: ChannelActionHandle,
    session: SessionId,
    channel: ChannelId,
}

impl Harness {
    fn new() -> Result<Self, Box<dyn Error>> {
        let control = Arc::new(FakeControl::default());
        seed_lines(&control, LIVE_FIXTURE.lines().take(2));
        let (_, provider, source, _) = test_provider(Arc::clone(&control))?;
        let session = SessionId::from_str(THREAD_ID)?;
        let channel = ChannelId::new();
        let actions = Arc::new(Mutex::new(None));
        let mut host = RuntimeHost::new();
        host.register_provider(provider, source)?;
        host.register_channel(
            AcceptingChannel {
                descriptor: ChannelDescriptor::new(
                    channel,
                    ChannelKind::new("test")?,
                    NonEmptyText::new("Local forms")?,
                    ChannelCapabilities::SESSION_VIEW
                        | ChannelCapabilities::REMOTE_COMMAND
                        | ChannelCapabilities::FORM_INPUT
                        | ChannelCapabilities::TEXT_INPUT,
                ),
            },
            CapturingChannelSource {
                handle: Arc::clone(&actions),
            },
        )?;
        host.start()?;
        host.subscribe(channel, session)?;
        let handle = locked(&actions).clone().ok_or("missing action handle")?;
        Ok(Self {
            host,
            control,
            handle,
            session,
            channel,
        })
    }

    fn command(&self, payload: AgentCommandPayload) -> TestResult {
        self.handle.submit_command(AgentCommand::new(
            CommandId::new(),
            self.session,
            self.channel,
            Timestamp::now_utc(),
            payload,
        ))?;
        Ok(())
    }

    fn form(&self, title: &str) -> Result<InteractionRequest, Box<dyn Error>> {
        let get = || {
            self.host.inspect_bridge(|bridge| {
                bridge
                    .session_aggregate(self.session)
                    .and_then(|aggregate| {
                        aggregate
                            .pending_interactions()
                            .find(|request| request.prompt().as_str() == title)
                            .cloned()
                    })
            })
        };
        wait_until(|| get().ok().flatten().is_some())?;
        get()?.ok_or_else(|| "missing form".into())
    }

    fn choose(&self, request: &InteractionRequest, label: &str) -> TestResult {
        let InteractionRequestPayload::Form(form) = request.payload() else {
            return Err("not a form".into());
        };
        let field = form.fields().first().ok_or("missing field")?;
        let option = field
            .options()
            .iter()
            .find(|option| option.label().as_str().contains(label))
            .ok_or("missing option")?;
        self.handle
            .submit_interaction_response(InteractionResponse::new(
                request.id(),
                self.session,
                self.channel,
                Timestamp::now_utc(),
                InteractionResponsePayload::Form(FormResponse::new(vec![FormAnswer::new(
                    field.id(),
                    FormAnswerValue::Choice(option.id()),
                )])?),
            ))?;
        Ok(())
    }

    fn outgoing(&self, method: &str, count: usize) -> Result<serde_json::Value, Box<dyn Error>> {
        let get = || {
            locked(&self.control.outgoing)
                .iter()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|frame| frame["method"] == method)
                .collect::<Vec<_>>()
        };
        wait_until(|| get().len() >= count)?;
        get()
            .get(count - 1)
            .cloned()
            .ok_or_else(|| "missing request".into())
    }

    fn plan(&self) -> TestResult {
        self.control
            .push_text(LIVE_FIXTURE.lines().nth(2).ok_or("missing turn")?);
        let mut item: serde_json::Value =
            serde_json::from_str(LIVE_FIXTURE.lines().nth(5).ok_or("missing item")?)?;
        item["params"]["item"] =
            json!({"id":"plan-1", "type":"plan", "text":"Create the requested file."});
        self.control.push_text(item.to_string());
        let end = LIVE_FIXTURE.lines().nth(6).ok_or("missing completion")?;
        self.control.push_text(end);
        self.control.push_text(end); // observer/proxy duplicate
        self.control.push_text(json!({"method":"thread/status/changed", "params":{"threadId":THREAD_ID,"status":{"type":"idle"}}}).to_string());
        Ok(())
    }
}

#[test]
fn proposed_plan_is_interactive_and_implementation_failure_can_retry() -> TestResult {
    let mut h = Harness::new()?;
    h.plan()?;
    let form = h.form("计划已准备好")?;
    assert_eq!(
        h.host
            .inspect_bridge(|b| b.session_aggregate(h.session).map(|s| s.session().state()))?,
        Some(AgentState::WaitingForInteraction)
    );
    h.choose(&form, "实施计划")?;
    assert!(h.choose(&form, "实施计划").is_err());
    let request = h.outgoing("turn/start", 1)?;
    assert_eq!(request["params"]["collaborationMode"]["mode"], "default");
    assert_eq!(request["params"]["input"][0]["text"], "Implement the plan.");
    h.control.push_text(
        json!({"id":request["id"], "error":{"code":-32602,"message":"Try again"}}).to_string(),
    );
    let retry = h.form("计划已准备好")?;
    assert_ne!(retry.id(), form.id());
    h.choose(&retry, "继续修改")?;
    h.command(AgentCommandPayload::SubmitPrompt {
        text: NonEmptyText::new("Revise the filename")?,
        delivery: PromptDelivery::Queue,
    })?;
    let revision = h.outgoing("turn/start", 2)?;
    assert_eq!(revision["params"]["collaborationMode"]["mode"], "plan");
    assert_eq!(
        revision["params"]["input"][0]["text"],
        "Revise the filename"
    );
    h.host.stop()?;
    Ok(())
}

#[test]
fn model_buttons_follow_pagination_and_revalidate_before_atomic_update() -> TestResult {
    let mut h = Harness::new()?;
    h.command(AgentCommandPayload::ListModels)?;
    let first = h.outgoing("model/list", 1)?;
    h.control.push_text(
        json!({"id":first["id"],"result":{"data":[],"nextCursor":"page-two"}}).to_string(),
    );
    let second = h.outgoing("model/list", 2)?;
    assert_eq!(second["params"]["cursor"], "page-two");
    let model = json!({"id":"catalog-id","model":"actual-model","displayName":"Button Model","description":"Test model", "hidden":false,"isDefault":false,
        "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"Fast"},{"reasoningEffort":"high","description":"Thorough"}]});
    h.control.push_text(
        json!({"id":second["id"],"result":{"data":[model.clone()],"nextCursor":null}}).to_string(),
    );
    h.choose(&h.form("选择模型")?, "Button Model")?;
    h.choose(&h.form("选择推理强度")?, "使用 high")?;
    let validate = h.outgoing("model/list", 3)?;
    h.control.push_text(
        json!({"id":validate["id"],"result":{"data":[model],"nextCursor":null}}).to_string(),
    );
    // Status is processed after the catalog response, and gives an observable completion barrier.
    wait_until(|| {
        h.host.inspect_bridge(|bridge| bridge.session_aggregate(h.session).is_some_and(|aggregate|
        aggregate.recent_events().any(|event| matches!(event.payload(), AgentEventPayload::Message(message) if message.content().as_str().contains("Model set"))))).unwrap_or(false)
    })?;
    h.command(AgentCommandPayload::SubmitPrompt {
        text: NonEmptyText::new("After model selection")?,
        delivery: PromptDelivery::Queue,
    })?;
    let turn = h.outgoing("turn/start", 1)?;
    assert_eq!(turn["params"]["model"], "actual-model");
    assert_eq!(turn["params"]["effort"], "high");
    h.host.stop()?;
    Ok(())
}

#[test]
fn desktop_next_turn_invalidates_the_previous_plan_choice() -> TestResult {
    let mut h = Harness::new()?;
    h.plan()?;
    let choice = h.form("计划已准备好")?;
    let mut start: serde_json::Value =
        serde_json::from_str(LIVE_FIXTURE.lines().nth(2).ok_or("missing start")?)?;
    start["params"]["turn"]["id"] = json!("next-desktop-turn");
    h.control.push_text(start.to_string());
    wait_until(|| {
        h.host
            .inspect_bridge(|b| {
                b.session_aggregate(h.session).is_some_and(|s| {
                    s.session().state() == AgentState::Running
                        && s.pending_interactions().len() == 0
                })
            })
            .unwrap_or(false)
    })?;
    assert!(h.choose(&choice, "实施计划").is_err());
    assert!(
        !locked(&h.control.outgoing)
            .iter()
            .any(|frame| frame.contains("Implement the plan."))
    );
    h.host.stop()?;
    Ok(())
}

#[test]
fn typing_revision_closes_plan_buttons_and_preserves_plan_mode() -> TestResult {
    let mut h = Harness::new()?;
    h.plan()?;
    let choice = h.form("计划已准备好")?;
    h.command(AgentCommandPayload::SubmitPrompt {
        text: NonEmptyText::new("Change the plan")?,
        delivery: PromptDelivery::Queue,
    })?;
    let request = h.outgoing("turn/start", 1)?;
    assert_eq!(request["params"]["collaborationMode"]["mode"], "plan");
    assert!(h.choose(&choice, "实施计划").is_err());
    h.host.stop()?;
    Ok(())
}
