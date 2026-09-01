//! Persistent Relay server configuration and TLS identity loading.

use std::{
    fs,
    io::BufReader,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;
use x509_parser::{extensions::GeneralName, parse_x509_certificate};
use zeroize::Zeroizing;

use crate::{RelayEndpoint, RelayError};

/// Current on-disk Relay configuration schema.
pub const RELAY_CONFIG_SCHEMA_VERSION: u16 = 1;

/// Strict configuration for one single-Host Relay deployment.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayServerConfig {
    /// On-disk schema version, currently `1`.
    pub schema_version: u16,
    /// Public listener address; wildcard binds are allowed only here.
    pub bind_address: SocketAddr,
    /// Public DNS authority presented by the TLS certificate.
    pub public_endpoint: RelayEndpoint,
    /// Full leaf-plus-intermediate PEM chain.
    pub certificate_chain: PathBuf,
    /// Matching PKCS#8, PKCS#1, or SEC1 PEM private key.
    pub private_key: PathBuf,
    /// Only Host UUIDv7 allowed to register routes.
    pub host_id: String,
    /// Base64URL SHA-256 of the one-time Host enrollment Token.
    pub host_authentication_key: String,
}

/// Validated certificate metadata printed by checks and deployment probes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateStatus {
    /// Certificate DNS name required by this configuration.
    pub server_name: String,
    /// Leaf expiry as a UTC Unix timestamp.
    pub not_after_unix_seconds: i64,
    /// Whole remaining seconds at validation time.
    pub remaining_seconds: i64,
}

impl RelayServerConfig {
    /// Reads and strictly decodes a configuration file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, RelayError> {
        let bytes = fs::read(path.as_ref())
            .map_err(|source| RelayError::io("read Relay configuration", source))?;
        let config = serde_json::from_slice::<Self>(&bytes)?;
        config.validate()?;
        Ok(config)
    }

    /// Atomically writes a new private Relay configuration.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), RelayError> {
        self.validate()?;
        let path = path.as_ref();
        let parent = path
            .parent()
            .ok_or_else(|| RelayError::invalid("config", "path has no parent"))?;
        fs::create_dir_all(parent)
            .map_err(|source| RelayError::io("create Relay configuration directory", source))?;
        let temporary = path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(self)?;
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|source| RelayError::io("create temporary Relay configuration", source))?;
        use std::io::Write as _;
        file.write_all(&bytes)
            .map_err(|source| RelayError::io("write temporary Relay configuration", source))?;
        file.sync_all()
            .map_err(|source| RelayError::io("sync temporary Relay configuration", source))?;
        fs::rename(&temporary, path)
            .map_err(|source| RelayError::io("replace Relay configuration", source))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|source| RelayError::io("set Relay configuration permissions", source))?;
        }
        Ok(())
    }

    /// Validates nonsecret fields and key encodings without opening sockets.
    pub fn validate(&self) -> Result<(), RelayError> {
        if self.schema_version != RELAY_CONFIG_SCHEMA_VERSION {
            return Err(RelayError::invalid("schema_version", "unsupported version"));
        }
        if self.bind_address.port() == 0 {
            return Err(RelayError::invalid("bind_address", "port must be non-zero"));
        }
        let host_id = Uuid::parse_str(&self.host_id)
            .map_err(|error| RelayError::invalid("host_id", error.to_string()))?;
        if host_id.get_version_num() != 7 {
            return Err(RelayError::invalid("host_id", "must be UUIDv7"));
        }
        decode_key(&self.host_authentication_key)?;
        if self.certificate_chain.as_os_str().is_empty() || self.private_key.as_os_str().is_empty()
        {
            return Err(RelayError::invalid(
                "tls",
                "certificate and key paths are required",
            ));
        }
        Ok(())
    }

    /// Returns the decoded Host proof key while keeping debug output redacted.
    pub fn host_authentication_key(&self) -> Result<Zeroizing<[u8; 32]>, RelayError> {
        Ok(Zeroizing::new(decode_key(&self.host_authentication_key)?))
    }

    /// Loads, matches, and validates the configured TLS certificate and key.
    pub fn tls_server_config(&self) -> Result<(Arc<ServerConfig>, CertificateStatus), RelayError> {
        self.validate()?;
        let certificate_bytes = fs::read(&self.certificate_chain)
            .map_err(|source| RelayError::io("read Relay certificate chain", source))?;
        let mut certificate_reader = BufReader::new(certificate_bytes.as_slice());
        let certificates = rustls_pemfile::certs(&mut certificate_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| RelayError::Tls {
                message: error.to_string(),
            })?;
        let leaf = certificates
            .first()
            .ok_or_else(|| RelayError::invalid("certificate_chain", "contains no certificate"))?;
        let (_, parsed) =
            parse_x509_certificate(leaf.as_ref()).map_err(|error| RelayError::Tls {
                message: format!("cannot parse leaf certificate: {error}"),
            })?;
        let names = parsed
            .subject_alternative_name()
            .map_err(|error| RelayError::Tls {
                message: format!("cannot parse certificate SAN: {error}"),
            })?
            .ok_or_else(|| RelayError::invalid("certificate", "leaf has no SAN"))?;
        let matches_name = names.value.general_names.iter().any(|name| {
            matches!(name, GeneralName::DNSName(value) if value.eq_ignore_ascii_case(self.public_endpoint.host()))
        });
        if !matches_name {
            return Err(RelayError::invalid(
                "certificate",
                "SAN does not contain the configured public DNS name",
            ));
        }
        let not_after_unix_seconds = parsed.validity().not_after.timestamp();
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if not_after_unix_seconds <= now {
            return Err(RelayError::invalid(
                "certificate",
                "leaf certificate is expired",
            ));
        }

        let key_bytes = fs::read(&self.private_key)
            .map_err(|source| RelayError::io("read Relay private key", source))?;
        let mut key_reader = BufReader::new(key_bytes.as_slice());
        let private_key = rustls_pemfile::private_key(&mut key_reader)
            .map_err(|error| RelayError::Tls {
                message: error.to_string(),
            })?
            .ok_or_else(|| RelayError::invalid("private_key", "contains no private key"))?;
        let private_key =
            PrivateKeyDer::try_from(private_key.secret_der().to_vec()).map_err(|error| {
                RelayError::Tls {
                    message: error.to_string(),
                }
            })?;
        let server = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .map_err(|error| RelayError::Tls {
                message: error.to_string(),
            })?;
        Ok((
            Arc::new(server),
            CertificateStatus {
                server_name: self.public_endpoint.host().to_owned(),
                not_after_unix_seconds,
                remaining_seconds: not_after_unix_seconds - now,
            },
        ))
    }
}

fn decode_key(value: &str) -> Result<[u8; 32], RelayError> {
    let bytes = URL_SAFE_NO_PAD.decode(value)?;
    bytes
        .try_into()
        .map_err(|_| RelayError::invalid("host_authentication_key", "must contain 32 bytes"))
}

/// Builds one configuration from validated initialization inputs.
pub fn new_server_config(
    bind_address: SocketAddr,
    public_endpoint: RelayEndpoint,
    certificate_chain: PathBuf,
    private_key: PathBuf,
    host_id: String,
    host_authentication_key: &[u8; 32],
) -> Result<RelayServerConfig, RelayError> {
    let config = RelayServerConfig {
        schema_version: RELAY_CONFIG_SCHEMA_VERSION,
        bind_address,
        public_endpoint,
        certificate_chain,
        private_key,
        host_id,
        host_authentication_key: URL_SAFE_NO_PAD.encode(host_authentication_key),
    };
    config.validate()?;
    Ok(config)
}
