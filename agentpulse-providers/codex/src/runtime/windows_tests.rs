
fn windows_config() -> Result<CodexProviderConfig, Box<dyn Error>> {
    let id = ProviderId::new();
    Ok(CodexProviderConfig::discovering(id, std::env::temp_dir().join(format!("ap-win-{id}")))?)
}

#[test]
fn loopback_config_reserves_port_and_accepts_long_unicode_paths() -> TestResult {
    let id = ProviderId::new();
    let root = std::env::temp_dir().join("\u{6d4b}\u{8bd5}".repeat(64));
    let config = CodexProviderConfig::discovering(id, root)?;
    assert_eq!(config.remote_uri(), format!("ws://{}", config.proxy_address));
    assert_eq!(config.proxy_token.expose().len(), 64);
    assert!(!format!("{config:?}").contains(config.proxy_token.expose()));
    assert_eq!(config.app_server_uri, "ws://127.0.0.1:0");
    assert!(std::net::TcpListener::bind(config.proxy_address).is_err());
    let address = config.proxy_address;
    drop(config);
    let _released = std::net::TcpListener::bind(address)?;
    Ok(())
}

#[test]
fn reported_address_requires_nonzero_ipv4_loopback_listener() {
    assert!(transport::reported_address("listening on: ws://127.0.0.1:1234").is_none());
    assert!(transport::reported_address("listening on: ws://0.0.0.0:4000").is_none());
    assert!(transport::reported_address("listening on: ws://127.0.0.1:0").is_none());
    assert!(transport::reported_address("readyz: http://127.0.0.1:4000/readyz").is_none());
    assert_eq!(transport::reported_address("codex app-server\n  listening on: ws://127.0.0.1:4000\n").map(|a| a.port()), Some(4000));
}

#[test]
fn proxy_requires_bearer_and_rejects_non_root_targets_and_browser_origin() -> TestResult {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::http::header::{AUTHORIZATION, ORIGIN};
    let config = windows_config()?;
    let listener = transport::bind_proxy(&config)?;
    let address = config.proxy_address;
    let uri = config.remote_uri().to_owned();
    let token = config.proxy_token.expose().to_owned();
    let worker = thread::spawn(move || -> Result<(), String> {
        for should_accept in [false, false, false, false, false, true] {
            let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
            stream.set_read_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?;
            stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?;
            let result = transport::accept_proxy(stream, &config);
            if result.is_ok() != should_accept { return Err(format!("unexpected handshake acceptance: {should_accept}")); }
        }
        Ok(())
    });
    let cases = [
        (uri.clone(), None, None, false),
        (uri.clone(), Some("wrong-token"), None, false),
        (
            format!("{uri}/invalid"),
            Some(token.as_str()),
            None,
            false,
        ),
        (
            format!("{uri}/?unexpected=true"),
            Some(token.as_str()),
            None,
            false,
        ),
        (
            uri.clone(),
            Some(token.as_str()),
            Some("https://example.invalid"),
            false,
        ),
        (uri, Some(token.as_str()), None, true),
    ];
    for (case_index, (target, supplied_token, origin, should_accept)) in
        cases.into_iter().enumerate()
    {
        let stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut request = target.as_str().into_client_request()?;
        if let Some(supplied_token) = supplied_token {
            request.headers_mut().insert(AUTHORIZATION, format!("Bearer {supplied_token}").parse()?);
        }
        if let Some(origin) = origin { request.headers_mut().insert(ORIGIN, origin.parse()?); }
        let result = tungstenite::client(request, stream);
        if should_accept {
            assert!(result.is_ok());
        } else {
            assert!(
                matches!(&result, Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response))) if response.status() == 403),
                "rejection case {case_index} returned {result:?}"
            );
        }
    }
    worker.join().map_err(|_| "handshake server panicked")??;
    Ok(())
}

#[test]
fn proxy_restart_refuses_an_occupied_port() -> TestResult {
    let config = windows_config()?;
    drop(transport::bind_proxy(&config)?);
    let occupied = std::net::TcpListener::bind(config.proxy_address)?;
    assert!(transport::bind_proxy(&config).is_err());
    drop(occupied);
    let _restarted = transport::bind_proxy(&config)?;
    Ok(())
}

#[test]
#[ignore = "child process fixture for readiness and handshake failure tests"]
fn readiness_child() -> TestResult {
    match std::env::var("AGENTPULSE_READINESS_FIXTURE").as_deref() {
        Ok("no-listener") => thread::sleep(Duration::from_secs(20)),
        Ok("no-handshake") => {
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
            eprintln!("  listening on: ws://{}", listener.local_addr()?);
            let (_stream, _) = listener.accept()?;
            thread::sleep(Duration::from_secs(20));
        }
        _ => {}
    }
    Ok(())
}

#[test]
fn readiness_and_handshake_failures_are_bounded_and_processes_are_reaped() -> TestResult {
    for mode in ["no-listener", "no-handshake", "exit"] {
        let config = windows_config()?.with_startup_timeout(Duration::from_millis(500));
        let mut runtime = ManagedCodexRuntime::default();
        let mut command = Command::new(std::env::current_exe()?);
        command.args(["--exact", "runtime::tests::windows_tests::readiness_child", "--ignored", "--nocapture"])
            .env("AGENTPULSE_READINESS_FIXTURE", mode).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        let started = Instant::now();
        assert!(runtime.launch_windows(&config, &mut command).is_err());
        runtime.stop(&config)?;
        assert!(runtime.process.is_none());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(locked(&config.app_server_address).is_none());
    }
    Ok(())
}

#[test]
#[ignore = "requires installed Codex and normal user home; no model turn is started"]
fn installed_windows_runtime_proxy_roundtrip_restart_and_bad_clients() -> TestResult {
    use crate::CodexProvider;
    let config = windows_config()?.with_startup_timeout(Duration::from_secs(15));
    let root = config.runtime_root.clone();
    let parts = CodexProvider::build(config.clone())?;
    let (port, source, handle) = parts.into_parts();
    let mut host = RuntimeHost::new();
    host.register_provider(port, source)?;
    let operation = (|| -> TestResult {
        for _ in 0..2 {
            host.start()?;
            assert_eq!(handle.snapshot().health(), CodexProviderHealth::Running);
            // Idle and malformed peers must not prevent a valid desktop connection.
            let idle = TcpStream::connect(config.proxy_address)?;
            let mut malformed = TcpStream::connect(config.proxy_address)?;
            use std::io::Write;
            malformed.write_all(b"GET /invalid HTTP/1.1\r\nHost: localhost\r\n\r\n")?;
            drop(malformed);
            let mut desktop = connect_desktop(&config)?;
            desktop.send(Message::text(r#"{"id":101,"method":"initialize","params":{"clientInfo":{"name":"agentpulse_windows_test","version":"0.1.0"},"capabilities":{"experimentalApi":true}}}"#))?;
            expect_response(&mut desktop, 101)?;
            desktop.send(Message::text(r#"{"method":"initialized"}"#))?;
            desktop.send(Message::text(r#"{"id":102,"method":"model/list","params":{}}"#))?;
            let models = expect_response(&mut desktop, 102)?;
            assert!(models["data"].is_array());
            assert_eq!(handle.snapshot().health(), CodexProviderHealth::Running);
            let started = Instant::now();
            host.stop()?;
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(!config.runtime_directory.exists());
            assert!(locked(&config.app_server_address).is_none());
            assert!(TcpStream::connect(config.proxy_address).is_err());
            drop(idle);
            drop(desktop);
        }
        Ok(())
    })();
    let cleanup = host.stop();
    let _ = fs::remove_dir(root);
    operation?;
    cleanup?;
    Ok(())
}

fn expect_response(socket: &mut WebSocket<LocalStream>, id: u64) -> Result<serde_json::Value, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        match socket.read() {
            Ok(Message::Text(text)) => {
                let value: serde_json::Value = serde_json::from_str(&text)?;
                if value["id"] == id {
                    if !value["error"].is_null() { return Err(format!("RPC error: {}", value["error"]).into()); }
                    return Ok(value["result"].clone());
                }
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => socket.flush()?,
            Err(tungstenite::Error::Io(error)) if is_timeout(&error) => {}
            other => return Err(format!("unexpected desktop frame: {other:?}").into()),
        }
    }
    Err("desktop response timed out".into())
}

#[test]
fn slow_handshake_has_a_total_deadline() -> TestResult {
    use std::io::Write;
    let config = windows_config()?;
    let listener = transport::bind_proxy(&config)?;
    let address = config.proxy_address;
    let worker = thread::spawn(move || -> Result<Duration, String> {
        let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
        let started = Instant::now();
        assert!(transport::accept_proxy(stream, &config).is_err());
        Ok(started.elapsed())
    });
    let mut stream = TcpStream::connect(address)?;
    // Every byte arrives within the old per-read timeout. A total deadline
    // must still end the handshake instead of letting this occupy a slot.
    for _ in 0..12 {
        if stream.write_all(b"G").is_err() { break; }
        thread::sleep(Duration::from_millis(200));
    }
    let elapsed = worker.join().map_err(|_| "handshake worker panicked")??;
    assert!(elapsed < Duration::from_secs(3));
    Ok(())
}

#[test]
fn version_probe_timeout_terminates_the_fixture() -> TestResult {
    let config = windows_config()?.with_startup_timeout(Duration::from_millis(200));
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["--exact", "runtime::tests::windows_tests::readiness_child", "--ignored", "--nocapture"])
        .env("AGENTPULSE_READINESS_FIXTURE", "no-listener").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    assert!(matches!(probe_windows_command(&config, &mut command), Err(CodexProviderSourceError::VersionProbe { .. })));
    assert!(started.elapsed() < Duration::from_secs(2));
    Ok(())
}
