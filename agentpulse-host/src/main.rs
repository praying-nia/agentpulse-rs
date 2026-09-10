//! AgentPulse Host command-line application.

mod direct;

use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fs::{self, File},
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use agentpulse_bridge::RuntimeHost;
use agentpulse_channel_native::{
    NATIVE_TRANSPORT_VERSION, NativeChannel, NativeChannelConfig, NativeChannelHealth,
};
use agentpulse_core::{ChannelId, ProviderId, SessionAggregateConfig};
use agentpulse_pairing::{
    FileCredentialAuthorizer, HostCredentialStore, PairingSession, terminal_qr,
};
use agentpulse_platform::{
    AdminListener, AdminStream, atomic_write_private, ensure_private_dir, open_private_lock,
    protect_file,
};
use agentpulse_protocol::V2_PROTOCOL_VERSION;
use agentpulse_provider_codex::{
    CodexProvider, CodexProviderConfig, CodexProviderHealth, SUPPORTED_CODEX_CLI_VERSION,
};
use agentpulse_relay::{
    RelayConnectionCanceller, RelayEndpoint, RelayError, RelayHostConnectionConfig,
    RouteRegistration, connect_host_once_with_route_check_and_waiting, derive_route,
    device_root_from_token,
};
use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use fs2::FileExt;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

type AppResult<T> = Result<T, Box<dyn Error>>;
const DEFAULT_NATIVE_PORT: u16 = 49_320;
const RELAY_CONNECTOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const CODEX_PROXY_TOKEN_ENV: &str = "AGENTPULSE_CODEX_PROXY_TOKEN";

#[derive(Parser)]
#[command(name = "agentpulse", version, about = "Secure local AgentPulse Host")]
struct Cli {
    /// Overrides the private AgentPulse configuration directory.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: HostCommand,
}

#[derive(Subcommand)]
enum HostCommand {
    /// Creates a new local Host identity and certificate authority.
    Init(InitArgs),
    /// Manages the explicit Codex thread discovery boundary.
    Threads(ThreadsArgs),
    /// Runs the Codex Provider and authenticated Native LAN endpoint.
    Serve(ServeArgs),
    /// Starts Codex against the running managed App Server.
    Codex(CodexArgs),
    /// Opens a two-minute, one-device pairing session.
    Pair,
    /// Manages paired Android devices.
    Devices(DevicesArgs),
    /// Rotates Host credentials and revokes every device.
    Credentials(CredentialsArgs),
    /// Configures the optional outbound public Relay path.
    Relay(RelayArgs),
    /// Configures explicit public direct pairing and Native endpoints.
    Direct(direct::DirectArgs),
    /// Prints current Host and endpoint health.
    Status,
    /// Gracefully stops the running Host.
    Stop,
}

#[derive(Args)]
struct InitArgs {
    /// Human-readable computer name shown in the app.
    #[arg(long)]
    name: String,
}

#[derive(Args)]
struct ThreadsArgs {
    #[command(subcommand)]
    command: ThreadsCommand,
}

#[derive(Subcommand)]
enum ThreadsCommand {
    /// Adds one or more Codex UUIDv7 thread IDs.
    Add { thread_ids: Vec<String> },
    /// Removes one or more configured thread IDs.
    Remove { thread_ids: Vec<String> },
    /// Lists configured thread IDs.
    List,
}

#[derive(Args)]
struct ServeArgs {
    /// Local IP; must match direct settings when configured, otherwise private/link-local or Relay loopback.
    #[arg(long)]
    bind: Option<IpAddr>,
    /// Stable Native WSS port (default 49320); must match configured direct settings.
    #[arg(long)]
    port: Option<u16>,
    /// Codex executable to version-check and launch.
    #[arg(long, default_value = "codex")]
    codex: PathBuf,
    /// Follows threads started in this App Server without persistent thread bindings.
    #[arg(long)]
    discover_threads: bool,
}

#[derive(Args)]
struct CodexArgs {
    /// Codex executable to start.
    #[arg(long, default_value = "codex")]
    codex: PathBuf,
    /// Arguments forwarded after `codex --remote <URI>`.
    #[arg(last = true)]
    arguments: Vec<String>,
}

#[derive(Args)]
struct DevicesArgs {
    #[command(subcommand)]
    command: DevicesCommand,
}

#[derive(Subcommand)]
enum DevicesCommand {
    /// Lists paired device metadata without credentials.
    List,
    /// Revokes a device and disconnects it within one transport poll interval.
    Revoke { client_id: String },
}

#[derive(Args)]
struct CredentialsArgs {
    #[command(subcommand)]
    command: CredentialsCommand,
}

#[derive(Subcommand)]
enum CredentialsCommand {
    /// Replaces the local CA and revokes every paired device.
    Rotate {
        /// Required explicit acknowledgement.
        #[arg(long)]
        confirm_revoke_all: bool,
    },
}

#[derive(Args)]
struct RelayArgs {
    #[command(subcommand)]
    command: RelayCommand,
}

#[derive(Subcommand)]
enum RelayCommand {
    /// Stores one public endpoint and reads its enrollment Token from standard input.
    Configure {
        /// Canonical Relay DNS authority, such as relay.example.com:2333.
        #[arg(long)]
        endpoint: RelayEndpoint,
        /// Required acknowledgement that the Token is supplied on standard input.
        #[arg(long)]
        token_stdin: bool,
    },
    /// Prints the configured endpoint without revealing its Token.
    Status,
    /// Removes the optional Relay configuration.
    Disable {
        /// Required explicit acknowledgement.
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Clone, Debug)]
struct HostPaths {
    data_dir: PathBuf,
    credential_store: PathBuf,
    runtime_dir: PathBuf,
    lock_file: PathBuf,
    admin_socket: PathBuf,
    status_file: PathBuf,
    relay_config: PathBuf,
}

impl HostPaths {
    fn resolve(override_dir: Option<PathBuf>) -> AppResult<Self> {
        let data_dir = if let Some(path) = override_dir {
            path
        } else {
            ProjectDirs::from("moe", "gensoukyo", "AgentPulse")
                .ok_or("cannot resolve a private configuration directory")?
                .config_dir()
                .to_path_buf()
        };
        let runtime_dir = data_dir.join("runtime");
        Ok(Self {
            credential_store: data_dir.join("host-credentials.json"),
            lock_file: runtime_dir.join("host.lock"),
            admin_socket: runtime_dir.join("admin.sock"),
            status_file: runtime_dir.join("status.json"),
            relay_config: data_dir.join("relay.json"),
            data_dir,
            runtime_dir,
        })
    }

    fn ensure(&self) -> AppResult<()> {
        ensure_private_dir(&self.data_dir)?;
        ensure_private_dir(&self.runtime_dir)?;
        Ok(())
    }

    fn store(&self) -> HostCredentialStore {
        HostCredentialStore::new(&self.credential_store)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeStatus {
    host_id: String,
    host_name: String,
    server_name: String,
    native_address: SocketAddr,
    codex_executable: Option<PathBuf>,
    codex_remote_uri: String,
    provider_health: String,
    native_health: String,
    #[serde(default)]
    direct: Option<direct::DirectSettings>,
    relay_endpoint: Option<String>,
    relay_health: String,
    relay_last_error: Option<String>,
    #[serde(default)]
    relay_ready_routes: Vec<String>,
    pid: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexLaunchCredentials {
    remote_uri: String,
    auth_token: Option<String>,
    executable: Option<PathBuf>,
}

const RELAY_HOST_CONFIG_SCHEMA_VERSION: u16 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayHostConfigRecord {
    schema_version: u16,
    endpoint: RelayEndpoint,
    enrollment_token: String,
}

struct RelayHostSettings {
    endpoint: RelayEndpoint,
    enrollment_token: Zeroizing<String>,
}

#[derive(Clone, Debug)]
struct RelayRuntimeState(Arc<Mutex<RelayRuntimeSnapshot>>);

#[derive(Clone, Debug)]
struct RelayRuntimeSnapshot {
    ready_routes: Vec<String>,
    health: String,
    last_error: Option<String>,
}

struct RelayConnector {
    canceller: RelayConnectionCanceller,
    worker: thread::JoinHandle<()>,
}

impl RelayConnector {
    fn cancel_and_join(self, timeout: Duration) -> Result<(), &'static str> {
        self.canceller.cancel();
        let deadline = Instant::now() + timeout;
        while !self.worker.is_finished() {
            if Instant::now() >= deadline {
                return Err("Relay connector did not stop before its deadline; detaching it");
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.worker
            .join()
            .map_err(|_| "Relay connector thread terminated unexpectedly")
    }
}

impl RelayRuntimeState {
    fn new(initial_health: &str) -> Self {
        Self(Arc::new(Mutex::new(RelayRuntimeSnapshot {
            ready_routes: Vec::new(),
            health: initial_health.to_owned(),
            last_error: None,
        })))
    }

    fn update(&self, health: &str, last_error: Option<String>) {
        if let Ok(mut state) = self.0.lock() {
            state.ready_routes.clear();
            state.health = health.to_owned();
            state.last_error = last_error;
        }
    }

    fn ready(&self, routes: &[RouteRegistration]) {
        if let Ok(mut state) = self.0.lock() {
            for route in routes {
                if !state.ready_routes.contains(&route.route_id) {
                    state.ready_routes.push(route.route_id.clone());
                }
            }
            state.health = "waiting_or_tunneling".to_owned();
            state.last_error = None;
        }
    }

    fn unready(&self, route_id: &str, error: Option<String>) {
        if let Ok(mut state) = self.0.lock() {
            state.ready_routes.retain(|id| id != route_id);
            if state.ready_routes.is_empty() {
                state.health = "refreshing".to_owned();
            }
            state.last_error = error;
        }
    }

    fn snapshot(&self) -> RelayRuntimeSnapshot {
        self.0.lock().map_or_else(
            |_| RelayRuntimeSnapshot {
                ready_routes: Vec::new(),
                health: "unavailable".to_owned(),
                last_error: Some("Relay status lock is unavailable".to_owned()),
            },
            |state| state.clone(),
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum AdminRequest {
    Status,
    CodexLaunch,
    Stop,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminResponse {
    ok: bool,
    status: Option<RuntimeStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_launch: Option<CodexLaunchCredentials>,
    error: Option<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        let mut source = error.source();
        while let Some(cause) = source {
            eprintln!("caused by: {cause}");
            source = cause.source();
        }
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    let cli = Cli::parse();
    let paths = HostPaths::resolve(cli.data_dir)?;
    paths.ensure()?;
    match cli.command {
        HostCommand::Init(args) => initialize(&paths, &args.name),
        HostCommand::Threads(args) => threads(&paths, args.command),
        HostCommand::Serve(args) => serve(&paths, args),
        HostCommand::Codex(args) => codex(&paths, args),
        HostCommand::Pair => pair(&paths),
        HostCommand::Devices(args) => devices(&paths, args.command),
        HostCommand::Credentials(args) => credentials(&paths, args.command),
        HostCommand::Relay(args) => relay(&paths, args.command),
        HostCommand::Direct(args) => direct::run(&paths, args.command),
        HostCommand::Status => print_status(&paths),
        HostCommand::Stop => stop(&paths),
    }
}

fn initialize(paths: &HostPaths, name: &str) -> AppResult<()> {
    let identity = paths.store().initialize(name)?;
    println!("Initialized AgentPulse Host '{}'", identity.host_name);
    println!("Host ID: {}", identity.host_id);
    println!("Next: agentpulse threads add <CODEX_THREAD_UUIDV7>");
    Ok(())
}

fn threads(paths: &HostPaths, command: ThreadsCommand) -> AppResult<()> {
    let store = paths.store();
    let identity = store.load_identity()?;
    match command {
        ThreadsCommand::List => {
            if identity.thread_ids.is_empty() {
                println!("No Codex threads configured.");
            } else {
                for thread_id in identity.thread_ids {
                    println!("{thread_id}");
                }
            }
        }
        ThreadsCommand::Add { thread_ids } => {
            if thread_ids.is_empty() {
                return Err("at least one thread ID is required".into());
            }
            let mut configured = identity.thread_ids.into_iter().collect::<BTreeSet<_>>();
            configured.extend(thread_ids);
            store.set_thread_ids(configured.into_iter().collect())?;
            println!("Updated the Codex thread set.");
        }
        ThreadsCommand::Remove { thread_ids } => {
            if thread_ids.is_empty() {
                return Err("at least one thread ID is required".into());
            }
            let mut configured = identity.thread_ids.into_iter().collect::<BTreeSet<_>>();
            for thread_id in thread_ids {
                if !configured.remove(&thread_id) {
                    return Err(format!("thread {thread_id} is not configured").into());
                }
            }
            store.set_thread_ids(configured.into_iter().collect())?;
            println!("Updated the Codex thread set.");
        }
    }
    Ok(())
}

fn serve(paths: &HostPaths, args: ServeArgs) -> AppResult<()> {
    let _lock = acquire_serve_lock(paths)?;
    let _status_cleanup = StatusFileCleanup(paths);
    let store = paths.store();
    let identity = store.load_identity()?;
    eprintln!(
        "agentpulse host: configured_thread_ids={:?} discover_threads={}",
        identity.thread_ids, args.discover_threads
    );
    let relay_settings = load_relay_settings(&paths.relay_config, &identity.host_id)?;
    if identity.thread_ids.is_empty() && !args.discover_threads {
        return Err("no Codex threads configured; run `agentpulse threads add` first".into());
    }
    let direct = direct::load(paths)?;
    let native_port = direct
        .as_ref()
        .map_or(args.port.unwrap_or(DEFAULT_NATIVE_PORT), |v| v.native_port);
    let bind_ip = if let Some(settings) = &direct {
        if args.bind.is_some_and(|v| v != settings.bind)
            || args.port.is_some_and(|v| v != settings.native_port)
        {
            return Err("serve --bind/--port conflict with direct configuration".into());
        }
        settings.bind
    } else {
        match args.bind {
            Some(address) => {
                if address.is_loopback() {
                    if relay_settings.is_none() {
                        return Err("loopback Native binding requires a configured Relay".into());
                    }
                } else {
                    validate_private_ip(address)?;
                }
                address
            }
            None => select_private_ip()?,
        }
    };
    let provider_id = ProviderId::from_str(&identity.provider_id)?;
    let channel_id = ChannelId::from_str(&identity.channel_id)?;
    let codex_executable = agentpulse_provider_codex::resolve_codex_executable(&args.codex)?;
    let runtime_root = paths.runtime_dir.join("codex");
    let provider_config = if args.discover_threads {
        CodexProviderConfig::discovering(provider_id, runtime_root)?
    } else {
        CodexProviderConfig::new(provider_id, runtime_root, identity.thread_ids.clone())?
    }
    .with_codex_executable(codex_executable.clone());
    let provider_parts = CodexProvider::build(provider_config)?;
    let provider_handle = provider_parts.handle().clone();
    let (provider_port, provider_source, _) = provider_parts.into_parts();
    let authorizer = Arc::new(FileCredentialAuthorizer::new(store.clone()));
    let constructor = if direct.is_some() {
        NativeChannelConfig::authenticated_public
    } else {
        NativeChannelConfig::authenticated_lan
    };
    let native_config = constructor(
        channel_id,
        SocketAddr::new(bind_ip, native_port),
        identity.tls_identity()?,
        authorizer,
    )?;
    let native_parts = NativeChannel::build(native_config)?;
    let native_handle = native_parts.handle().clone();
    let (native_port, native_source, _) = native_parts.into_parts();
    let mut host = RuntimeHost::with_session_config(SessionAggregateConfig::retain_all());
    host.register_provider(provider_port, provider_source)?;
    host.register_channel(native_port, native_source)?;
    host.start()?;
    let native_address = native_handle
        .snapshot()
        .local_address
        .ok_or("Native endpoint did not publish its listening address")?;
    let mut admin = AdminListener::bind(&paths.admin_socket)?;
    let stop_requested = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stop_requested);
    ctrlc::set_handler(move || signal_stop.store(true, Ordering::Release))?;
    let relay_endpoint = relay_settings
        .as_ref()
        .map(|settings| settings.endpoint.to_string());
    let relay_runtime = relay_settings
        .as_ref()
        .map(|_| RelayRuntimeState::new("starting"));
    let relay_thread = relay_settings.map(|settings| {
        spawn_relay_connector(
            store.clone(),
            identity.host_id.clone(),
            native_address,
            settings,
            Arc::clone(&stop_requested),
            relay_runtime
                .as_ref()
                .cloned()
                .unwrap_or_else(|| RelayRuntimeState::new("unavailable")),
        )
    });
    let mut status = RuntimeStatus {
        host_id: identity.host_id.clone(),
        host_name: identity.host_name.clone(),
        server_name: identity.server_name.clone(),
        native_address,
        codex_executable: Some(codex_executable.clone()),
        codex_remote_uri: provider_handle.remote_uri().to_owned(),
        provider_health: health_name(provider_handle.snapshot().health()).to_owned(),
        native_health: native_health_name(native_handle.snapshot().health).to_owned(),
        direct: direct.clone(),
        relay_endpoint,
        relay_health: relay_runtime
            .as_ref()
            .map_or_else(|| "disabled".to_owned(), |state| state.snapshot().health),
        relay_last_error: None,
        relay_ready_routes: Vec::new(),
        pid: std::process::id(),
    };
    write_status(&paths.status_file, &status)?;
    let mdns = if direct.is_some() || native_address.ip().is_loopback() {
        None
    } else {
        Some(publish_mdns(
            &identity.host_id,
            &identity.host_name,
            &identity.server_name,
            native_address,
        )?)
    };
    println!("AgentPulse Host '{}' is ready.", identity.host_name);
    println!(
        "Native WSS: wss://{}:{}/agentpulse/native/v3",
        identity.server_name,
        native_address.port()
    );
    println!(
        "Codex: {} --remote {}",
        codex_executable.display(),
        provider_handle.remote_uri()
    );
    println!("Pair a phone in another terminal: agentpulse pair");

    let loop_result = run_admin_loop(
        &mut admin,
        &stop_requested,
        &mut status,
        &provider_handle,
        &native_handle,
        relay_runtime.as_ref(),
        &paths.status_file,
    );
    stop_requested.store(true, Ordering::Release);
    if let Some(relay_connector) = relay_thread
        && let Err(error) = relay_connector.cancel_and_join(RELAY_CONNECTOR_SHUTDOWN_TIMEOUT)
    {
        eprintln!("warning: {error}");
    }
    let stop_result = host.stop();
    if let Some(mdns) = mdns {
        let _ = mdns.shutdown();
    }
    loop_result?;
    let _ = stop_result?;
    Ok(())
}

fn run_admin_loop(
    listener: &mut AdminListener,
    stop_requested: &AtomicBool,
    status: &mut RuntimeStatus,
    provider: &agentpulse_provider_codex::CodexProviderHandle,
    native: &agentpulse_channel_native::NativeChannelHandle,
    relay: Option<&RelayRuntimeState>,
    status_file: &Path,
) -> AppResult<()> {
    while !stop_requested.load(Ordering::Acquire) {
        status.provider_health = health_name(provider.snapshot().health()).to_owned();
        status.native_health = native_health_name(native.snapshot().health).to_owned();
        if let Some(relay) = relay {
            let relay = relay.snapshot();
            status.relay_health = relay.health;
            status.relay_last_error = relay.last_error;
            status.relay_ready_routes = relay.ready_routes;
        }
        write_status(status_file, status)?;
        if provider.snapshot().health() == CodexProviderHealth::Failed {
            return Err(provider
                .snapshot()
                .last_error()
                .unwrap_or("Codex Provider failed")
                .to_owned()
                .into());
        }
        if native.snapshot().health == NativeChannelHealth::Failed {
            return Err(native
                .snapshot()
                .last_error
                .unwrap_or_else(|| "Native Channel failed".to_owned())
                .into());
        }
        match listener.accept() {
            Ok(mut stream) => {
                if let Err(error) = handle_admin(&mut stream, stop_requested, status, provider) {
                    eprintln!("warning: admin client failed: {error}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn handle_admin(
    stream: &mut AdminStream,
    stop_requested: &AtomicBool,
    status: &RuntimeStatus,
    provider: &agentpulse_provider_codex::CodexProviderHandle,
) -> AppResult<()> {
    let payload = stream.receive(Duration::from_secs(2))?;
    let request = serde_json::from_slice::<AdminRequest>(&payload);
    let mut response = match request {
        Ok(AdminRequest::Status) => AdminResponse {
            ok: true,
            status: Some(status.clone()),
            codex_launch: None,
            error: None,
        },
        Ok(AdminRequest::CodexLaunch) => AdminResponse {
            ok: true,
            status: None,
            codex_launch: Some(CodexLaunchCredentials {
                remote_uri: provider.remote_uri().to_owned(),
                auth_token: provider.remote_auth_token().map(str::to_owned),
                executable: status.codex_executable.clone(),
            }),
            error: None,
        },
        Ok(AdminRequest::Stop) => {
            stop_requested.store(true, Ordering::Release);
            AdminResponse {
                ok: true,
                status: Some(status.clone()),
                codex_launch: None,
                error: None,
            }
        }
        Err(error) => AdminResponse {
            ok: false,
            status: None,
            codex_launch: None,
            error: Some(error.to_string()),
        },
    };
    let payload = Zeroizing::new(serde_json::to_vec(&response)?);
    if let Some(credentials) = response.codex_launch.as_mut()
        && let Some(token) = credentials.auth_token.as_mut()
    {
        token.zeroize();
    }
    stream.send(payload.as_slice(), Duration::from_secs(2))?;
    Ok(())
}

fn codex(paths: &HostPaths, args: CodexArgs) -> AppResult<()> {
    let credentials = request_admin(paths, AdminRequest::CodexLaunch)?
        .codex_launch
        .ok_or("running Host did not return Codex launch credentials")?;
    let executable = if args.codex == Path::new("codex") {
        credentials.executable.clone().unwrap_or(args.codex)
    } else {
        args.codex
    };
    let auth_token = credentials.auth_token.map(Zeroizing::new);
    let mut command = Command::new(agentpulse_provider_codex::resolve_codex_executable(
        &executable,
    )?);
    configure_codex_command(
        &mut command,
        &credentials.remote_uri,
        auth_token.as_ref().map(|token| token.as_str()),
        &args.arguments,
    );
    let mut child = command.spawn()?;
    drop(command);
    let exit = child.wait()?;
    if !exit.success() {
        return Err(format!("Codex exited with {exit}").into());
    }
    Ok(())
}

fn configure_codex_command(
    command: &mut Command,
    remote_uri: &str,
    auth_token: Option<&str>,
    arguments: &[String],
) {
    command.arg("--remote").arg(remote_uri);
    if let Some(token) = auth_token {
        command
            .arg("--remote-auth-token-env")
            .arg(CODEX_PROXY_TOKEN_ENV)
            .env(CODEX_PROXY_TOKEN_ENV, token);
    }
    command.args(arguments);
}

fn pair(paths: &HostPaths) -> AppResult<()> {
    let status = request_admin(paths, AdminRequest::Status)?
        .status
        .ok_or("running Host did not return status")?;
    if let Some(settings) = &status.direct {
        let session = PairingSession::bind_direct(
            paths.store(),
            settings.pairing_bind(),
            settings.pairing_endpoint.tuple(),
            settings.native_endpoint.tuple(),
            NATIVE_TRANSPORT_VERSION,
            vec![V2_PROTOCOL_VERSION],
        )?;
        let approval_deadline = Instant::now() + session.remaining_lifetime();
        println!("Pairing expires in two minutes. Scan this QR code:");
        println!("{}", terminal_qr(session.pairing_uri())?);
        let outcome = session.serve(|request| {
            let display_name = request.display_name.clone();
            let client_id = request.client_id.clone();
            let (sender, receiver) = mpsc::sync_channel(1);
            thread::spawn(move || {
                let _ = sender.send(approve_device(&display_name, &client_id));
            });
            receiver
                .recv_timeout(approval_deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(false)
        })?;
        println!(
            "Paired '{}' ({}) successfully.",
            outcome.display_name, outcome.client_id
        );
        return Ok(());
    }
    let settings = load_relay_settings(&paths.relay_config, &status.host_id)?
        .ok_or("QR pairing requires a configured Relay; run `agentpulse relay configure` first")?;
    let session = PairingSession::bind(
        paths.store(),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        status.native_address,
        settings.endpoint.to_string(),
        NATIVE_TRANSPORT_VERSION,
        vec![V2_PROTOCOL_VERSION],
    )?;
    eprintln!(
        "agentpulse pairing: session listener_port={} bundle_port={}",
        session.local_address().port(),
        session.bundle().port
    );
    let pairing_root = device_root_from_token(&session.bundle().bootstrap_token);
    let route = derive_route(&pairing_root, &settings.endpoint)?.registration();
    let relay_config = RelayHostConnectionConfig::new(
        settings.endpoint.clone(),
        status.host_id,
        settings.enrollment_token.as_str(),
        session.local_address(),
    )?;
    let relay_stop = Arc::new(AtomicBool::new(false));
    let connector_stop = Arc::clone(&relay_stop);
    let (ready_sender, ready_receiver) = mpsc::sync_channel::<Result<(), String>>(1);
    let _connector = thread::spawn(move || {
        let routes = [route];
        let backoff_seconds = [1_u64, 2, 5];
        let mut backoff_index = 0_usize;
        let mut announced = false;
        while !connector_stop.load(Ordering::Acquire) {
            let signal = ready_sender.clone();
            let became_waiting = Arc::new(AtomicBool::new(false));
            let callback_state = Arc::clone(&became_waiting);
            let result = connect_host_once_with_route_check_and_waiting(
                &relay_config,
                &routes,
                &connector_stop,
                || true,
                move || {
                    callback_state.store(true, Ordering::Release);
                    let _ = signal.try_send(Ok(()));
                },
            );
            if became_waiting.load(Ordering::Acquire) {
                announced = true;
                backoff_index = 0;
            } else if !announced {
                let _ = ready_sender.try_send(Err(result.err().map_or_else(
                    || "Relay pairing route closed unexpectedly".to_owned(),
                    |error| error.to_string(),
                )));
                break;
            }
            if connector_stop.load(Ordering::Acquire) {
                break;
            }
            let delay = backoff_seconds[backoff_index];
            backoff_index = (backoff_index + 1).min(backoff_seconds.len() - 1);
            sleep_until_stopped(&connector_stop, Duration::from_secs(delay));
        }
    });
    match ready_receiver.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(format!("could not publish QR pairing route: {error}").into()),
        Err(_) => return Err("timed out publishing QR pairing route".into()),
    }
    println!("Pairing expires in two minutes. Scan this QR code:");
    println!("{}", terminal_qr(session.pairing_uri())?);
    let result = session.serve_with_ready(
        |request| approve_device(&request.display_name, &request.client_id),
        |token| {
            let route = derive_route(&device_root_from_token(token), &settings.endpoint)
                .map_err(|error| agentpulse_pairing::PairingError::InvalidField {
                    field: "relay_route",
                    reason: error.to_string(),
                })?
                .registration();
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if request_admin(paths, AdminRequest::Status)
                    .ok()
                    .and_then(|response| response.status)
                    .is_some_and(|status| status.relay_ready_routes.contains(&route.route_id))
                {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(agentpulse_pairing::PairingError::InvalidField {
                field: "relay_route",
                reason: "timed out waiting for device route registration".to_owned(),
            })
        },
    );
    relay_stop.store(true, Ordering::Release);
    let outcome = result?;
    println!(
        "Paired '{}' ({}) successfully.",
        outcome.display_name, outcome.client_id
    );
    Ok(())
}

fn approve_device(display_name: &str, client_id: &str) -> bool {
    print!("Approve device '{display_name}' ({client_id})? [y/N] ");
    if std::io::stdout().flush().is_err() {
        return false;
    }
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .is_ok_and(|_| matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn devices(paths: &HostPaths, command: DevicesCommand) -> AppResult<()> {
    let store = paths.store();
    match command {
        DevicesCommand::List => {
            let devices = store.devices()?;
            if devices.is_empty() {
                println!("No paired devices.");
            } else {
                for device in devices {
                    println!(
                        "{}\t{}\t{}\t{}",
                        device.client_id,
                        device.display_name,
                        device.version.as_deref().unwrap_or("-"),
                        device.paired_at_unix_seconds
                    );
                }
            }
        }
        DevicesCommand::Revoke { client_id } => {
            store.revoke_device(&client_id)?;
            println!("Revoked device {client_id}.");
        }
    }
    Ok(())
}

fn credentials(paths: &HostPaths, command: CredentialsCommand) -> AppResult<()> {
    let _guard = acquire_stopped_lock(paths)?;
    match command {
        CredentialsCommand::Rotate { confirm_revoke_all } => {
            if !confirm_revoke_all {
                return Err("credential rotation requires --confirm-revoke-all".into());
            }
            let identity = paths.store().rotate_credentials()?;
            println!(
                "Rotated credentials for '{}' and revoked every device.",
                identity.host_name
            );
        }
    }
    Ok(())
}

fn relay(paths: &HostPaths, command: RelayCommand) -> AppResult<()> {
    let store = paths.store();
    let identity = store.load_identity()?;
    match command {
        RelayCommand::Configure {
            endpoint,
            token_stdin,
        } => {
            let _guard = acquire_stopped_lock(paths)?;
            if !token_stdin {
                return Err(
                    "Relay configuration requires --token-stdin; pipe the Token through standard input"
                        .into(),
                );
            }
            let mut input = String::new();
            std::io::stdin().take(130).read_to_string(&mut input)?;
            let token = input.trim_end_matches(['\r', '\n']);
            if token.len() > 128 || token.trim() != token {
                return Err("Relay enrollment Token is malformed".into());
            }
            let _ = RelayHostConnectionConfig::new(
                endpoint.clone(),
                identity.host_id,
                token,
                SocketAddr::from(([127, 0, 0, 1], 1)),
            )?;
            save_relay_settings(&paths.relay_config, &endpoint, token)?;
            println!("Configured optional Relay endpoint {endpoint}.");
            println!("Restart `agentpulse serve` to enable the public path.");
        }
        RelayCommand::Status => {
            match load_relay_settings(&paths.relay_config, &identity.host_id)? {
                Some(settings) => println!("Relay endpoint: {}", settings.endpoint),
                None => println!("Relay is disabled."),
            }
        }
        RelayCommand::Disable { confirm } => {
            let _guard = acquire_stopped_lock(paths)?;
            if !confirm {
                return Err("disabling Relay requires --confirm".into());
            }
            match fs::remove_file(&paths.relay_config) {
                Ok(()) => println!("Disabled the optional Relay path."),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    println!("Relay is already disabled.");
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn load_relay_settings(path: &Path, host_id: &str) -> AppResult<Option<RelayHostSettings>> {
    match protect_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let record = serde_json::from_slice::<RelayHostConfigRecord>(&bytes)?;
    if record.schema_version != RELAY_HOST_CONFIG_SCHEMA_VERSION {
        return Err(format!(
            "unsupported Relay Host configuration schema {}",
            record.schema_version
        )
        .into());
    }
    let _ = RelayHostConnectionConfig::new(
        record.endpoint.clone(),
        host_id,
        &record.enrollment_token,
        SocketAddr::from(([127, 0, 0, 1], 1)),
    )?;
    Ok(Some(RelayHostSettings {
        endpoint: record.endpoint,
        enrollment_token: Zeroizing::new(record.enrollment_token),
    }))
}

fn save_relay_settings(path: &Path, endpoint: &RelayEndpoint, token: &str) -> AppResult<()> {
    let record = RelayHostConfigRecord {
        schema_version: RELAY_HOST_CONFIG_SCHEMA_VERSION,
        endpoint: endpoint.clone(),
        enrollment_token: token.to_owned(),
    };
    atomic_write_private(path, &serde_json::to_vec_pretty(&record)?)?;
    Ok(())
}

fn relay_routes(
    store: &HostCredentialStore,
    endpoint: &RelayEndpoint,
) -> AppResult<Vec<RouteRegistration>> {
    let mut routes = store
        .device_credential_digests()?
        .into_iter()
        .map(|device| {
            derive_route(device.token_sha256(), endpoint).map(|route| route.registration())
        })
        .collect::<Result<Vec<_>, _>>()?;
    routes.sort_by(|left, right| left.route_id.cmp(&right.route_id));
    Ok(routes)
}

fn spawn_relay_connector(
    store: HostCredentialStore,
    host_id: String,
    native_address: SocketAddr,
    settings: RelayHostSettings,
    stop: Arc<AtomicBool>,
    runtime: RelayRuntimeState,
) -> RelayConnector {
    spawn_relay_connector_with(
        store,
        host_id,
        native_address,
        settings,
        stop,
        runtime,
        |config, routes, stop, current, ready| {
            connect_host_once_with_route_check_and_waiting(config, routes, stop, current, ready)
        },
    )
}

fn spawn_relay_connector_with<F>(
    store: HostCredentialStore,
    host_id: String,
    native_address: SocketAddr,
    settings: RelayHostSettings,
    stop: Arc<AtomicBool>,
    runtime: RelayRuntimeState,
    connect: F,
) -> RelayConnector
where
    F: Fn(
            &RelayHostConnectionConfig,
            &[RouteRegistration],
            &AtomicBool,
            &dyn Fn() -> bool,
            &dyn Fn(),
        ) -> Result<agentpulse_relay::RelayTunnelStats, RelayError>
        + Send
        + Sync
        + 'static,
{
    let connect = Arc::new(connect);
    let canceller = RelayConnectionCanceller::new();
    let worker_canceller = canceller.clone();
    let worker = thread::spawn(move || {
        let config = match RelayHostConnectionConfig::new(
            settings.endpoint.clone(),
            host_id,
            settings.enrollment_token.as_str(),
            native_address,
        ) {
            Ok(config) => config.with_connection_canceller(worker_canceller),
            Err(error) => {
                runtime.update("failed", Some(error.to_string()));
                return;
            }
        };
        // Keep a separate registration per device. A newly paired device must
        // not wait for another route's 15-second heartbeat or active tunnel.
        let mut workers: HashMap<String, (Arc<AtomicBool>, thread::JoinHandle<()>)> =
            HashMap::new();
        while !stop.load(Ordering::Acquire) {
            match relay_routes(&store, &settings.endpoint) {
                Ok(routes) => {
                    for (id, (device_stop, _)) in &workers {
                        if !routes.iter().any(|route| &route.route_id == id) {
                            device_stop.store(true, Ordering::Release);
                            runtime.unready(id, None);
                        }
                    }
                    let finished = workers
                        .iter()
                        .filter(|(_, (_, worker))| worker.is_finished())
                        .map(|(id, _)| id.clone())
                        .collect::<Vec<_>>();
                    for id in finished {
                        if let Some((_, worker)) = workers.remove(&id) {
                            let _ = worker.join();
                        }
                    }
                    if routes.is_empty() {
                        runtime.update("waiting_for_device", None);
                    }
                    for route in routes {
                        // The store permits 16 devices; allow bounded retiring
                        // workers during revocation/re-pairing as well.
                        if workers.contains_key(&route.route_id) || workers.len() >= 32 {
                            continue;
                        }
                        let device_stop = Arc::new(AtomicBool::new(false));
                        let worker_stop = Arc::clone(&device_stop);
                        let device_config = config.clone();
                        let device_runtime = runtime.clone();
                        let device_store = store.clone();
                        let endpoint = settings.endpoint.clone();
                        let id = route.route_id.clone();
                        let connect = Arc::clone(&connect);
                        let worker = thread::spawn(move || {
                            let routes = [route];
                            let backoff_seconds = [1_u64, 2, 5, 10, 30];
                            let mut backoff_index = 0_usize;
                            while !worker_stop.load(Ordering::Acquire) {
                                let result = connect(
                                    &device_config,
                                    &routes,
                                    &worker_stop,
                                    &|| {
                                        relay_routes(&device_store, &endpoint)
                                            .is_ok_and(|current| current.contains(&routes[0]))
                                    },
                                    &|| {
                                        if !worker_stop.load(Ordering::Acquire) {
                                            device_runtime.ready(&routes);
                                        }
                                    },
                                );
                                device_runtime.unready(
                                    &routes[0].route_id,
                                    result.as_ref().err().map(ToString::to_string),
                                );
                                match result {
                                    Ok(_) | Err(RelayError::RoutesChanged) => backoff_index = 0,
                                    Err(RelayError::Stopped) => break,
                                    Err(_) => {
                                        let delay = backoff_seconds[backoff_index];
                                        backoff_index =
                                            (backoff_index + 1).min(backoff_seconds.len() - 1);
                                        sleep_until_stopped(
                                            &worker_stop,
                                            Duration::from_secs(delay),
                                        );
                                    }
                                }
                            }
                        });
                        workers.insert(id, (device_stop, worker));
                    }
                }
                Err(error) => runtime.update("retrying", Some(error.to_string())),
            }
            sleep_until_stopped(&stop, Duration::from_millis(100));
        }
        for (device_stop, _) in workers.values() {
            device_stop.store(true, Ordering::Release);
        }
        // The owner cancels all sockets before joining this supervisor.
        for (_, (_, worker)) in workers {
            let _ = worker.join();
        }
        runtime.update("stopped", None);
    });
    RelayConnector { canceller, worker }
}

fn sleep_until_stopped(stop: &AtomicBool, duration: Duration) {
    let steps = duration.as_millis().div_ceil(100);
    for _ in 0..steps {
        if stop.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn print_status(paths: &HostPaths) -> AppResult<()> {
    match request_admin(paths, AdminRequest::Status) {
        Ok(response) => {
            let status = response.status.ok_or("Host status was unavailable")?;
            println!("Host: {} ({})", status.host_name, status.host_id);
            println!(
                "Native: {} at {}",
                status.native_health, status.native_address
            );
            println!("Provider: {}", status.provider_health);
            if let Some(direct) = &status.direct {
                println!(
                    "Direct pairing: {}:{}; Native: {}:{}",
                    direct.pairing_endpoint.host,
                    direct.pairing_endpoint.port,
                    direct.native_endpoint.host,
                    direct.native_endpoint.port
                );
            }
            match status.relay_endpoint {
                Some(endpoint) => {
                    println!("Relay: {} at {endpoint}", status.relay_health);
                    if let Some(error) = status.relay_last_error {
                        println!("Relay last error: {error}");
                    }
                }
                None => println!("Relay: disabled"),
            }
            println!("Host PID: {}", status.pid);
            if let Some(executable) = status.codex_executable {
                println!("Codex executable: {}", executable.display());
            }
            println!("Codex remote: {}", status.codex_remote_uri);
        }
        Err(_) => println!("AgentPulse Host is stopped."),
    }
    Ok(())
}

fn stop(paths: &HostPaths) -> AppResult<()> {
    let response = request_admin(paths, AdminRequest::Stop)?;
    if !response.ok {
        return Err(response
            .error
            .unwrap_or_else(|| "stop failed".to_owned())
            .into());
    }
    println!("AgentPulse Host is stopping.");
    Ok(())
}

fn request_admin(paths: &HostPaths, request: AdminRequest) -> AppResult<AdminResponse> {
    let mut stream = AdminStream::connect(&paths.admin_socket, Duration::from_secs(3))
        .map_err(|error| format!("cannot connect to AgentPulse Host: {error}"))?;
    stream.send(&serde_json::to_vec(&request)?, Duration::from_secs(3))?;
    let bytes = Zeroizing::new(stream.receive(Duration::from_secs(3))?);
    let response: AdminResponse = serde_json::from_slice(bytes.as_slice())?;
    if response.ok {
        Ok(response)
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "admin request failed".to_owned())
            .into())
    }
}

fn acquire_serve_lock(paths: &HostPaths) -> AppResult<File> {
    let lock = open_private_lock(&paths.lock_file)?;
    lock.try_lock_exclusive()
        .map_err(|_| "AgentPulse Host is already running")?;
    Ok(lock)
}

fn acquire_stopped_lock(paths: &HostPaths) -> AppResult<File> {
    acquire_serve_lock(paths)
        .map_err(|_| "stop AgentPulse Host before changing private configuration".into())
}

fn write_status(path: &Path, status: &RuntimeStatus) -> AppResult<()> {
    atomic_write_private(path, &serde_json::to_vec_pretty(status)?)?;
    Ok(())
}

fn cleanup_runtime_files(paths: &HostPaths) {
    for path in [&paths.status_file] {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("warning: failed to remove {}: {error}", path.display());
        }
    }
}

struct StatusFileCleanup<'a>(&'a HostPaths);
impl Drop for StatusFileCleanup<'_> {
    fn drop(&mut self) {
        cleanup_runtime_files(self.0);
    }
}

fn publish_mdns(
    host_id: &str,
    host_name: &str,
    server_name: &str,
    address: SocketAddr,
) -> AppResult<ServiceDaemon> {
    let daemon = ServiceDaemon::new()?;
    let properties = HashMap::from([
        ("host_id".to_owned(), host_id.to_owned()),
        ("host_name".to_owned(), host_name.to_owned()),
        ("protocol".to_owned(), "1".to_owned()),
        ("server_name".to_owned(), server_name.to_owned()),
    ]);
    let instance = format!("AgentPulse-{}", &host_id[..8]);
    let service = ServiceInfo::new(
        "_agentpulse._tcp.local.",
        &instance,
        &format!("{}.", server_name.trim_end_matches('.')),
        address.ip(),
        address.port(),
        properties,
    )?;
    daemon.register(service)?;
    Ok(daemon)
}

fn select_private_ip() -> AppResult<IpAddr> {
    let mut addresses = if_addrs::get_if_addrs()?
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|address| validate_private_ip(*address).is_ok())
        .collect::<BTreeSet<_>>();
    match addresses.len() {
        1 => addresses
            .pop_first()
            .ok_or_else(|| "private address disappeared".into()),
        0 => Err("no private LAN address found; pass --bind after connecting to a LAN".into()),
        _ => Err(format!(
            "multiple private LAN addresses found: {}; choose one with --bind",
            addresses
                .into_iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
    }
}

fn validate_private_ip(address: IpAddr) -> AppResult<()> {
    let valid = match address {
        IpAddr::V4(address) => address.is_private() || address.is_link_local(),
        IpAddr::V6(address) => address.is_unique_local() || address.is_unicast_link_local(),
    };
    if valid {
        Ok(())
    } else {
        Err(format!("{address} is not a private or link-local LAN address").into())
    }
}

fn health_name(health: CodexProviderHealth) -> &'static str {
    match health {
        CodexProviderHealth::Stopped => "stopped",
        CodexProviderHealth::Starting => "starting",
        CodexProviderHealth::Running => "running",
        CodexProviderHealth::Failed => "failed",
        _ => "unknown",
    }
}

fn native_health_name(health: NativeChannelHealth) -> &'static str {
    match health {
        NativeChannelHealth::Stopped => "stopped",
        NativeChannelHealth::Listening => "listening",
        NativeChannelHealth::Connected => "connected",
        NativeChannelHealth::Failed => "failed",
        _ => "unknown",
    }
}

#[allow(dead_code)]
fn _supported_codex_version() -> &'static str {
    SUPPORTED_CODEX_CLI_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short_test_root() -> PathBuf {
        let uuid = uuid::Uuid::now_v7().simple().to_string();
        let suffix = &uuid[uuid.len() - 16..];
        #[cfg(unix)]
        let temporary = Path::new("/tmp");
        #[cfg(windows)]
        let temporary = std::env::temp_dir();
        temporary.join(format!("aph{suffix}"))
    }

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn paths() -> AppResult<(Self, HostPaths)> {
            let root = short_test_root();
            let paths = HostPaths::resolve(Some(root.clone()))?;
            paths.ensure()?;
            Ok((Self(root), paths))
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn instance_lock_excludes_another_host_and_releases_on_drop() -> AppResult<()> {
        let (_directory, paths) = TestDirectory::paths()?;
        let lock = acquire_serve_lock(&paths)?;
        assert!(acquire_serve_lock(&paths).is_err());
        assert!(acquire_stopped_lock(&paths).is_err());
        drop(lock);
        let _next = acquire_serve_lock(&paths)?;
        Ok(())
    }

    #[test]
    fn admin_rejects_bad_json_then_serves_status_and_stop() -> AppResult<()> {
        let (_directory, paths) = TestDirectory::paths()?;
        let mut listener = AdminListener::bind(&paths.admin_socket)?;
        let status: RuntimeStatus = serde_json::from_value(serde_json::json!({
            "host_id": "test", "host_name": "Windows Host", "server_name": "test.local",
            "native_address": "127.0.0.1:49320", "codex_executable": null,
            "codex_remote_uri": "test://placeholder", "provider_health": "starting",
            "native_health": "starting", "relay_endpoint": null, "relay_health": "disabled",
            "relay_last_error": null, "pid": std::process::id()
        }))?;
        write_status(&paths.status_file, &status)?;
        write_status(&paths.status_file, &status)?;
        let provider = CodexProvider::build(CodexProviderConfig::discovering(
            ProviderId::new(),
            paths.runtime_dir.join("admin-test-codex"),
        )?)?
        .handle()
        .clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stopped = Arc::clone(&stopped);
        let worker = thread::spawn(move || -> Result<(), String> {
            for _ in 0..4 {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok(stream) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                };
                handle_admin(&mut stream, &server_stopped, &status, &provider)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        let mut bad = AdminStream::connect(&paths.admin_socket, Duration::from_secs(2))?;
        bad.send(b"not json", Duration::from_secs(2))?;
        let response: AdminResponse =
            serde_json::from_slice(&bad.receive(Duration::from_secs(2))?)?;
        assert!(!response.ok);
        drop(bad);
        let response = request_admin(&paths, AdminRequest::Status)?;
        assert_eq!(
            response.status.map(|s| s.host_name),
            Some("Windows Host".to_owned())
        );
        let launch = request_admin(&paths, AdminRequest::CodexLaunch)?
            .codex_launch
            .ok_or("admin omitted Codex launch credentials")?;
        #[cfg(windows)]
        assert!(launch.auth_token.is_some());
        #[cfg(not(windows))]
        assert!(launch.auth_token.is_none());
        assert!(!launch.remote_uri.is_empty());
        assert!(!stopped.load(Ordering::Acquire));
        assert!(request_admin(&paths, AdminRequest::Stop)?.ok);
        worker.join().map_err(|_| "admin worker panicked")??;
        assert!(stopped.load(Ordering::Acquire));
        cleanup_runtime_files(&paths);
        assert!(!paths.status_file.exists());
        Ok(())
    }

    #[test]
    fn codex_launch_keeps_proxy_token_out_of_arguments() {
        let token = "proxy-token-must-not-appear-in-arguments";
        let forwarded = vec!["--no-alt-screen".to_owned()];
        let mut command = Command::new("codex.exe");
        configure_codex_command(
            &mut command,
            "ws://127.0.0.1:49152",
            Some(token),
            &forwarded,
        );

        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "--remote",
                "ws://127.0.0.1:49152",
                "--remote-auth-token-env",
                CODEX_PROXY_TOKEN_ENV,
                "--no-alt-screen",
            ]
        );
        assert!(arguments.iter().all(|argument| argument != token));
        let injected = command
            .get_envs()
            .find(|(name, _)| *name == CODEX_PROXY_TOKEN_ENV)
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(injected.as_deref(), Some(token));
    }

    #[test]
    fn serve_uses_stable_native_port_by_default() -> AppResult<()> {
        let cli = Cli::try_parse_from(["agentpulse", "serve", "--bind", "192.168.50.4"])?;
        let args = match cli.command {
            HostCommand::Serve(args) => args,
            _ => return Err("serve command was not selected".into()),
        };
        assert_eq!(args.port, None);
        Ok(())
    }

    #[test]
    fn serve_accepts_explicit_native_port_override() -> AppResult<()> {
        let cli = Cli::try_parse_from([
            "agentpulse",
            "serve",
            "--bind",
            "192.168.50.4",
            "--port",
            "49321",
        ])?;
        let args = match cli.command {
            HostCommand::Serve(args) => args,
            _ => return Err("serve command was not selected".into()),
        };
        assert_eq!(args.port, Some(49_321));
        Ok(())
    }

    #[test]
    fn newly_paired_device_registers_while_existing_device_is_connected() -> AppResult<()> {
        let (_directory, paths) = TestDirectory::paths()?;
        let store = paths.store();
        let identity = store.initialize("Pairing readiness")?;
        let first = uuid::Uuid::now_v7().to_string();
        let second = uuid::Uuid::now_v7().to_string();
        store.issue_device(&first, "First", None)?;
        let endpoint: RelayEndpoint = "relay.example.com:2333".parse()?;
        let initial_routes = relay_routes(&store, &endpoint)?;
        let stop = Arc::new(AtomicBool::new(false));
        let runtime = RelayRuntimeState::new("starting");
        let (sender, receiver) = mpsc::channel();
        let connector = spawn_relay_connector_with(
            store.clone(),
            identity.host_id,
            SocketAddr::from(([127, 0, 0, 1], 49320)),
            RelayHostSettings {
                endpoint: endpoint.clone(),
                enrollment_token: Zeroizing::new("test-enrollment".to_owned()),
            },
            Arc::clone(&stop),
            runtime.clone(),
            move |_, routes, device_stop, _, ready| {
                ready();
                let _ = sender.send(routes[0].route_id.clone());
                // Represents an existing waiting registration or active tunnel.
                while !device_stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(RelayError::Stopped)
            },
        );
        let result = (|| -> AppResult<()> {
            assert_eq!(
                receiver.recv_timeout(Duration::from_secs(2))?,
                initial_routes[0].route_id
            );
            store.issue_device(&second, "Second", None)?;
            let new_id = receiver.recv_timeout(Duration::from_secs(2))?;
            assert_ne!(new_id, initial_routes[0].route_id);
            assert_eq!(runtime.snapshot().ready_routes.len(), 2);
            store.revoke_device(&second)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while runtime.snapshot().ready_routes.contains(&new_id) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                runtime.snapshot().ready_routes,
                vec![initial_routes[0].route_id.clone()]
            );
            Ok(())
        })();
        stop.store(true, Ordering::Release);
        connector.cancel_and_join(Duration::from_secs(2))?;
        result
    }

    #[test]
    fn relay_connector_join_is_bounded() -> AppResult<()> {
        let (release_sender, release_receiver) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let worker = thread::spawn(move || {
            let _ = release_receiver.recv();
            worker_finished.store(true, Ordering::Release);
        });
        let connector = RelayConnector {
            canceller: RelayConnectionCanceller::new(),
            worker,
        };

        let started = Instant::now();
        assert!(
            connector
                .cancel_and_join(Duration::from_millis(20))
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        release_sender.send(())?;
        let deadline = Instant::now() + Duration::from_secs(1);
        while !finished.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(finished.load(Ordering::Acquire));
        Ok(())
    }
}
