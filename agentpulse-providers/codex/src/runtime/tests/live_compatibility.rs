// The managed runtime and protocol validators used here are production code.
use serde_json::Value;

fn live_response(
    io: &mut dyn AppServerIo,
    protocol: &mut ProtocolEngine,
    expected: ExpectedResponse,
) -> Result<Value, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match io.read()? {
            ReadOutcome::Timeout => continue,
            ReadOutcome::Closed => return Err("App Server closed during compatibility check".into()),
            ReadOutcome::Text(text) => match protocol.parse_server_text(&text)? {
                ServerFrame::Response { expected: actual, result, .. } if actual == expected => return Ok(result),
                ServerFrame::Notification { .. } => {},
                ServerFrame::Request { id, method, .. } => io.write_text(protocol.unsupported_request_response(id, &method)?)?,
                other => return Err(format!("unexpected live frame: {other:?}").into()),
            },
        }
    }
    Err("App Server compatibility response timed out".into())
}

#[test]
#[ignore = "requires installed Codex and user home; optional inference with AGENTPULSE_CODEX_LIVE_TURN=1"]
fn installed_codex_catalog_thread_and_optional_turn_match_schema() -> TestResult {
    let root = std::env::temp_dir().join(format!("apcheck{}", ProviderId::new().to_string().chars().rev().take(8).collect::<String>()));
    let config = CodexProviderConfig::discovering(ProviderId::new(), &root)?
        .with_startup_timeout(Duration::from_secs(30));
    let mut runtime = ManagedCodexRuntime::default();
    let operation = (|| -> TestResult {
        let mut io = runtime.start(&config)?;
        let mut protocol = ProtocolEngine::new(ProtocolSchema::compile()?);
        let (_, request) = protocol.initialize_request()?;
        io.write_text(request)?;
        live_response(io.as_mut(), &mut protocol, ExpectedResponse::Initialize)?;
        io.write_text(protocol.initialized_notification()?)?;
        for expected in [ExpectedResponse::ModelList, ExpectedResponse::PermissionProfileList] {
            let (_, request) = protocol.request(expected, json!({}))?;
            io.write_text(request)?;
            let result = live_response(io.as_mut(), &mut protocol, expected)?;
            if expected == ExpectedResponse::ModelList {
                assert!(result["data"].as_array().is_some_and(|models| !models.is_empty()));
            }
        }
        let (_, request) = protocol.request(ExpectedResponse::ThreadStart, json!({
            "cwd": root, "ephemeral": true, "approvalPolicy": "never", "sandbox": "read-only"
        }))?;
        io.write_text(request)?;
        let thread = live_response(io.as_mut(), &mut protocol, ExpectedResponse::ThreadStart)?;
        let thread_id = thread["thread"]["id"].as_str().ok_or("missing live thread ID")?;
        if std::env::var("AGENTPULSE_CODEX_LIVE_TURN").as_deref() == Ok("1") {
            let (_, request) = protocol.request(ExpectedResponse::TurnStart, json!({
                "threadId": thread_id,
                "input": [{"type":"text", "text":"Reply with exactly AP_CODEX_COMPAT_OK. Do not use tools.", "text_elements":[]}]
            }))?;
            io.write_text(request)?;
            live_response(io.as_mut(), &mut protocol, ExpectedResponse::TurnStart)?;
            let deadline = Instant::now() + Duration::from_secs(120);
            let mut replied = false;
            let mut completed = false;
            while Instant::now() < deadline {
                match io.read()? {
                    ReadOutcome::Timeout => {},
                    ReadOutcome::Closed => return Err("App Server closed during live turn".into()),
                    ReadOutcome::Text(text) => match protocol.parse_server_text(&text)? {
                        ServerFrame::Notification { method, params } => {
                            if method == "item/completed" && params["item"]["type"] == "agentMessage" {
                                replied |= params["item"]["text"].as_str().is_some_and(|text| text.contains("AP_CODEX_COMPAT_OK"));
                            }
                            if method == "turn/completed" {
                                assert_eq!(params["turn"]["status"], "completed");
                                completed = true;
                                break;
                            }
                        },
                        ServerFrame::Request { id, method, .. } => io.write_text(protocol.unsupported_request_response(id, &method)?)?,
                        other => return Err(format!("unexpected live turn frame: {other:?}").into()),
                    },
                }
            }
            assert!(completed && replied, "live turn did not complete with the expected assistant reply");
        }
        io.close()?;
        Ok(())
    })();
    let cleanup = runtime.stop(&config);
    let _ = fs::remove_dir_all(&root);
    operation?;
    cleanup?;
    Ok(())
}
