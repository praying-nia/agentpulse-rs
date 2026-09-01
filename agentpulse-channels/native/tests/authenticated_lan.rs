//! Real authenticated private-LAN TLS flow and revocation checks.

use std::{
    error::Error,
    fs,
    net::{IpAddr, SocketAddr, TcpStream},
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use agentpulse_bridge::RuntimeHost;
use agentpulse_channel_native::{
    NATIVE_WEBSOCKET_PATH, NATIVE_WEBSOCKET_SUBPROTOCOL, NativeChannel, NativeChannelConfig,
    NativeChannelHealth, NativeClientMessage, NativeErrorCode, NativeServerMessage,
    decode_server_message, encode_client_message,
};
use agentpulse_core::ChannelId;
use agentpulse_pairing::{FileCredentialAuthorizer, HostCredentialStore};
use agentpulse_protocol::V1_PROTOCOL_VERSION;
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, StreamOwned,
    pki_types::{CertificateDer, ServerName},
};
use tungstenite::{ClientRequestBuilder, Message, WebSocket, client, http::Uri};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn Error>>;
type TlsClient = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn create() -> Result<Self, std::io::Error> {
        let path = std::env::temp_dir().join(format!("agentpulse-native-tls-{}", Uuid::now_v7()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn private_address() -> Result<IpAddr, Box<dyn Error>> {
    if_addrs::get_if_addrs()?
        .into_iter()
        .map(|interface| interface.ip())
        .find(|address| match address {
            IpAddr::V4(address) => address.is_private() || address.is_link_local(),
            // A link-local IPv6 address needs an interface scope identifier,
            // which is lost when `if_addrs` exposes it as a bare `IpAddr`.
            IpAddr::V6(address) => address.is_unique_local(),
        })
        .ok_or_else(|| "no private address is available for the TLS test".into())
}

fn connect(
    address: SocketAddr,
    server_name: &str,
    ca_der: &[u8],
    client_id: &str,
    token: &str,
) -> Result<TlsClient, Box<dyn Error>> {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(ca_der.to_vec()))?;
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connection = ClientConnection::new(
        Arc::new(config),
        ServerName::try_from(server_name.to_owned())?,
    )?;
    let tcp = TcpStream::connect(address)?;
    tcp.set_read_timeout(Some(Duration::from_secs(2)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(2)))?;
    let stream = StreamOwned::new(connection, tcp);
    let uri: Uri = format!(
        "wss://{server_name}:{}{NATIVE_WEBSOCKET_PATH}",
        address.port()
    )
    .parse()?;
    let request = ClientRequestBuilder::new(uri)
        .with_sub_protocol(NATIVE_WEBSOCKET_SUBPROTOCOL)
        .with_header("Authorization", format!("Bearer {token}"))
        .with_header("X-AgentPulse-Client-Id", client_id);
    let (socket, response) = client(request, stream)?;
    assert_eq!(
        response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok()),
        Some(NATIVE_WEBSOCKET_SUBPROTOCOL),
    );
    Ok(socket)
}

fn send_hello(socket: &mut TlsClient, client_id: &str) -> TestResult {
    let bytes = encode_client_message(&NativeClientMessage::Hello {
        client_id: client_id.to_owned(),
        display_name: "Authenticated Test Client".to_owned(),
        version: Some("0.1.0-test".to_owned()),
        supported_protocol_versions: vec![V1_PROTOCOL_VERSION],
    })?;
    socket.send(Message::text(String::from_utf8(bytes)?))?;
    Ok(())
}

fn read_server(socket: &mut TlsClient) -> Result<NativeServerMessage, Box<dyn Error>> {
    loop {
        match socket.read()? {
            Message::Text(text) => return Ok(decode_server_message(text.as_bytes())?),
            Message::Ping(_) | Message::Pong(_) => socket.flush()?,
            Message::Close(_) => return Err("server closed before an application frame".into()),
            Message::Binary(_) | Message::Frame(_) => return Err("unexpected frame".into()),
        }
    }
}

fn wait_for_listening(handle: &agentpulse_channel_native::NativeChannelHandle) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(2);
    while handle.snapshot().health != NativeChannelHealth::Listening && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if handle.snapshot().health != NativeChannelHealth::Listening {
        return Err("Native Channel did not return to listening".into());
    }
    Ok(())
}

#[test]
#[ignore = "requires a private interface and socket access"]
fn tls_upgrade_rejects_bad_credentials_binds_hello_and_observes_revocation() -> TestResult {
    let directory = TestDirectory::create()?;
    let store = HostCredentialStore::new(directory.0.join("credentials.json"));
    let identity = store.initialize("TLS Test Host")?;
    let client_id = Uuid::now_v7().to_string();
    let other_client_id = Uuid::now_v7().to_string();
    let token = store.issue_device(&client_id, "TLS Test Client", Some("0.1.0"))?;
    let config = NativeChannelConfig::authenticated_lan(
        ChannelId::new(),
        SocketAddr::new(private_address()?, 0),
        identity.tls_identity()?,
        Arc::new(FileCredentialAuthorizer::new(store.clone())),
    )?;
    let parts = NativeChannel::build(config)?;
    let (channel, source, handle) = parts.into_parts();
    let mut host = RuntimeHost::new();
    host.register_channel(channel, source)?;
    let _ = host.start()?;
    let address = handle
        .snapshot()
        .local_address
        .ok_or("TLS listener did not expose an address")?;

    assert!(
        connect(
            address,
            &identity.server_name,
            &identity.ca_certificate_der,
            &client_id,
            "wrong-token"
        )
        .is_err()
    );
    wait_for_listening(&handle)?;

    let mut mismatched = connect(
        address,
        &identity.server_name,
        &identity.ca_certificate_der,
        &client_id,
        &token,
    )?;
    send_hello(&mut mismatched, &other_client_id)?;
    assert!(matches!(
        read_server(&mut mismatched)?,
        NativeServerMessage::Error {
            code: NativeErrorCode::InvalidHandshake,
            recoverable: false,
            ..
        }
    ));
    drop(mismatched);
    wait_for_listening(&handle)?;

    let mut authorized = connect(
        address,
        &identity.server_name,
        &identity.ca_certificate_der,
        &client_id,
        &token,
    )?;
    send_hello(&mut authorized, &client_id)?;
    assert!(matches!(
        read_server(&mut authorized)?,
        NativeServerMessage::Hello { .. }
    ));
    store.revoke_device(&client_id)?;

    let deadline = Instant::now() + Duration::from_secs(2);
    while handle.snapshot().client_id.is_some() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(handle.snapshot().client_id.is_none());
    assert!(matches!(authorized.read(), Err(_) | Ok(Message::Close(_))));

    let _ = host.stop()?;
    Ok(())
}
