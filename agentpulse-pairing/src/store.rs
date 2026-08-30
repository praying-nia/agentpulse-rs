//! Strict atomic Host identity and device-credential storage.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use agentpulse_transport::{BearerTokenAuthorizer, TlsServerIdentity};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use fs2::FileExt;
use rand::Rng;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::PairingError;

const STORE_SCHEMA_VERSION: u16 = 1;
const MAX_DEVICES: usize = 16;
const LEAF_LIFETIME_DAYS: i64 = 90;
const LEAF_RENEWAL_DAYS: i64 = 14;
const CA_LIFETIME_DAYS: i64 = 3650;

/// Public and TLS identity needed by Host services.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostIdentitySnapshot {
    /// Stable UUIDv7 Host identity.
    pub host_id: String,
    /// User-facing Host name.
    pub host_name: String,
    /// Stable UUIDv7 Provider identity.
    pub provider_id: String,
    /// Stable UUIDv7 Native Channel identity.
    pub channel_id: String,
    /// Stable certificate DNS name.
    pub server_name: String,
    /// Explicit Codex UUIDv7 threads.
    pub thread_ids: Vec<String>,
    /// Current leaf certificate DER.
    pub leaf_certificate_der: Vec<u8>,
    /// Current CA certificate DER.
    pub ca_certificate_der: Vec<u8>,
    /// Current leaf private key DER.
    pub leaf_private_key_der: Vec<u8>,
    /// Leaf expiry as UTC Unix seconds.
    pub leaf_not_after_unix_seconds: i64,
}

impl HostIdentitySnapshot {
    /// Builds the ordered leaf-and-CA chain used by rustls.
    pub fn tls_identity(&self) -> Result<TlsServerIdentity, PairingError> {
        Ok(TlsServerIdentity::from_der(
            vec![
                self.leaf_certificate_der.clone(),
                self.ca_certificate_der.clone(),
            ],
            self.leaf_private_key_der.clone(),
        )?)
    }

    /// Returns the lowercase SHA-256 of the current leaf certificate.
    #[must_use]
    pub fn leaf_sha256(&self) -> String {
        hex(&Sha256::digest(&self.leaf_certificate_der))
    }

    /// Returns the CA DER using standard padded Base64.
    #[must_use]
    pub fn ca_certificate_base64(&self) -> String {
        STANDARD.encode(&self.ca_certificate_der)
    }
}

/// Nonsecret paired-device information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceCredentialSummary {
    /// Stable Android installation identity.
    pub client_id: String,
    /// Last approved display name.
    pub display_name: String,
    /// Optional client build version.
    pub version: Option<String>,
    /// Pairing time as UTC Unix seconds.
    pub paired_at_unix_seconds: i64,
}

/// Versioned persistent Host identity and credential store.
#[derive(Clone, Debug)]
pub struct HostCredentialStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl HostCredentialStore {
    /// Opens a store at an explicit caller-owned path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let lock_path = path.with_extension("lock");
        Self { path, lock_path }
    }

    /// Returns the credential file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates a fresh identity without replacing an existing installation.
    pub fn initialize(&self, host_name: &str) -> Result<HostIdentitySnapshot, PairingError> {
        validate_name(host_name)?;
        self.ensure_parent()?;
        let lock = self.open_lock()?;
        lock.lock_exclusive()
            .map_err(|source| PairingError::io("lock", &self.lock_path, source))?;
        if self.path.exists() {
            return Err(PairingError::AlreadyInitialized {
                path: self.path.clone(),
            });
        }
        let host_id = Uuid::now_v7().to_string();
        let server_name = format!("{host_id}.agentpulse.local");
        let certificates = generate_identity(host_name, &server_name)?;
        let record = StoreRecord {
            schema_version: STORE_SCHEMA_VERSION,
            host_id,
            host_name: host_name.to_owned(),
            provider_id: Uuid::now_v7().to_string(),
            channel_id: Uuid::now_v7().to_string(),
            server_name,
            thread_ids: Vec::new(),
            certificates,
            devices: Vec::new(),
        };
        self.save_unlocked(&record)?;
        snapshot(&record)
    }

    /// Loads the identity and automatically renews a near-expiry leaf.
    pub fn load_identity(&self) -> Result<HostIdentitySnapshot, PairingError> {
        let record = self.update(|record| {
            if record.certificates.leaf_not_after_unix_seconds
                <= now_unix_seconds() + Duration::days(LEAF_RENEWAL_DAYS).whole_seconds()
            {
                renew_leaf(record)?;
            }
            Ok(())
        })?;
        snapshot(&record)
    }

    /// Replaces the explicit configured Codex thread list.
    pub fn set_thread_ids(&self, thread_ids: Vec<String>) -> Result<(), PairingError> {
        let mut unique = std::collections::BTreeSet::new();
        for thread_id in &thread_ids {
            validate_uuid_v7("thread_id", thread_id)?;
            if !unique.insert(thread_id.clone()) {
                return Err(PairingError::InvalidField {
                    field: "thread_id",
                    reason: format!("duplicate thread {thread_id}"),
                });
            }
        }
        let _ = self.update(|record| {
            record.thread_ids = thread_ids;
            Ok(())
        })?;
        Ok(())
    }

    /// Lists nonsecret device metadata in pairing order.
    pub fn devices(&self) -> Result<Vec<DeviceCredentialSummary>, PairingError> {
        Ok(self
            .load_record()?
            .devices
            .into_iter()
            .map(|device| DeviceCredentialSummary {
                client_id: device.client_id,
                display_name: device.display_name,
                version: device.version,
                paired_at_unix_seconds: device.paired_at_unix_seconds,
            })
            .collect())
    }

    /// Issues or replaces one device-specific bearer credential.
    pub fn issue_device(
        &self,
        client_id: &str,
        display_name: &str,
        version: Option<&str>,
    ) -> Result<String, PairingError> {
        validate_uuid_v7("client_id", client_id)?;
        validate_name(display_name)?;
        if let Some(version) = version {
            validate_bounded("version", version, 64)?;
        }
        let mut secret = Zeroizing::new([0_u8; 32]);
        rand::rng().fill_bytes(secret.as_mut());
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret.as_ref());
        let token_hash = hex(&Sha256::digest(token.as_bytes()));
        let _ = self.update(|record| {
            if let Some(device) = record
                .devices
                .iter_mut()
                .find(|device| device.client_id == client_id)
            {
                device.display_name = display_name.to_owned();
                device.version = version.map(str::to_owned);
                device.token_sha256 = token_hash.clone();
                device.paired_at_unix_seconds = now_unix_seconds();
                return Ok(());
            }
            if record.devices.len() >= MAX_DEVICES {
                return Err(PairingError::DeviceCapacity {
                    capacity: MAX_DEVICES,
                });
            }
            record.devices.push(DeviceRecord {
                client_id: client_id.to_owned(),
                display_name: display_name.to_owned(),
                version: version.map(str::to_owned),
                token_sha256: token_hash.clone(),
                paired_at_unix_seconds: now_unix_seconds(),
            });
            Ok(())
        })?;
        Ok(token)
    }

    /// Revokes one device credential.
    pub fn revoke_device(&self, client_id: &str) -> Result<(), PairingError> {
        let _ = self.update(|record| {
            let original = record.devices.len();
            record
                .devices
                .retain(|device| device.client_id != client_id);
            if original == record.devices.len() {
                return Err(PairingError::DeviceNotFound {
                    client_id: client_id.to_owned(),
                });
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Rotates the CA and leaf, revoking every device while retaining Host ID.
    pub fn rotate_credentials(&self) -> Result<HostIdentitySnapshot, PairingError> {
        let record = self.update(|record| {
            record.certificates = generate_identity(&record.host_name, &record.server_name)?;
            record.devices.clear();
            Ok(())
        })?;
        snapshot(&record)
    }

    fn authorize(&self, client_id: &str, token: &str) -> bool {
        let Ok(record) = self.load_record() else {
            return false;
        };
        let Some(device) = record
            .devices
            .iter()
            .find(|device| device.client_id == client_id)
        else {
            return false;
        };
        let supplied = Sha256::digest(token.as_bytes());
        let Some(expected) = decode_hex_32(&device.token_sha256) else {
            return false;
        };
        bool::from(supplied.as_slice().ct_eq(&expected))
    }

    fn update(
        &self,
        operation: impl FnOnce(&mut StoreRecord) -> Result<(), PairingError>,
    ) -> Result<StoreRecord, PairingError> {
        self.ensure_parent()?;
        let lock = self.open_lock()?;
        lock.lock_exclusive()
            .map_err(|source| PairingError::io("lock", &self.lock_path, source))?;
        let mut record = self.load_unlocked()?;
        operation(&mut record)?;
        validate_record(&record)?;
        self.save_unlocked(&record)?;
        Ok(record)
    }

    fn load_record(&self) -> Result<StoreRecord, PairingError> {
        self.ensure_parent()?;
        let lock = self.open_lock()?;
        lock.lock_shared()
            .map_err(|source| PairingError::io("lock", &self.lock_path, source))?;
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> Result<StoreRecord, PairingError> {
        if !self.path.exists() {
            return Err(PairingError::NotInitialized {
                path: self.path.clone(),
            });
        }
        let bytes =
            fs::read(&self.path).map_err(|source| PairingError::io("read", &self.path, source))?;
        let record: StoreRecord = serde_json::from_slice(&bytes)?;
        validate_record(&record)?;
        Ok(record)
    }

    fn save_unlocked(&self, record: &StoreRecord) -> Result<(), PairingError> {
        let bytes = serde_json::to_vec_pretty(record)?;
        let temporary = self.path.with_extension("tmp");
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|source| PairingError::io("create temporary", &temporary, source))?;
        file.write_all(&bytes)
            .map_err(|source| PairingError::io("write temporary", &temporary, source))?;
        file.sync_all()
            .map_err(|source| PairingError::io("sync temporary", &temporary, source))?;
        fs::rename(&temporary, &self.path)
            .map_err(|source| PairingError::io("replace", &self.path, source))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .map_err(|source| PairingError::io("set permissions on", &self.path, source))?;
        }
        if let Some(parent) = self.path.parent() {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| PairingError::io("sync directory", parent, source))?;
        }
        Ok(())
    }

    fn ensure_parent(&self) -> Result<(), PairingError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| PairingError::InvalidField {
                field: "credential_path",
                reason: "path has no parent".to_owned(),
            })?;
        fs::create_dir_all(parent)
            .map_err(|source| PairingError::io("create directory", parent, source))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|source| PairingError::io("set permissions on", parent, source))?;
        }
        Ok(())
    }

    fn open_lock(&self) -> Result<File, PairingError> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .map_err(|source| PairingError::io("open lock", &self.lock_path, source))
    }
}

/// File-backed authorizer that observes revocation without restarting the Host.
#[derive(Clone, Debug)]
pub struct FileCredentialAuthorizer {
    store: HostCredentialStore,
}

impl FileCredentialAuthorizer {
    /// Creates an authorizer for one initialized store.
    #[must_use]
    pub const fn new(store: HostCredentialStore) -> Self {
        Self { store }
    }
}

impl BearerTokenAuthorizer for FileCredentialAuthorizer {
    fn authorize(&self, client_id: &str, bearer_token: &str) -> bool {
        self.store.authorize(client_id, bearer_token)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreRecord {
    schema_version: u16,
    host_id: String,
    host_name: String,
    provider_id: String,
    channel_id: String,
    server_name: String,
    thread_ids: Vec<String>,
    certificates: CertificateRecord,
    devices: Vec<DeviceRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CertificateRecord {
    ca_certificate_der: String,
    ca_private_key_der: String,
    leaf_certificate_der: String,
    leaf_private_key_der: String,
    leaf_not_after_unix_seconds: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceRecord {
    client_id: String,
    display_name: String,
    version: Option<String>,
    token_sha256: String,
    paired_at_unix_seconds: i64,
}

fn generate_identity(
    host_name: &str,
    server_name: &str,
) -> Result<CertificateRecord, PairingError> {
    let now = OffsetDateTime::now_utc();
    let ca_key = KeyPair::generate()?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.distinguished_name.push(
        DnType::CommonName,
        format!("AgentPulse {host_name} Local CA"),
    );
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    ca_params.not_before = now - Duration::days(1);
    ca_params.not_after = now + Duration::days(CA_LIFETIME_DAYS);
    let ca_certificate = ca_params.self_signed(&ca_key)?;
    let (leaf_certificate_der, leaf_private_key_der, leaf_not_after_unix_seconds) =
        create_leaf(server_name, &ca_certificate, &ca_key)?;
    Ok(CertificateRecord {
        ca_certificate_der: STANDARD.encode(ca_certificate.der()),
        ca_private_key_der: STANDARD.encode(ca_key.serialize_der()),
        leaf_certificate_der: STANDARD.encode(leaf_certificate_der),
        leaf_private_key_der: STANDARD.encode(leaf_private_key_der),
        leaf_not_after_unix_seconds,
    })
}

fn renew_leaf(record: &mut StoreRecord) -> Result<(), PairingError> {
    let ca_der = STANDARD.decode(&record.certificates.ca_certificate_der)?;
    let ca_key_der = Zeroizing::new(STANDARD.decode(&record.certificates.ca_private_key_der)?);
    let ca_key = KeyPair::try_from(ca_key_der.as_slice())?;
    let ca_der = rustls::pki_types::CertificateDer::from(ca_der);
    let ca_params = CertificateParams::from_ca_cert_der(&ca_der)?;
    let ca_certificate = ca_params.self_signed(&ca_key)?;
    let (leaf_certificate_der, leaf_private_key_der, leaf_not_after_unix_seconds) =
        create_leaf(&record.server_name, &ca_certificate, &ca_key)?;
    record.certificates.leaf_certificate_der = STANDARD.encode(leaf_certificate_der);
    record.certificates.leaf_private_key_der = STANDARD.encode(leaf_private_key_der);
    record.certificates.leaf_not_after_unix_seconds = leaf_not_after_unix_seconds;
    Ok(())
}

fn create_leaf(
    server_name: &str,
    ca_certificate: &rcgen::Certificate,
    ca_key: &KeyPair,
) -> Result<(Vec<u8>, Vec<u8>, i64), PairingError> {
    let now = OffsetDateTime::now_utc();
    let not_after = now + Duration::days(LEAF_LIFETIME_DAYS);
    let leaf_key = KeyPair::generate()?;
    let mut leaf_params = CertificateParams::new(vec![server_name.to_owned()])?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, server_name);
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.not_before = now - Duration::days(1);
    leaf_params.not_after = not_after;
    let leaf_certificate = leaf_params.signed_by(&leaf_key, ca_certificate, ca_key)?;
    Ok((
        leaf_certificate.der().to_vec(),
        leaf_key.serialize_der(),
        not_after.unix_timestamp(),
    ))
}

fn snapshot(record: &StoreRecord) -> Result<HostIdentitySnapshot, PairingError> {
    Ok(HostIdentitySnapshot {
        host_id: record.host_id.clone(),
        host_name: record.host_name.clone(),
        provider_id: record.provider_id.clone(),
        channel_id: record.channel_id.clone(),
        server_name: record.server_name.clone(),
        thread_ids: record.thread_ids.clone(),
        leaf_certificate_der: STANDARD.decode(&record.certificates.leaf_certificate_der)?,
        ca_certificate_der: STANDARD.decode(&record.certificates.ca_certificate_der)?,
        leaf_private_key_der: STANDARD.decode(&record.certificates.leaf_private_key_der)?,
        leaf_not_after_unix_seconds: record.certificates.leaf_not_after_unix_seconds,
    })
}

fn validate_record(record: &StoreRecord) -> Result<(), PairingError> {
    if record.schema_version != STORE_SCHEMA_VERSION {
        return Err(PairingError::InvalidStore {
            message: format!("unsupported schema version {}", record.schema_version),
        });
    }
    validate_uuid_v7("host_id", &record.host_id)?;
    validate_uuid_v7("provider_id", &record.provider_id)?;
    validate_uuid_v7("channel_id", &record.channel_id)?;
    validate_name(&record.host_name)?;
    validate_bounded("server_name", &record.server_name, 253)?;
    if record.devices.len() > MAX_DEVICES {
        return Err(PairingError::InvalidStore {
            message: "device capacity exceeded".to_owned(),
        });
    }
    for thread_id in &record.thread_ids {
        validate_uuid_v7("thread_id", thread_id)?;
    }
    for device in &record.devices {
        validate_uuid_v7("client_id", &device.client_id)?;
        validate_name(&device.display_name)?;
        if decode_hex_32(&device.token_sha256).is_none() {
            return Err(PairingError::InvalidStore {
                message: "invalid device credential hash".to_owned(),
            });
        }
    }
    let _ = snapshot(record)?;
    Ok(())
}

fn validate_name(value: &str) -> Result<(), PairingError> {
    validate_bounded("display_name", value, 80)
}

fn validate_bounded(field: &'static str, value: &str, maximum: usize) -> Result<(), PairingError> {
    if value.trim().is_empty() || value.len() > maximum {
        Err(PairingError::InvalidField {
            field,
            reason: "must be nonblank and within its size limit".to_owned(),
        })
    } else {
        Ok(())
    }
}

fn validate_uuid_v7(field: &'static str, value: &str) -> Result<(), PairingError> {
    let uuid = Uuid::parse_str(value).map_err(|error| PairingError::InvalidField {
        field,
        reason: error.to_string(),
    })?;
    if uuid.get_version_num() != 7 {
        return Err(PairingError::InvalidField {
            field,
            reason: "must be UUIDv7".to_owned(),
        });
    }
    Ok(())
}

fn now_unix_seconds() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut output = [0_u8; 32];
    let bytes = value.as_bytes();
    for index in 0..32 {
        let high = digit(bytes[index * 2])?;
        let low = digit(bytes[index * 2 + 1])?;
        output[index] = (high << 4) | low;
    }
    Some(output)
}

fn digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use agentpulse_transport::BearerTokenAuthorizer;

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Result<Self, std::io::Error> {
            let path = std::env::temp_dir().join(format!("agentpulse-pairing-{}", Uuid::now_v7()));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }

        fn store(&self) -> HostCredentialStore {
            HostCredentialStore::new(self.0.join("credentials.json"))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn identity_is_stable_private_and_has_a_valid_tls_chain() -> TestResult {
        let directory = TestDirectory::create()?;
        let store = directory.store();
        let created = store.initialize("Studio Host")?;
        let loaded = store.load_identity()?;

        assert_eq!(created.host_id, loaded.host_id);
        assert_eq!(created.provider_id, loaded.provider_id);
        assert_eq!(created.channel_id, loaded.channel_id);
        assert_eq!(created.leaf_certificate_der, loaded.leaf_certificate_der);
        assert_eq!(created.leaf_sha256().len(), 64);
        let _ = loaded.tls_identity()?;

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&directory.0)?.permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(store.path())?.permissions().mode() & 0o777,
                0o600
            );
        }
        Ok(())
    }

    #[test]
    fn device_tokens_rotate_revoke_and_fail_closed() -> TestResult {
        let directory = TestDirectory::create()?;
        let store = directory.store();
        let original = store.initialize("Studio Host")?;
        let client_id = Uuid::now_v7().to_string();
        let token = store.issue_device(&client_id, "Pixel", Some("0.1.0"))?;
        let authorizer = FileCredentialAuthorizer::new(store.clone());

        assert!(authorizer.authorize(&client_id, &token));
        assert!(!authorizer.authorize(&client_id, "wrong-token"));
        assert_eq!(store.devices()?.len(), 1);

        store.revoke_device(&client_id)?;
        assert!(!authorizer.authorize(&client_id, &token));

        let replacement = store.issue_device(&client_id, "Pixel", None)?;
        let rotated = store.rotate_credentials()?;
        assert_eq!(rotated.host_id, original.host_id);
        assert_ne!(rotated.ca_certificate_der, original.ca_certificate_der);
        assert!(!authorizer.authorize(&client_id, &replacement));
        assert!(store.devices()?.is_empty());

        fs::write(store.path(), b"not-json")?;
        assert!(!authorizer.authorize(&client_id, &replacement));
        Ok(())
    }
}
