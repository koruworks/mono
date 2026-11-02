// SPDX-License-Identifier: MPL-2.0

use std::ffi::OsString;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use koru_session_protocol::{ErrorCode, ProviderKind, SessionId};

use super::config::{DEFAULT_EVENT_CAPACITY, DEFAULT_INPUT_BYTES};
use super::{CodexConfig, CodexControl, SUPPORTED_CODEX_VERSIONS, failure, provider_kind};
use crate::AttachmentPathResolver;
use crate::providers::{
    AgentCapabilities, AgentControl, AgentStart, CompatibilityProbe, ManagedProvider,
    ManagedProviderFactory, ManagedProviderRequest, ProbeCommand, ProbeLimits, ProviderDiagnostic,
    ProviderFailure, ProviderStage, ProviderVersion, ProviderWake, run_probe_command,
};

#[derive(Clone)]
pub struct CodexCompatibilityProbe {
    executable: OsString,
    working_directory: PathBuf,
    attachment_paths: Arc<dyn AttachmentPathResolver>,
}

impl CodexCompatibilityProbe {
    pub fn new(
        executable: impl Into<OsString>,
        working_directory: impl Into<PathBuf>,
        attachment_paths: Arc<dyn AttachmentPathResolver>,
    ) -> Self {
        Self {
            executable: executable.into(),
            working_directory: working_directory.into(),
            attachment_paths,
        }
    }

    pub(super) fn version(&self, limits: ProbeLimits) -> Result<ProviderVersion, ProviderFailure> {
        let output = run_probe_command(
            ProbeCommand::new(self.executable.clone()).arg("--version"),
            limits,
        )?;
        if !output.success() {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::InvalidVersion,
            ));
        }
        let output = std::str::from_utf8(output.stdout()).map_err(|_| {
            failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::InvalidVersion,
            )
        })?;
        let version = output.split_ascii_whitespace().last().ok_or_else(|| {
            failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::InvalidVersion,
            )
        })?;
        ProviderVersion::from_str(version)
    }
}

/// Root-owned production factory. It resolves no paths and performs the exact
/// reviewed version gate before returning one unstarted session control.
#[derive(Clone)]
pub struct CodexFactory {
    attachment_paths: Arc<dyn AttachmentPathResolver>,
    limits: ProbeLimits,
}

impl CodexFactory {
    pub fn new(attachment_paths: Arc<dyn AttachmentPathResolver>) -> Self {
        Self {
            attachment_paths,
            limits: ProbeLimits::default(),
        }
    }
}

impl ManagedProviderFactory for CodexFactory {
    fn supports_model_selection(&self) -> bool {
        true
    }

    /// The app-server's `model/list`, from a process started for the
    /// question and shut down after it.
    fn discover_models(
        &self,
        executable: OsString,
        working_directory: PathBuf,
    ) -> Result<koru_session_protocol::ProviderModels, ProviderFailure> {
        let executable = crate::providers::resolve_probe_executable(&executable)?;
        let probe = CodexCompatibilityProbe::new(
            executable.clone(),
            working_directory.clone(),
            Arc::clone(&self.attachment_paths),
        );
        let version = probe.version(self.limits)?;
        if !SUPPORTED_CODEX_VERSIONS.contains(&version.as_str()) {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            ));
        }
        let config = CodexConfig::new(
            executable,
            working_directory,
            version,
            Arc::clone(&self.attachment_paths),
        )?;
        CodexControl::new(config).discover_models()
    }

    /// Codex advertises no modes; Koru gives it three (koru-ai D7).
    fn fixed_modes(&self) -> Option<koru_session_protocol::AgentModes> {
        Some(super::modes::offered())
    }

    fn compatibility_probe(
        &self,
        executable: OsString,
        working_directory: PathBuf,
    ) -> Result<Box<dyn CompatibilityProbe>, ProviderFailure> {
        let executable = crate::providers::resolve_probe_executable(&executable)?;
        Ok(Box::new(CodexCompatibilityProbe::new(
            executable,
            working_directory,
            Arc::clone(&self.attachment_paths),
        )))
    }

    fn create(&self, request: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        // A chosen model rides to `thread/start`; a chosen effort has no
        // Codex route yet and is refused rather than ignored (protocol v16,
        // `LaunchSelection`).
        if request
            .selection
            .as_ref()
            .is_some_and(|selection| selection.reasoning.is_some())
        {
            return Err(failure(
                ErrorCode::CapabilityNotNegotiated,
                ProviderStage::Initialize,
                ProviderDiagnostic::HandshakeRejected,
            ));
        }
        if request
            .delegation
            .as_ref()
            .is_some_and(|launch| launch.model.is_some())
        {
            return Err(failure(
                ErrorCode::CapabilityNotNegotiated,
                ProviderStage::Initialize,
                ProviderDiagnostic::HandshakeRejected,
            ));
        }
        if request.provider != provider_kind() {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            ));
        }
        let probe = CodexCompatibilityProbe::new(
            request.executable.clone(),
            request.working_directory.clone(),
            Arc::clone(&self.attachment_paths),
        );
        let version = probe.version(self.limits)?;
        if !SUPPORTED_CODEX_VERSIONS.contains(&version.as_str()) {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            ));
        }
        let config = CodexConfig::new(
            request.executable,
            request.working_directory,
            version,
            Arc::clone(&self.attachment_paths),
        )?
        .with_delegation(request.delegation)
        .with_selection(request.selection)
        .with_mcp_servers(request.mcp_servers);
        let wake: Arc<dyn ProviderWake> = config.wake.clone();
        Ok(ManagedProvider {
            control: Box::new(CodexControl::new(config)),
            wake,
        })
    }
}

impl CompatibilityProbe for CodexCompatibilityProbe {
    fn provider(&self) -> ProviderKind {
        provider_kind()
    }

    fn raw_available(&self) -> bool {
        run_probe_command(
            ProbeCommand::new(self.executable.clone()).arg("--version"),
            ProbeLimits::default(),
        )
        .is_ok_and(|output| output.success())
    }

    fn probe(&self, limits: ProbeLimits) -> Result<AgentCapabilities, ProviderFailure> {
        let version = self.version(limits)?;
        if !SUPPORTED_CODEX_VERSIONS.contains(&version.as_str()) {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            ));
        }
        let config = CodexConfig::new(
            self.executable.clone(),
            self.working_directory.clone(),
            version,
            Arc::clone(&self.attachment_paths),
        )?
        .with_limits(
            limits.maximum_output_bytes.max(1),
            DEFAULT_INPUT_BYTES,
            DEFAULT_EVENT_CAPACITY,
            limits.timeout,
            limits.timeout,
        )?;
        let mut control = CodexControl::new(config);
        let capabilities = control.start(AgentStart::fresh(SessionId::mint(), None))?;
        control.shutdown()?;
        Ok(capabilities)
    }
}
