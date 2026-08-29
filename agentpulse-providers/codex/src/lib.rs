//! A complete read-only Codex Provider backed by a managed App Server.
//!
//! The Provider owns a Unix-socket Codex App Server, resumes an explicit set
//! of threads, strictly validates the version-pinned protocol, and publishes
//! normalized live session events through `agentpulse-bridge`.

mod config;
mod error;
mod mapper;
mod port;
mod protocol;
mod runtime;
mod status;

use std::sync::{Arc, Mutex};

use agentpulse_core::{NonEmptyText, ProviderCapabilities, ProviderDescriptor, ProviderKind};

pub use config::CodexProviderConfig;
pub use error::{CodexProviderBuildError, CodexProviderPortError, CodexProviderSourceError};
pub use port::CodexProviderPort;
pub use runtime::CodexProviderSource;
pub use status::{CodexProviderHealth, CodexProviderSnapshot};

use mapper::CodexEventMapper;
use protocol::ProtocolSchema;
use status::{SharedStatus, snapshot};

/// The only Codex CLI version accepted by this Provider release.
pub const SUPPORTED_CODEX_CLI_VERSION: &str = "0.150.1";

/// SHA-256 of the bundled official `0.150.1` App Server schema bundle.
pub const BUNDLED_CODEX_SCHEMA_SHA256: &str =
    "18ba0e2282f69f7b3a05ffdc8ab0801c1468f25d72de3b4a37f1c8be67432a1d";

/// Thread-safe monitoring handle for a built Codex Provider.
#[derive(Clone)]
pub struct CodexProviderHandle {
    remote_uri: String,
    status: SharedStatus,
}

impl CodexProviderHandle {
    /// Returns the shared App Server endpoint for `codex --remote` clients.
    #[must_use]
    pub fn remote_uri(&self) -> &str {
        &self.remote_uri
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

/// Factory for a version-pinned managed Codex Provider.
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
            ProviderCapabilities::SESSION_STATE,
        )
        .with_version(NonEmptyText::new(SUPPORTED_CODEX_CLI_VERSION)?);
        let status = Arc::new(Mutex::new(Default::default()));
        let mapper = CodexEventMapper::new(config.provider_id, &config.threads);
        let handle = CodexProviderHandle {
            remote_uri: config.remote_uri.clone(),
            status: Arc::clone(&status),
        };
        let source = CodexProviderSource::new(config, schema, mapper, status);
        Ok(CodexProviderParts {
            port: CodexProviderPort::new(descriptor),
            source,
            handle,
        })
    }
}
