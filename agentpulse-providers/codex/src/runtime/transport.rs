//! Transport-specific setup; forwarding and protocol mapping stay shared.
use super::*;
#[cfg(windows)]
pub(super) use std::net::{TcpListener as LocalListener, TcpStream as LocalStream};
#[cfg(unix)]
pub(super) use std::os::unix::net::{UnixListener as LocalListener, UnixStream as LocalStream};

pub(super) fn bind_proxy(
    config: &CodexProviderConfig,
) -> Result<LocalListener, CodexProviderSourceError> {
    #[cfg(unix)]
    {
        let listener = LocalListener::bind(&config.proxy_socket_path)
            .map_err(|error| CodexProviderSourceError::runtime("client proxy bind", error))?;
        if let Err(error) =
            fs::set_permissions(&config.proxy_socket_path, fs::Permissions::from_mode(0o600))
        {
            let _ = fs::remove_file(&config.proxy_socket_path);
            return Err(CodexProviderSourceError::runtime(
                "client proxy permissions",
                error,
            ));
        }
        Ok(listener)
    }
    #[cfg(windows)]
    {
        let listener = config
            .proxy_listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        listener
            .map_or_else(|| LocalListener::bind(config.proxy_address), Ok)
            .map_err(|error| CodexProviderSourceError::runtime("client proxy bind", error))
    }
}

pub(super) fn cleanup_proxy(config: &CodexProviderConfig) -> io::Result<()> {
    #[cfg(unix)]
    {
        match fs::remove_file(&config.proxy_socket_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    #[cfg(windows)]
    {
        let _ = config;
        Ok(())
    }
}

pub(super) fn connect(
    config: &CodexProviderConfig,
    timeout: Duration,
) -> Result<WebSocket<LocalStream>, CodexProviderSourceError> {
    #[cfg(unix)]
    let stream = LocalStream::connect(&config.socket_path)
        .map_err(|error| CodexProviderSourceError::runtime("App Server connection", error))?;
    #[cfg(windows)]
    let stream = {
        let address = config
            .app_server_address
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ok_or_else(|| {
                CodexProviderSourceError::transport("App Server has no listening address")
            })?;
        LocalStream::connect_timeout(&address, timeout)
            .map_err(|error| CodexProviderSourceError::runtime("App Server connection", error))?
    };
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| CodexProviderSourceError::runtime("App Server I/O timeout", error))?;
    #[cfg(windows)]
    stream
        .set_nodelay(true)
        .map_err(|error| CodexProviderSourceError::runtime("App Server TCP_NODELAY", error))?;
    stream
        .set_nonblocking(true)
        .map_err(|error| CodexProviderSourceError::runtime("handshake nonblocking mode", error))?;
    let (socket, _) = drive_handshake(
        tungstenite::client::client_with_config(
            "ws://localhost/",
            stream,
            Some(websocket_config()),
        ),
        timeout,
    )
    .map_err(|error| {
        CodexProviderSourceError::transport(format!(
            "App Server WebSocket handshake failed: {error}"
        ))
    })?;
    socket
        .get_ref()
        .set_nonblocking(false)
        .map_err(|error| CodexProviderSourceError::runtime("App Server blocking mode", error))?;

    Ok(socket)
}

fn websocket_config() -> tungstenite::protocol::WebSocketConfig {
    tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(16 * 1024 * 1024))
        .max_frame_size(Some(16 * 1024 * 1024))
}

// tungstenite's callback fixes the error type to an HTTP response.
#[allow(clippy::result_large_err)]
pub(super) fn accept_proxy(
    stream: LocalStream,
    config: &CodexProviderConfig,
) -> Result<WebSocket<LocalStream>, String> {
    stream
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    let result = {
        let _ = config;
        drive_handshake(
            tungstenite::accept_with_config(stream, Some(websocket_config())),
            Duration::from_secs(2),
        )
    };
    #[cfg(windows)]
    let result = {
        use tungstenite::handshake::server::{Request, Response};
        let expected_path = config
            .remote_uri
            .strip_prefix(&format!("ws://{}", config.proxy_address))
            .ok_or("invalid proxy URI")?
            .to_owned();
        drive_handshake(
            tungstenite::accept_hdr_with_config(
                stream,
                move |request: &Request, response: Response| {
                    if request.uri().path() != expected_path
                        || request.uri().query().is_some()
                        || request.headers().contains_key("origin")
                    {
                        let mut rejection =
                            tungstenite::http::Response::new(Some("Forbidden".to_owned()));
                        *rejection.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
                        return Err(rejection);
                    }
                    Ok(response)
                },
                Some(websocket_config()),
            ),
            Duration::from_secs(2),
        )
    };
    let socket = result?;
    socket
        .get_ref()
        .set_nonblocking(false)
        .map_err(|error| error.to_string())?;
    Ok(socket)
}

fn drive_handshake<R: tungstenite::handshake::HandshakeRole>(
    mut result: Result<R::FinalResult, tungstenite::HandshakeError<R>>,
    timeout: Duration,
) -> Result<R::FinalResult, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match result {
            Ok(value) => return Ok(value),
            Err(tungstenite::HandshakeError::Failure(error)) => return Err(error.to_string()),
            Err(tungstenite::HandshakeError::Interrupted(handshake)) => {
                if Instant::now() >= deadline {
                    return Err("WebSocket handshake timed out".to_owned());
                }
                thread::sleep(Duration::from_millis(5));
                result = handshake.handshake();
            }
        }
    }
}

#[cfg(windows)]
pub(super) fn reported_address(stderr: &str) -> Option<std::net::SocketAddr> {
    stderr
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .find_map(|line| {
            let address = line
                .trim()
                .strip_prefix("listening on: ws://")?
                .trim()
                .parse::<std::net::SocketAddr>()
                .ok()?;
            (address.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                && address.port() != 0)
                .then_some(address)
        })
}
