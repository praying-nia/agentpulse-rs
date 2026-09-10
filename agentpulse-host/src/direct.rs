//! Explicit public endpoints, kept separate from local listening addresses.
use super::{AppResult, HostPaths, atomic_write_private};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};

#[derive(Args)]
pub(super) struct DirectArgs {
    #[command(subcommand)]
    pub command: DirectCommand,
}

#[derive(Subcommand)]
pub(super) enum DirectCommand {
    /// Saves explicit direct endpoints; takes effect on the next serve startup.
    Configure {
        #[arg(long)]
        bind: IpAddr,
        #[arg(long, default_value_t = 49320)]
        native_port: u16,
        #[arg(long, default_value_t = 49321)]
        pairing_port: u16,
        #[arg(long)]
        native_endpoint: String,
        #[arg(long)]
        pairing_endpoint: String,
    },
    /// Shows saved direct settings.
    Status,
    /// Removes direct settings for the next serve startup.
    Disable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Endpoint {
    pub host: String,
    pub port: u16,
}
impl Endpoint {
    fn parse(value: &str) -> AppResult<Self> {
        let (host, port) = if let Some(rest) = value.strip_prefix('[') {
            rest.split_once("]:")
                .ok_or("IPv6 endpoint must use [address]:port")?
        } else {
            let pair = value
                .rsplit_once(':')
                .ok_or("endpoint must use host:port")?;
            if pair.0.contains(':') {
                return Err("IPv6 endpoint must use [address]:port".into());
            }
            pair
        };
        let endpoint = Self {
            host: host.to_ascii_lowercase(),
            port: port.parse()?,
        };
        endpoint.validate()?;
        Ok(endpoint)
    }
    fn validate(&self) -> AppResult<()> {
        agentpulse_pairing::validate_direct_host(&self.host)?;
        if self.port == 0 {
            return Err("public port must be non-zero".into());
        }
        Ok(())
    }
    pub fn tuple(&self) -> (String, u16) {
        (self.host.clone(), self.port)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectSettings {
    schema_version: u16,
    pub bind: IpAddr,
    pub native_port: u16,
    pub pairing_port: u16,
    pub native_endpoint: Endpoint,
    pub pairing_endpoint: Endpoint,
}
impl DirectSettings {
    fn validate(&self) -> AppResult<()> {
        if self.schema_version != 1
            || self.bind.is_multicast()
            || self.bind.is_unspecified()
            || self.bind.is_loopback()
        {
            return Err("direct configuration requires version 1 and a concrete non-loopback unicast bind IP".into());
        }
        if self.native_port == 0 || self.pairing_port == 0 || self.native_port == self.pairing_port
        {
            return Err("direct native and pairing ports must be non-zero and distinct".into());
        }
        self.native_endpoint.validate()?;
        self.pairing_endpoint.validate()?;
        if self.native_endpoint.host == self.pairing_endpoint.host
            && self.native_endpoint.port == self.pairing_endpoint.port
        {
            return Err("public native and pairing endpoints must be distinct".into());
        }
        Ok(())
    }
    pub fn pairing_bind(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.pairing_port)
    }
}

pub(super) fn load(paths: &HostPaths) -> AppResult<Option<DirectSettings>> {
    let bytes = match std::fs::read(paths.data_dir.join("direct.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let settings: DirectSettings = serde_json::from_slice(&bytes)?;
    settings.validate()?;
    Ok(Some(settings))
}

pub(super) fn run(paths: &HostPaths, command: DirectCommand) -> AppResult<()> {
    match command {
        DirectCommand::Configure {
            bind,
            native_port,
            pairing_port,
            native_endpoint,
            pairing_endpoint,
        } => {
            let settings = DirectSettings {
                schema_version: 1,
                bind,
                native_port,
                pairing_port,
                native_endpoint: Endpoint::parse(&native_endpoint)?,
                pairing_endpoint: Endpoint::parse(&pairing_endpoint)?,
            };
            settings.validate()?;
            atomic_write_private(
                &paths.data_dir.join("direct.json"),
                &serde_json::to_vec_pretty(&settings)?,
            )?;
            println!("Direct settings saved. Restart agentpulse serve to apply them.");
        }
        DirectCommand::Status => match load(paths)? {
            Some(settings) => println!("{}", serde_json::to_string_pretty(&settings)?),
            None => println!("Direct pairing is disabled."),
        },
        DirectCommand::Disable => {
            // Existing listeners keep their effective configuration until stopped.
            match std::fs::remove_file(paths.data_dir.join("direct.json")) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            println!("Direct settings removed. Restart agentpulse serve to apply this change.");
        }
    }
    Ok(())
}
