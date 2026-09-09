//! A Codex Provider with live observation and command/file approval write-back.
//!
//! The Provider owns a local Codex App Server (Unix socket or Windows loopback),
//! either resumes an explicit
//! set of threads or follows threads opened by another client of that same
//! server, strictly validates the schema-pinned protocol, and publishes
//! normalized live session events through `agentpulse-bridge`.

mod approval;
mod config;
mod control;
mod error;
mod executable;
mod mapper;
mod port;
mod protocol;
mod runtime;
mod status;

use std::sync::{Arc, Mutex};

use agentpulse_core::{NonEmptyText, ProviderCapabilities, ProviderDescriptor, ProviderKind};

pub use config::CodexProviderConfig;
pub use error::{CodexProviderBuildError, CodexProviderPortError, CodexProviderSourceError};
pub use executable::resolve_codex_executable;
pub use port::CodexProviderPort;
pub use runtime::CodexProviderSource;
pub use status::{CodexProviderHealth, CodexProviderSnapshot};

use approval::ApprovalRuntimeState;
use control::ControlRuntimeState;
use mapper::CodexEventMapper;
use protocol::ProtocolSchema;
use status::{SharedStatus, snapshot};

/// Preferred current Codex CLI version for this Provider release.
pub const SUPPORTED_CODEX_CLI_VERSION: &str = "0.153.0";

/// Explicit Codex CLI versions verified against the bundled schema and fixtures.
pub const SUPPORTED_CODEX_CLI_VERSIONS: &[&str] = &["0.150.1", "0.152.0", "0.152.1", "0.153.0"];

pub(crate) const SUPPORTED_CODEX_CLI_VERSION_REQUIREMENT: &str =
    "0.150.1, 0.152.0, 0.152.1, 0.153.0, or a valid version newer than 0.153.0";

/// SHA-256 of the official experimental `0.153.0` schema bundle.
pub const BUNDLED_CODEX_SCHEMA_SHA256: &str =
    "b06f77062369d481a59cc70720c12b89cb9dd49c385863923262102d3ad6c978";

/// Thread-safe monitoring handle for a built Codex Provider.
#[derive(Clone)]
pub struct CodexProviderHandle {
    remote_uri: String,
    #[cfg(windows)]
    remote_auth_token: config::ProxyToken,
    status: SharedStatus,
}

impl CodexProviderHandle {
    /// Returns the private observing proxy endpoint for `codex --remote` clients.
    #[must_use]
    pub fn remote_uri(&self) -> &str {
        &self.remote_uri
    }

    /// Returns the bearer token required by a Windows loopback Proxy.
    ///
    /// Callers must pass this only through the launched Codex process environment.
    #[must_use]
    pub fn remote_auth_token(&self) -> Option<&str> {
        #[cfg(windows)]
        {
            Some(self.remote_auth_token.expose())
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Returns an atomic point-in-time status copy.
    #[must_use]
    pub fn snapshot(&self) -> CodexProviderSnapshot {
        snapshot(&self.status)
    }
}

/// The paired Port, Source, and monitoring handle produced by the factory.
pub struct CodexProviderParts {
    port: CodexProviderPort,
    source: CodexProviderSource,
    handle: CodexProviderHandle,
}

impl CodexProviderParts {
    /// Borrows the handle before the Port and Source are moved into RuntimeHost.
    #[must_use]
    pub const fn handle(&self) -> &CodexProviderHandle {
        &self.handle
    }

    /// Splits the complete Provider into RuntimeHost registration parts and a handle.
    #[must_use]
    pub fn into_parts(self) -> (CodexProviderPort, CodexProviderSource, CodexProviderHandle) {
        (self.port, self.source, self.handle)
    }
}

/// Factory for a schema-pinned managed Codex Provider.
pub struct CodexProvider;

impl CodexProvider {
    /// Validates configuration, compiles the bundled schema, and builds a paired Adapter.
    pub fn build(
        config: CodexProviderConfig,
    ) -> Result<CodexProviderParts, CodexProviderBuildError> {
        config.validate_build_settings()?;
        let schema = ProtocolSchema::compile()?;
        let descriptor = ProviderDescriptor::new(
            config.provider_id,
            ProviderKind::new("codex")?,
            NonEmptyText::new("Codex")?,
            ProviderCapabilities::SESSION_STATE
                | ProviderCapabilities::APPROVAL_REQUEST
                | ProviderCapabilities::APPROVAL_RESPONSE
                | ProviderCapabilities::USER_INPUT_REQUEST
                | ProviderCapabilities::USER_INPUT_RESPONSE
                | ProviderCapabilities::PROMPT_SUBMIT
                | ProviderCapabilities::CANCEL
                | ProviderCapabilities::CONTROL,
        )
        .with_version(NonEmptyText::new(SUPPORTED_CODEX_CLI_VERSION)?);
        let status = Arc::new(Mutex::new(Default::default()));
        let approvals = Arc::new(Mutex::new(ApprovalRuntimeState::new()));
        let controls = Arc::new(Mutex::new(ControlRuntimeState::new()));
        let mapper =
            CodexEventMapper::new(config.provider_id, &config.threads, config.discover_threads);
        let handle = CodexProviderHandle {
            remote_uri: config.remote_uri.clone(),
            #[cfg(windows)]
            remote_auth_token: config.proxy_token.clone(),
            status: Arc::clone(&status),
        };
        let source = CodexProviderSource::new(
            config,
            schema,
            mapper,
            status,
            Arc::clone(&approvals),
            Arc::clone(&controls),
        );
        Ok(CodexProviderParts {
            port: CodexProviderPort::new(descriptor, approvals, controls),
            source,
            handle,
        })
    }
}
