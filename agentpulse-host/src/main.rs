//! AgentPulse Host command-line application.

mod ble;

use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Write},
    net::{IpAddr, SocketAddr},
    os::unix::{fs::OpenOptionsExt, fs::PermissionsExt, net::UnixListener, net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use agentpulse_bridge::RuntimeHost;
use agentpulse_channel_native::{
    NATIVE_TRANSPORT_VERSION, NativeChannel, NativeChannelConfig, NativeChannelHealth,
};
use agentpulse_core::{ChannelId, ProviderId};
use agentpulse_pairing::{
    FileCredentialAuthorizer, HostCredentialStore, PairingSession, terminal_qr,
};
use agentpulse_protocol::V1_PROTOCOL_VERSION;
use agentpulse_provider_codex::{
    CodexProvider, CodexProviderConfig, CodexProviderHealth, SUPPORTED_CODEX_CLI_VERSION,
};
use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use fs2::FileExt;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{Deserialize, Serialize};

type AppResult<T> = Result<T, Box<dyn Error>>;
const DEFAULT_NATIVE_PORT: u16 = 49_320;

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
    /// Explicit private or link-local IP. Omit only when exactly one is available.
    #[arg(long)]
    bind: Option<IpAddr>,
    /// Stable authenticated Native WSS port advertised to paired clients.
    #[arg(long, default_value_t = DEFAULT_NATIVE_PORT)]
    port: u16,
    /// Codex executable to version-check and launch.
    #[arg(long, default_value = "codex")]
    codex: PathBuf,
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

#[derive(Clone, Debug)]
struct HostPaths {
    data_dir: PathBuf,
    credential_store: PathBuf,
    runtime_dir: PathBuf,
    lock_file: PathBuf,
    admin_socket: PathBuf,
    status_file: PathBuf,
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
            data_dir,
            runtime_dir,
        })
    }

    fn ensure(&self) -> AppResult<()> {
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(&self.runtime_dir)?;
        fs::set_permissions(&self.data_dir, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&self.runtime_dir, fs::Permissions::from_mode(0o700))?;
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
    codex_remote_uri: String,
    provider_health: String,
    native_health: String,
    pid: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum AdminRequest {
    Status,
    Stop,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminResponse {
    ok: bool,
    status: Option<RuntimeStatus>,
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
    prepare_admin_socket(paths)?;
    let store = paths.store();
    let identity = store.load_identity()?;
    if identity.thread_ids.is_empty() {
        return Err("no Codex threads configured; run `agentpulse threads add` first".into());
    }
    let bind_ip = match args.bind {
        Some(address) => {
            validate_private_ip(address)?;
            address
        }
        None => select_private_ip()?,
    };
    let provider_id = ProviderId::from_str(&identity.provider_id)?;
    let channel_id = ChannelId::from_str(&identity.channel_id)?;
    let provider_config = CodexProviderConfig::new(
        provider_id,
        paths.runtime_dir.join("codex"),
        identity.thread_ids.clone(),
    )?
    .with_codex_executable(args.codex.clone());
    let provider_parts = CodexProvider::build(provider_config)?;
    let provider_handle = provider_parts.handle().clone();
    let (provider_port, provider_source, _) = provider_parts.into_parts();
    let authorizer = Arc::new(FileCredentialAuthorizer::new(store.clone()));
    let native_config = NativeChannelConfig::authenticated_lan(
        channel_id,
        SocketAddr::new(bind_ip, args.port),
        identity.tls_identity()?,
        authorizer,
    )?;
    let native_parts = NativeChannel::build(native_config)?;
    let native_handle = native_parts.handle().clone();
    let (native_port, native_source, _) = native_parts.into_parts();
    let mut host = RuntimeHost::new();
    host.register_provider(provider_port, provider_source)?;
    host.register_channel(native_port, native_source)?;
    host.start()?;
    let native_address = native_handle
        .snapshot()
        .local_address
        .ok_or("Native endpoint did not publish its listening address")?;
    let admin = UnixListener::bind(&paths.admin_socket)?;
    fs::set_permissions(&paths.admin_socket, fs::Permissions::from_mode(0o600))?;
    admin.set_nonblocking(true)?;
    let stop_requested = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stop_requested);
    ctrlc::set_handler(move || signal_stop.store(true, Ordering::Release))?;
    let mut status = RuntimeStatus {
        host_id: identity.host_id.clone(),
        host_name: identity.host_name.clone(),
        server_name: identity.server_name.clone(),
        native_address,
        codex_remote_uri: provider_handle.remote_uri().to_owned(),
        provider_health: health_name(provider_handle.snapshot().health()).to_owned(),
        native_health: native_health_name(native_handle.snapshot().health).to_owned(),
        pid: std::process::id(),
    };
    write_status(&paths.status_file, &status)?;
    let mdns = publish_mdns(
        &identity.host_id,
        &identity.host_name,
        &identity.server_name,
        native_address,
    )?;
    println!("AgentPulse Host '{}' is ready.", identity.host_name);
    println!(
        "Native WSS: wss://{}:{}/agentpulse/native/v1",
        identity.server_name,
        native_address.port()
    );
    println!(
        "Codex: {} --remote {}",
        args.codex.display(),
        provider_handle.remote_uri()
    );
    println!("Pair a phone in another terminal: agentpulse pair");

    let loop_result = run_admin_loop(
        &admin,
        &stop_requested,
        &mut status,
        &provider_handle,
        &native_handle,
        &paths.status_file,
    );
    let stop_result = host.stop();
    let _ = mdns.shutdown();
    cleanup_runtime_files(paths);
    loop_result?;
    let _ = stop_result?;
    Ok(())
}

fn run_admin_loop(
    listener: &UnixListener,
    stop_requested: &AtomicBool,
    status: &mut RuntimeStatus,
    provider: &agentpulse_provider_codex::CodexProviderHandle,
    native: &agentpulse_channel_native::NativeChannelHandle,
    status_file: &Path,
) -> AppResult<()> {
    while !stop_requested.load(Ordering::Acquire) {
        status.provider_health = health_name(provider.snapshot().health()).to_owned();
        status.native_health = native_health_name(native.snapshot().health).to_owned();
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
            Ok((mut stream, _)) => handle_admin(&mut stream, stop_requested, status)?,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn handle_admin(
    stream: &mut UnixStream,
    stop_requested: &AtomicBool,
    status: &RuntimeStatus,
) -> AppResult<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut payload = Vec::new();
    stream.take(64 * 1024).read_to_end(&mut payload)?;
    let request = serde_json::from_slice::<AdminRequest>(&payload);
    let response = match request {
        Ok(AdminRequest::Status) => AdminResponse {
            ok: true,
            status: Some(status.clone()),
            error: None,
        },
        Ok(AdminRequest::Stop) => {
            stop_requested.store(true, Ordering::Release);
            AdminResponse {
                ok: true,
                status: Some(status.clone()),
                error: None,
            }
        }
        Err(error) => AdminResponse {
            ok: false,
            status: None,
            error: Some(error.to_string()),
        },
    };
    stream.write_all(&serde_json::to_vec(&response)?)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

fn codex(paths: &HostPaths, args: CodexArgs) -> AppResult<()> {
    let status = request_admin(paths, AdminRequest::Status)?
        .status
        .ok_or("running Host did not return status")?;
    let exit = Command::new(args.codex)
        .arg("--remote")
        .arg(status.codex_remote_uri)
        .args(args.arguments)
        .status()?;
    if !exit.success() {
        return Err(format!("Codex exited with {exit}").into());
    }
    Ok(())
}

fn pair(paths: &HostPaths) -> AppResult<()> {
    let status = request_admin(paths, AdminRequest::Status)?
        .status
        .ok_or("running Host did not return status")?;
    let session = PairingSession::bind(
        paths.store(),
        SocketAddr::new(status.native_address.ip(), 0),
        status.native_address,
        NATIVE_TRANSPORT_VERSION,
        vec![V1_PROTOCOL_VERSION],
    )?;
    println!("Pairing expires in two minutes. Scan this QR code:");
    println!("{}", terminal_qr(session.pairing_uri())?);
    println!("Pairing URI (manual fallback): {}", session.pairing_uri());
    let ble = match ble::BlePairingAdvertiser::start(session.pairing_uri()) {
        Ok(advertiser) => {
            println!(
                "BLE nearby pairing is active on service {}.",
                agentpulse_pairing::PAIRING_BLE_SERVICE_UUID
            );
            Some(advertiser)
        }
        Err(error) => {
            eprintln!("warning: BLE pairing is unavailable ({error}); use the QR code.");
            None
        }
    };
    let result = session.serve(|request| approve_device(&request.display_name, &request.client_id));
    drop(ble);
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
    let mut stream =
        UnixStream::connect(&paths.admin_socket).map_err(|_| "AgentPulse Host is not running")?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(&serde_json::to_vec(&request)?)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut bytes = Vec::new();
    BufReader::new(stream).read_to_end(&mut bytes)?;
    let response: AdminResponse = serde_json::from_slice(&bytes)?;
    if response.ok {
        Ok(response)
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "admin request failed".to_owned())
            .into())
    }
}

fn prepare_admin_socket(paths: &HostPaths) -> AppResult<()> {
    if !paths.admin_socket.exists() {
        return Ok(());
    }
    if UnixStream::connect(&paths.admin_socket).is_ok() {
        return Err("AgentPulse Host is already running".into());
    }
    fs::remove_file(&paths.admin_socket)?;
    Ok(())
}

fn acquire_serve_lock(paths: &HostPaths) -> AppResult<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&paths.lock_file)?;
    lock.try_lock_exclusive()
        .map_err(|_| "AgentPulse Host is already running")?;
    Ok(lock)
}

fn acquire_stopped_lock(paths: &HostPaths) -> AppResult<File> {
    acquire_serve_lock(paths).map_err(|_| "stop AgentPulse Host before rotating credentials".into())
}

fn write_status(path: &Path, status: &RuntimeStatus) -> AppResult<()> {
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(status)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true).mode(0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn cleanup_runtime_files(paths: &HostPaths) {
    for path in [&paths.admin_socket, &paths.status_file] {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("warning: failed to remove {}: {error}", path.display());
        }
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

    #[test]
    fn serve_uses_stable_native_port_by_default() -> AppResult<()> {
        let cli = Cli::try_parse_from(["agentpulse", "serve", "--bind", "192.168.50.4"])?;
        let args = match cli.command {
            HostCommand::Serve(args) => args,
            _ => return Err("serve command was not selected".into()),
        };
        assert_eq!(args.port, DEFAULT_NATIVE_PORT);
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
        assert_eq!(args.port, 49_321);
        Ok(())
    }
}
