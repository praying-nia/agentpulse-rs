//! Bounded single-Host Relay server.

use std::{
    collections::BTreeMap,
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::Duration,
};

use rustls::{ServerConfig, ServerConnection, StreamOwned};
use time::OffsetDateTime;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    CertificateStatus, RelayError, RelayServerConfig,
    crypto::{client_proof, decode_32, host_proof, verify_proof},
    framing::{read_frame, write_frame},
    protocol::{
        EndpointMessage, RelayErrorCode, RelayMessage, RouteRegistration, challenge,
        decode_endpoint, encode_relay,
    },
    tunnel::pump,
};

const TLS_TIMEOUT: Duration = Duration::from_secs(5);
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_PING_INTERVAL: Duration = Duration::from_secs(15);
const HOST_PONG_TIMEOUT: Duration = Duration::from_secs(5);
const TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const TUNNEL_STALLED_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTER_CONNECTIONS: usize = 32;
const MAX_WAITING_HOSTS: usize = 4;

type OuterStream = StreamOwned<ServerConnection, TcpStream>;

/// Bound Relay server and its validated certificate metadata.
pub struct RelayServer {
    listener: TcpListener,
    local_address: SocketAddr,
    tls: Arc<ServerConfig>,
    config: RelayServerConfig,
    certificate: CertificateStatus,
    state: Arc<Mutex<BTreeMap<String, WaitingHost>>>,
    active_connections: Arc<AtomicUsize>,
}

impl RelayServer {
    /// Validates configuration and binds the public listener.
    pub fn bind(config: RelayServerConfig) -> Result<Self, RelayError> {
        let (tls, certificate) = config.tls_server_config()?;
        let listener = TcpListener::bind(config.bind_address)
            .map_err(|source| RelayError::io("bind public Relay listener", source))?;
        listener
            .set_nonblocking(true)
            .map_err(|source| RelayError::io("configure public Relay listener", source))?;
        let local_address = listener
            .local_addr()
            .map_err(|source| RelayError::io("read Relay listener address", source))?;
        Ok(Self {
            listener,
            local_address,
            tls,
            config,
            certificate,
            state: Arc::new(Mutex::new(BTreeMap::new())),
            active_connections: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Returns the actual bound address.
    #[must_use]
    pub const fn local_address(&self) -> SocketAddr {
        self.local_address
    }

    /// Returns the validated leaf-certificate status.
    #[must_use]
    pub const fn certificate_status(&self) -> &CertificateStatus {
        &self.certificate
    }

    /// Serves until the shared stop flag is raised.
    pub fn run(&self, stop: Arc<AtomicBool>) -> Result<(), RelayError> {
        let mut workers = Vec::new();
        while !stop.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((tcp, _)) => {
                    if self.active_connections.load(Ordering::Acquire) >= MAX_OUTER_CONNECTIONS {
                        drop(tcp);
                        continue;
                    }
                    self.active_connections.fetch_add(1, Ordering::AcqRel);
                    let tls = Arc::clone(&self.tls);
                    let config = self.config.clone();
                    let state = Arc::clone(&self.state);
                    let active = Arc::clone(&self.active_connections);
                    let worker_stop = Arc::clone(&stop);
                    workers.push(thread::spawn(move || {
                        let _guard = ActiveGuard(active);
                        if let Err(error) = handle_outer(tcp, tls, config, state, &worker_stop)
                            && !matches!(error, RelayError::Stopped)
                        {
                            eprintln!("Relay connection closed: {error}");
                        }
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(source) => return Err(RelayError::io("accept Relay connection", source)),
            }
            let mut index = 0;
            while index < workers.len() {
                if workers[index].is_finished() {
                    let worker = workers.swap_remove(index);
                    let _ = worker.join();
                } else {
                    index += 1;
                }
            }
        }
        if let Ok(mut waiting) = self.state.lock() {
            waiting.clear();
        }
        for worker in workers {
            let _ = worker.join();
        }
        Ok(())
    }
}

impl std::fmt::Debug for RelayServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelayServer")
            .field("local_address", &self.local_address)
            .field("public_endpoint", &self.config.public_endpoint)
            .field("certificate", &self.certificate)
            .finish_non_exhaustive()
    }
}

struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct WaitingHost {
    connection_id: String,
    routes: BTreeMap<String, Zeroizing<[u8; 32]>>,
    client_sender: SyncSender<MatchedClient>,
}

struct WaitingHostGuard {
    state: Arc<Mutex<BTreeMap<String, WaitingHost>>>,
    connection_id: String,
}

impl Drop for WaitingHostGuard {
    fn drop(&mut self) {
        remove_waiting_host(&self.state, &self.connection_id);
    }
}

struct MatchedClient {
    connection_id: String,
    stream: OuterStream,
}

fn handle_outer(
    tcp: TcpStream,
    tls: Arc<ServerConfig>,
    config: RelayServerConfig,
    state: Arc<Mutex<BTreeMap<String, WaitingHost>>>,
    stop: &AtomicBool,
) -> Result<(), RelayError> {
    tcp.set_read_timeout(Some(TLS_TIMEOUT))
        .map_err(|source| RelayError::io("set Relay TLS read timeout", source))?;
    tcp.set_write_timeout(Some(TLS_TIMEOUT))
        .map_err(|source| RelayError::io("set Relay TLS write timeout", source))?;
    let connection = ServerConnection::new(tls).map_err(|error| RelayError::Tls {
        message: error.to_string(),
    })?;
    let mut stream = StreamOwned::new(connection, tcp);
    let (challenge_message, nonce, connection_id, expires_at) = challenge();
    write_relay(&mut stream, &challenge_message)?;
    stream
        .sock
        .set_read_timeout(Some(AUTH_TIMEOUT))
        .map_err(|source| RelayError::io("set Relay authentication timeout", source))?;
    let message = decode_endpoint(&read_frame(&mut stream)?)?;
    if expires_at <= OffsetDateTime::now_utc().unix_timestamp() {
        send_error(
            &mut stream,
            RelayErrorCode::InvalidHandshake,
            "authentication challenge expired",
            true,
        )?;
        return Err(RelayError::Timeout {
            operation: "authenticate Relay connection",
        });
    }
    match message {
        EndpointMessage::HostHello {
            host_id,
            routes,
            proof,
        } => handle_host(
            stream,
            &config,
            state,
            stop,
            connection_id,
            nonce,
            expires_at,
            host_id,
            routes,
            proof,
        ),
        EndpointMessage::ClientHello { route_id, proof } => handle_client(
            stream,
            state,
            connection_id,
            nonce,
            expires_at,
            route_id,
            proof,
        ),
        EndpointMessage::Pong { .. } => {
            send_error(
                &mut stream,
                RelayErrorCode::InvalidHandshake,
                "first endpoint message must authenticate a role",
                false,
            )?;
            Err(RelayError::Protocol {
                message: "unexpected Pong before authentication".to_owned(),
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_host(
    mut stream: OuterStream,
    config: &RelayServerConfig,
    state: Arc<Mutex<BTreeMap<String, WaitingHost>>>,
    stop: &AtomicBool,
    connection_id: String,
    nonce: [u8; 32],
    expires_at: i64,
    host_id: String,
    routes: Vec<RouteRegistration>,
    proof: String,
) -> Result<(), RelayError> {
    let key = config.host_authentication_key()?;
    let expected = host_proof(&key, &connection_id, &nonce, expires_at, &host_id, &routes)?;
    if host_id != config.host_id || !verify_proof(&expected, &proof) {
        send_error(
            &mut stream,
            RelayErrorCode::AuthenticationFailed,
            "authentication failed",
            false,
        )?;
        return Err(RelayError::Authentication);
    }
    let mut route_keys = BTreeMap::new();
    for route in routes {
        route_keys.insert(
            route.route_id,
            Zeroizing::new(decode_32("authentication_key", &route.authentication_key)?),
        );
    }
    let (client_sender, client_receiver) = mpsc::sync_channel(1);
    {
        let mut waiting = state.lock().map_err(|_| RelayError::Protocol {
            message: "Relay waiting state is unavailable".to_owned(),
        })?;
        if waiting.len() >= MAX_WAITING_HOSTS {
            send_error(
                &mut stream,
                RelayErrorCode::ResourceLimit,
                "Relay waiting registration capacity is full",
                true,
            )?;
            return Err(RelayError::HostBusy);
        }
        if routes_overlap(&waiting, &route_keys) {
            send_error(
                &mut stream,
                RelayErrorCode::HostBusy,
                "a matching Host route is already waiting",
                true,
            )?;
            return Err(RelayError::HostBusy);
        }
        waiting.insert(
            connection_id.clone(),
            WaitingHost {
                connection_id: connection_id.clone(),
                routes: route_keys,
                client_sender,
            },
        );
    }
    let _waiting_guard = WaitingHostGuard {
        state: Arc::clone(&state),
        connection_id: connection_id.clone(),
    };
    write_relay(
        &mut stream,
        &RelayMessage::HostWaiting {
            connection_id: connection_id.clone(),
        },
    )?;

    loop {
        if stop.load(Ordering::Acquire) {
            break Err(RelayError::Stopped);
        }
        match client_receiver.recv_timeout(HOST_PING_INTERVAL) {
            Ok(mut client) => {
                write_relay(
                    &mut stream,
                    &RelayMessage::TunnelReady {
                        peer_connection_id: client.connection_id.clone(),
                    },
                )?;
                write_relay(
                    &mut client.stream,
                    &RelayMessage::TunnelReady {
                        peer_connection_id: connection_id.clone(),
                    },
                )?;
                stream
                    .sock
                    .set_nonblocking(true)
                    .map_err(|source| RelayError::io("configure Host tunnel", source))?;
                client
                    .stream
                    .sock
                    .set_nonblocking(true)
                    .map_err(|source| RelayError::io("configure Client tunnel", source))?;
                break pump(
                    &mut stream,
                    &mut client.stream,
                    stop,
                    TUNNEL_IDLE_TIMEOUT,
                    TUNNEL_STALLED_TIMEOUT,
                )
                .map(|_| ());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let ping_id = Uuid::now_v7().to_string();
                write_relay(
                    &mut stream,
                    &RelayMessage::Ping {
                        ping_id: ping_id.clone(),
                    },
                )?;
                stream
                    .sock
                    .set_read_timeout(Some(HOST_PONG_TIMEOUT))
                    .map_err(|source| RelayError::io("set Host Pong timeout", source))?;
                match decode_endpoint(&read_frame(&mut stream)?)? {
                    EndpointMessage::Pong { ping_id: received } if received == ping_id => {}
                    _ => {
                        break Err(RelayError::Protocol {
                            message: "Host did not answer the matching Relay Ping".to_owned(),
                        });
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err(RelayError::HostUnavailable);
            }
        }
    }
}

fn handle_client(
    mut stream: OuterStream,
    state: Arc<Mutex<BTreeMap<String, WaitingHost>>>,
    connection_id: String,
    nonce: [u8; 32],
    expires_at: i64,
    route_id: String,
    proof: String,
) -> Result<(), RelayError> {
    let claim = {
        let waiting = state.lock().map_err(|_| RelayError::Protocol {
            message: "Relay waiting state is unavailable".to_owned(),
        })?;
        if waiting.is_empty() {
            send_error(
                &mut stream,
                RelayErrorCode::HostUnavailable,
                "Host is unavailable",
                true,
            )?;
            return Err(RelayError::HostUnavailable);
        }
        let Some((waiting, key)) = waiting
            .values()
            .find_map(|waiting| waiting.routes.get(&route_id).map(|key| (waiting, key)))
        else {
            send_error(
                &mut stream,
                RelayErrorCode::AuthenticationFailed,
                "authentication failed",
                false,
            )?;
            return Err(RelayError::Authentication);
        };
        (
            waiting.connection_id.clone(),
            waiting.client_sender.clone(),
            **key,
        )
    };
    let expected = client_proof(&claim.2, &connection_id, &nonce, expires_at, &route_id)?;
    if !verify_proof(&expected, &proof) {
        send_error(
            &mut stream,
            RelayErrorCode::AuthenticationFailed,
            "authentication failed",
            false,
        )?;
        return Err(RelayError::Authentication);
    }
    {
        let mut waiting = state.lock().map_err(|_| RelayError::Protocol {
            message: "Relay waiting state is unavailable".to_owned(),
        })?;
        if !waiting.contains_key(&claim.0) {
            send_error(
                &mut stream,
                RelayErrorCode::HostUnavailable,
                "Host is unavailable",
                true,
            )?;
            return Err(RelayError::HostUnavailable);
        }
        waiting.remove(&claim.0);
    }
    claim
        .1
        .send(MatchedClient {
            connection_id,
            stream,
        })
        .map_err(|_| RelayError::HostUnavailable)
}

fn remove_waiting_host(state: &Mutex<BTreeMap<String, WaitingHost>>, connection_id: &str) {
    if let Ok(mut waiting) = state.lock() {
        waiting.remove(connection_id);
    }
}

fn routes_overlap(
    waiting: &BTreeMap<String, WaitingHost>,
    candidate: &BTreeMap<String, Zeroizing<[u8; 32]>>,
) -> bool {
    candidate.keys().any(|route_id| {
        waiting
            .values()
            .any(|host| host.routes.contains_key(route_id))
    })
}

fn write_relay(stream: &mut OuterStream, message: &RelayMessage) -> Result<(), RelayError> {
    write_frame(stream, &encode_relay(message)?)
}

fn send_error(
    stream: &mut OuterStream,
    code: RelayErrorCode,
    message: &str,
    recoverable: bool,
) -> Result<(), RelayError> {
    write_relay(
        stream,
        &RelayMessage::Error {
            code,
            message: message.to_owned(),
            recoverable,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    fn waiting_host(connection_id: &str, route_id: &str) -> WaitingHost {
        let (client_sender, _) = mpsc::sync_channel(1);
        WaitingHost {
            connection_id: connection_id.to_owned(),
            routes: BTreeMap::from([(route_id.to_owned(), Zeroizing::new([7_u8; 32]))]),
            client_sender,
        }
    }

    #[test]
    fn waiting_guard_releases_only_its_registration_and_detects_route_overlap()
    -> Result<(), Box<dyn Error>> {
        let first = Uuid::now_v7().to_string();
        let second = Uuid::now_v7().to_string();
        let state = Arc::new(Mutex::new(BTreeMap::from([
            (first.clone(), waiting_host(&first, "first-route")),
            (second.clone(), waiting_host(&second, "second-route")),
        ])));
        drop(WaitingHostGuard {
            state: Arc::clone(&state),
            connection_id: first.clone(),
        });
        assert_eq!(
            state
                .lock()
                .map_err(|_| "state lock poisoned")?
                .get(&second)
                .map(|waiting| waiting.connection_id.as_str()),
            Some(second.as_str())
        );

        let overlapping = BTreeMap::from([("second-route".to_owned(), Zeroizing::new([9_u8; 32]))]);
        let distinct = BTreeMap::from([("third-route".to_owned(), Zeroizing::new([9_u8; 32]))]);
        let waiting = state.lock().map_err(|_| "state lock poisoned")?;
        assert!(routes_overlap(&waiting, &overlapping));
        assert!(!routes_overlap(&waiting, &distinct));
        Ok(())
    }
}
