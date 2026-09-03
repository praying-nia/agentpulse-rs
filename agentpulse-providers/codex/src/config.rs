//! Validated Codex Provider configuration.

use std::{
    collections::BTreeSet,
    env,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use agentpulse_core::{ExternalId, ProviderId, SessionId};

use crate::{CodexProviderBuildError, SUPPORTED_CODEX_CLI_VERSION};

const PORTABLE_UNIX_SOCKET_PATH_MAX: usize = 96;
const SOCKET_FILE_NAME: &str = "app-server.sock";

#[derive(Clone, Debug)]
pub(crate) struct ConfiguredThread {
    pub(crate) external_id: ExternalId,
    pub(crate) session_id: SessionId,
}

/// Configuration for one managed read-only Codex Provider instance.
#[derive(Clone, Debug)]
pub struct CodexProviderConfig {
    pub(crate) provider_id: ProviderId,
    pub(crate) runtime_root: PathBuf,
    pub(crate) runtime_directory: PathBuf,
    pub(crate) socket_path: PathBuf,
    pub(crate) remote_uri: String,
    pub(crate) threads: Vec<ConfiguredThread>,
    pub(crate) discover_threads: bool,
    pub(crate) codex_executable: PathBuf,
    pub(crate) startup_timeout: Duration,
    pub(crate) shutdown_timeout: Duration,
}

impl CodexProviderConfig {
    /// Creates a configuration for explicit Codex thread IDs.
    ///
    /// Codex-generated thread IDs are UUIDv7 values. The same UUID is reused as
    /// the stable AgentPulse Session ID so stop/start cycles do not create
    /// duplicate sessions.
    pub fn new(
        provider_id: ProviderId,
        runtime_root: impl Into<PathBuf>,
        thread_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, CodexProviderBuildError> {
        let thread_ids = thread_ids.into_iter().map(Into::into).collect::<Vec<_>>();
        if thread_ids.is_empty() {
            return Err(CodexProviderBuildError::EmptyThreadList);
        }

        let mut unique = BTreeSet::new();
        let mut threads = Vec::with_capacity(thread_ids.len());
        for thread_id in thread_ids {
            if !unique.insert(thread_id.clone()) {
                return Err(CodexProviderBuildError::DuplicateThreadId { thread_id });
            }
            let session_id = SessionId::from_str(&thread_id).map_err(|source| {
                CodexProviderBuildError::InvalidThreadId {
                    thread_id: thread_id.clone(),
                    source,
                }
            })?;
            let external_id = ExternalId::new(thread_id)?;
            threads.push(ConfiguredThread {
                external_id,
                session_id,
            });
        }

        Self::finish(provider_id, runtime_root.into(), threads, false)
    }

    /// Creates an ephemeral configuration that follows threads started or resumed by
    /// another client of the same managed App Server.
    pub fn discovering(
        provider_id: ProviderId,
        runtime_root: impl Into<PathBuf>,
    ) -> Result<Self, CodexProviderBuildError> {
        Self::finish(provider_id, runtime_root.into(), Vec::new(), true)
    }

    fn finish(
        provider_id: ProviderId,
        runtime_root: PathBuf,
        threads: Vec<ConfiguredThread>,
        discover_threads: bool,
    ) -> Result<Self, CodexProviderBuildError> {
        let runtime_root = absolute_path(runtime_root)?;

        let runtime_directory = runtime_root.join(runtime_directory_key(provider_id));
        let socket_path = runtime_directory.join(SOCKET_FILE_NAME);
        let socket_text =
            socket_path
                .to_str()
                .ok_or_else(|| CodexProviderBuildError::NonUtf8RuntimeRoot {
                    path: runtime_root.clone(),
                })?;
        let length = socket_text.len();
        if length > PORTABLE_UNIX_SOCKET_PATH_MAX {
            return Err(CodexProviderBuildError::SocketPathTooLong {
                length,
                maximum: PORTABLE_UNIX_SOCKET_PATH_MAX,
            });
        }
        let remote_uri = format!("unix://{socket_text}");

        Ok(Self {
            provider_id,
            runtime_root,
            runtime_directory,
            socket_path,
            remote_uri,
            threads,
            discover_threads,
            codex_executable: PathBuf::from("codex"),
            startup_timeout: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(5),
        })
    }

    /// Overrides the Codex executable used for version probing and App Server launch.
    #[must_use]
    pub fn with_codex_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.codex_executable = executable.into();
        self
    }

    /// Overrides the App Server readiness deadline.
    #[must_use]
    pub const fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Overrides the Source shutdown deadline.
    #[must_use]
    pub const fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Returns the configured Provider identity.
    #[must_use]
    pub const fn provider_id(&self) -> ProviderId {
        self.provider_id
    }

    /// Returns the preferred current Codex CLI version.
    #[must_use]
    pub const fn supported_codex_version(&self) -> &'static str {
        SUPPORTED_CODEX_CLI_VERSION
    }

    /// Returns the deterministic App Server endpoint shown to `codex --remote`.
    #[must_use]
    pub fn remote_uri(&self) -> &str {
        &self.remote_uri
    }

    /// Returns the caller-owned runtime root. The Provider never removes it.
    #[must_use]
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    pub(crate) fn validate_build_settings(&self) -> Result<(), CodexProviderBuildError> {
        if self.codex_executable.as_os_str().is_empty() {
            return Err(CodexProviderBuildError::EmptyExecutable);
        }
        if self.startup_timeout.is_zero() {
            return Err(CodexProviderBuildError::ZeroTimeout {
                field: "startup timeout",
            });
        }
        if self.shutdown_timeout.is_zero() {
            return Err(CodexProviderBuildError::ZeroTimeout {
                field: "shutdown timeout",
            });
        }
        Ok(())
    }
}

fn runtime_directory_key(provider_id: ProviderId) -> String {
    let bytes = provider_id.as_uuid().as_bytes();
    // UUIDv7 reserves the first 48 bits for time and fixes the version/variant
    // bits. Encoding only rand_a and rand_b retains all 74 random bits while
    // keeping the Unix socket path stable and substantially shorter.
    format!(
        "{:01x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[6] & 0x0f,
        bytes[7],
        bytes[8] & 0x3f,
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, CodexProviderBuildError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| CodexProviderBuildError::CurrentDirectory {
                message: error.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, str::FromStr};

    use super::*;

    const PROVIDER_ID: &str = "0198f1b7-7212-7a34-9678-90abcdef1234";
    const THREAD_ID: &str = "01a04932-b445-77f1-85aa-9e8304f16c71";

    #[test]
    fn normal_desktop_host_path_uses_stable_short_random_key() -> Result<(), Box<dyn Error>> {
        let provider_id = ProviderId::from_str(PROVIDER_ID)?;
        let runtime_root = "/home/desktop-user/.config/agentpulse/runtime/codex";

        let first = CodexProviderConfig::new(provider_id, runtime_root, [THREAD_ID])?;
        let second = CodexProviderConfig::new(provider_id, runtime_root, [THREAD_ID])?;

        assert_eq!(
            first
                .runtime_directory
                .file_name()
                .and_then(|key| key.to_str()),
            Some("a34167890abcdef1234")
        );
        assert_eq!(first.runtime_directory, second.runtime_directory);
        assert_eq!(first.socket_path, second.socket_path);
        assert!(first.socket_path.as_os_str().len() <= PORTABLE_UNIX_SOCKET_PATH_MAX);
        Ok(())
    }

    #[test]
    fn genuinely_long_runtime_root_still_rejects_socket_path() -> Result<(), Box<dyn Error>> {
        let provider_id = ProviderId::from_str(PROVIDER_ID)?;
        let runtime_root = PathBuf::from(format!("/{}", "x".repeat(80)));

        let Err(error) = CodexProviderConfig::new(provider_id, runtime_root, [THREAD_ID]) else {
            return Err("a genuinely long socket path must be rejected".into());
        };

        assert!(matches!(
            error,
            CodexProviderBuildError::SocketPathTooLong {
                length,
                maximum,
            } if maximum == PORTABLE_UNIX_SOCKET_PATH_MAX && length > maximum
        ));
        Ok(())
    }

    #[test]
    fn discovering_config_has_no_persistent_thread_binding() -> Result<(), Box<dyn Error>> {
        let provider_id = ProviderId::from_str(PROVIDER_ID)?;
        let config = CodexProviderConfig::discovering(provider_id, "/tmp/agentpulse")?;

        assert!(config.threads.is_empty());
        assert!(config.discover_threads);
        Ok(())
    }
}
