//! Command-line entry point for the public AgentPulse Relay.

use std::{
    error::Error,
    io,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use agentpulse_relay::{
    RelayEndpoint, RelayServer, RelayServerConfig, host_authentication_key, new_server_config,
    probe,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::{Parser, Subcommand};
use rand::Rng as _;
use zeroize::Zeroizing;

#[derive(Debug, Parser)]
#[command(name = "agentpulse-relay", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a private server configuration and print its enrollment Token once.
    Init {
        /// Destination JSON configuration file, which must not already exist.
        #[arg(long)]
        config: PathBuf,
        /// Public listener address, for example 0.0.0.0:2333.
        #[arg(long)]
        bind: SocketAddr,
        /// Canonical public DNS authority, such as relay.example.com:2333.
        #[arg(long)]
        public_endpoint: RelayEndpoint,
        /// Full leaf-plus-intermediate PEM certificate chain.
        #[arg(long)]
        certificate_chain: PathBuf,
        /// Matching private-key PEM file.
        #[arg(long)]
        private_key: PathBuf,
        /// UUIDv7 of the only Host allowed to register routes.
        #[arg(long)]
        host_id: String,
    },
    /// Validate configuration and serve the Relay until interrupted.
    Serve {
        /// Private JSON configuration file.
        #[arg(long)]
        config: PathBuf,
    },
    /// Validate configuration, certificate name, key match, and expiry.
    CheckConfig {
        /// Private JSON configuration file.
        #[arg(long)]
        config: PathBuf,
    },
    /// Verify that an endpoint presents trusted TLS and a Relay v1 challenge.
    Probe {
        /// Canonical public DNS authority.
        #[arg(long)]
        endpoint: RelayEndpoint,
    },
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("agentpulse-relay: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Command::Init {
            config,
            bind,
            public_endpoint,
            certificate_chain,
            private_key,
            host_id,
        } => {
            if config.try_exists()? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("configuration already exists: {}", config.display()),
                )
                .into());
            }
            let mut token_bytes = Zeroizing::new([0_u8; 32]);
            rand::rng().fill_bytes(token_bytes.as_mut());
            let enrollment_token = Zeroizing::new(URL_SAFE_NO_PAD.encode(token_bytes.as_ref()));
            let authentication_key = host_authentication_key(enrollment_token.as_str());
            let relay_config = new_server_config(
                bind,
                public_endpoint,
                certificate_chain,
                private_key,
                host_id,
                &authentication_key,
            )?;
            let (_, certificate) = relay_config.tls_server_config()?;
            relay_config.save(&config)?;
            println!("Relay configuration: {}", config.display());
            println!("Public endpoint: {}", relay_config.public_endpoint);
            println!(
                "Certificate expires at Unix timestamp: {}",
                certificate.not_after_unix_seconds
            );
            println!("Host enrollment Token: {}", enrollment_token.as_str());
            println!("Store this Token now; it will not be shown again.");
        }
        Command::Serve { config } => {
            let config = RelayServerConfig::load(config)?;
            let server = RelayServer::bind(config)?;
            let stop = Arc::new(AtomicBool::new(false));
            let signal_stop = Arc::clone(&stop);
            ctrlc::set_handler(move || signal_stop.store(true, Ordering::Release))?;
            println!("AgentPulse Relay listening on {}", server.local_address());
            println!(
                "Certificate expires at Unix timestamp: {}",
                server.certificate_status().not_after_unix_seconds
            );
            server.run(stop)?;
        }
        Command::CheckConfig { config } => {
            let config = RelayServerConfig::load(config)?;
            let (_, certificate) = config.tls_server_config()?;
            println!("Relay configuration is valid.");
            println!("Bind address: {}", config.bind_address);
            println!("Public endpoint: {}", config.public_endpoint);
            println!(
                "Certificate expires at Unix timestamp: {}",
                certificate.not_after_unix_seconds
            );
            let remaining_days = certificate.remaining_seconds / (24 * 60 * 60);
            println!("Certificate remaining whole days: {remaining_days}");
            if certificate.remaining_seconds < 14 * 24 * 60 * 60 {
                eprintln!("WARNING: Relay certificate expires in less than 14 days.");
            }
        }
        Command::Probe { endpoint } => {
            probe(&endpoint)?;
            println!("Relay TLS and v1 challenge are healthy: {endpoint}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_requires_an_explicit_subcommand() {
        assert!(Cli::try_parse_from(["agentpulse-relay"]).is_err());
    }

    #[test]
    fn init_never_has_a_default_config_path() {
        assert!(
            Cli::try_parse_from([
                "agentpulse-relay",
                "init",
                "--bind",
                "0.0.0.0:2333",
                "--public-endpoint",
                "relay.example.com:2333",
                "--certificate-chain",
                "/tmp/cert.pem",
                "--private-key",
                "/tmp/key.pem",
                "--host-id",
                "018f10a0-fd57-7c08-bb2a-9b61c761a62f",
            ])
            .is_err()
        );
    }
}
