// SPDX-License-Identifier: MPL-2.0

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use koru_session_protocol::{DelegationLaunch, ErrorCode, LaunchSelection};

use super::super::process::TransportWake;
use super::{ProviderDiagnostic, ProviderFailure, ProviderStage, ProviderVersion, failure};
use crate::{AttachmentPathResolver, providers::DEFAULT_PROVIDER_FRAME_BYTES};

pub(super) const DEFAULT_EVENT_CAPACITY: usize = 256;
pub(super) const DEFAULT_INPUT_BYTES: usize = 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct CodexConfig {
    pub(super) executable: OsString,
    pub(super) working_directory: PathBuf,
    pub(super) executable_version: ProviderVersion,
    pub(super) attachment_paths: Arc<dyn AttachmentPathResolver>,
    pub(super) maximum_frame_bytes: usize,
    pub(super) maximum_input_bytes: usize,
    pub(super) event_capacity: usize,
    pub(super) request_timeout: Duration,
    pub(super) shutdown_timeout: Duration,
    pub(super) wake: Arc<TransportWake>,
    /// The delegated launch, if this is one. Rendered into argv by
    /// `launch.rs`; a manual managed profile carries `None` and launches
    /// exactly as it did before delegation existed.
    pub(super) delegation: Option<DelegationLaunch>,
    /// What this launch chose by hand (protocol v16): a model the thread
    /// starts on, and an effort every turn carries.
    pub(super) selection: Option<LaunchSelection>,
    /// Stdio MCP servers rendered as `mcp_servers.<name>` overrides, beside
    /// any delegated endpoint.
    pub(super) mcp_servers: Vec<crate::providers::McpServerLaunch>,
}

impl CodexConfig {
    pub fn new(
        executable: impl Into<OsString>,
        working_directory: impl Into<PathBuf>,
        executable_version: ProviderVersion,
        attachment_paths: Arc<dyn AttachmentPathResolver>,
    ) -> Result<Self, ProviderFailure> {
        let working_directory = working_directory.into();
        if !working_directory.is_absolute() {
            return Err(failure(
                ErrorCode::InvalidRequest,
                ProviderStage::Spawn,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        Ok(Self {
            executable: executable.into(),
            working_directory,
            executable_version,
            attachment_paths,
            maximum_frame_bytes: DEFAULT_PROVIDER_FRAME_BYTES,
            maximum_input_bytes: DEFAULT_INPUT_BYTES,
            event_capacity: DEFAULT_EVENT_CAPACITY,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            wake: TransportWake::provider_default(),
            delegation: None,
            selection: None,
            mcp_servers: Vec::new(),
        })
    }

    /// Make this a delegated launch: the endpoint and allowlist are
    /// rendered privately into the app-server's configuration overrides.
    #[must_use]
    pub fn with_delegation(mut self, delegation: Option<DelegationLaunch>) -> Self {
        self.delegation = delegation;
        self
    }

    #[must_use]
    pub fn with_selection(mut self, selection: Option<LaunchSelection>) -> Self {
        self.selection = selection;
        self
    }

    #[must_use]
    pub fn with_mcp_servers(mut self, servers: Vec<crate::providers::McpServerLaunch>) -> Self {
        self.mcp_servers = servers;
        self
    }

    pub fn with_limits(
        mut self,
        maximum_frame_bytes: usize,
        maximum_input_bytes: usize,
        event_capacity: usize,
        request_timeout: Duration,
        shutdown_timeout: Duration,
    ) -> Result<Self, ProviderFailure> {
        if maximum_frame_bytes == 0
            || maximum_input_bytes == 0
            || event_capacity == 0
            || request_timeout.is_zero()
            || shutdown_timeout.is_zero()
        {
            return Err(failure(
                ErrorCode::InvalidRequest,
                ProviderStage::Initialize,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        self.maximum_frame_bytes = maximum_frame_bytes;
        self.maximum_input_bytes = maximum_input_bytes;
        self.event_capacity = event_capacity;
        self.request_timeout = request_timeout;
        self.shutdown_timeout = shutdown_timeout;
        Ok(self)
    }
}
